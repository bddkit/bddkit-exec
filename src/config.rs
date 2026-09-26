//! `resources.exec.<name>`: where and how commands run. Every key has a
//! default, which is what lets the manifest declare an empty
//! `implicit_instance`.

use std::time::Duration;

use serde_json::{Value, json};

/// A day: long enough for any command a test should wait on, and far from
/// where `Duration` and `Instant` arithmetic overflow.
const MAX_TIMEOUT_SECS: f64 = 86_400.0;

/// The one rule for a timeout, from the config or from a step.
pub fn timeout(seconds: f64) -> Result<Duration, String> {
    if seconds.is_finite() && seconds > 0.0 && seconds <= MAX_TIMEOUT_SECS {
        Ok(Duration::from_secs_f64(seconds))
    } else {
        Err(format!(
            "a timeout must be a positive number of seconds, at most {MAX_TIMEOUT_SECS}; got {seconds}"
        ))
    }
}

/// How long a stopped process gets between the graceful request and the
/// kill. Zero skips the request: the tree is killed at once.
pub fn grace(seconds: f64) -> Result<Duration, String> {
    if (0.0..=MAX_TIMEOUT_SECS).contains(&seconds) {
        Ok(Duration::from_secs_f64(seconds))
    } else {
        Err(format!(
            "a grace period must be a number of seconds from 0 to {MAX_TIMEOUT_SECS}; got {seconds}"
        ))
    }
}

/// `(name, type, description, example)` — the one list both `fields` in the
/// manifest and the unknown-key check in `parse` are derived from.
const FIELDS: &[(&str, &str, &str, &str)] = &[
    (
        "cwd",
        "string",
        "working directory of every command; relative to the feature file's workspace, which is also the default",
        "/srv/app",
    ),
    (
        "shell",
        "string",
        "shell a command string runs in: `-c <string>`, or `/S /C` for cmd and `-Command` for PowerShell on Windows; default sh, cmd on Windows",
        "bash",
    ),
    (
        "timeout_secs",
        "number",
        "how long a one-shot command may run before it is killed and the step fails; default 30",
        "30",
    ),
    (
        "stop_grace_secs",
        "number",
        "how long a stopped process gets to shut down after the graceful request (SIGTERM; CTRL_BREAK on Windows) before it is killed; 0 kills at once; default 2",
        "10",
    ),
    (
        "max_output_bytes",
        "number",
        "cap on each captured stream; reaching it fails the next assertion on that stream; default 8388608",
        "8388608",
    ),
    (
        "env",
        "nonscalar",
        "environment variables set on top of bddkit's own, as a map",
        "",
    ),
];

pub fn fields_json() -> Value {
    Value::Array(
        FIELDS
            .iter()
            .map(|(name, kind, description, example)| {
                let mut field = json!({"name": name, "type": kind, "description": description});
                if !example.is_empty() {
                    field["example"] = json!(example);
                }
                field
            })
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceConfig {
    /// `None` means the workspace dir, known only per dispatch.
    pub cwd: Option<String>,
    pub shell: String,
    pub timeout: Duration,
    pub stop_grace: Duration,
    pub max_output_bytes: usize,
    /// In declaration order; applied on top of the inherited environment.
    pub env: Vec<(String, String)>,
}

impl InstanceConfig {
    pub fn parse(config: &Value) -> Result<Self, String> {
        let empty = serde_json::Map::new();
        let map = match config {
            Value::Object(m) => m,
            Value::Null => &empty,
            _ => return Err("the instance body must be a mapping".into()),
        };
        if let Some(key) = map
            .keys()
            .find(|k| !FIELDS.iter().any(|(name, ..)| name == k))
        {
            let known: Vec<&str> = FIELDS.iter().map(|(name, ..)| *name).collect();
            return Err(format!(
                "unknown key {key:?}; accepted: {}",
                known.join(", ")
            ));
        }
        let string = |key: &str| -> Result<Option<String>, String> {
            match map.get(key) {
                None => Ok(None),
                Some(Value::String(s)) if !s.is_empty() && !s.contains('\0') => Ok(Some(s.clone())),
                Some(_) => Err(format!("{key:?} must be a non-empty string")),
            }
        };
        let timeout = match map.get("timeout_secs") {
            None => Duration::from_secs(30),
            Some(v) => v
                .as_f64()
                .ok_or_else(|| "\"timeout_secs\" must be a number".to_string())
                .and_then(timeout)
                .map_err(|e| format!("\"timeout_secs\": {e}"))?,
        };
        let stop_grace = match map.get("stop_grace_secs") {
            None => Duration::from_secs(2),
            Some(v) => v
                .as_f64()
                .ok_or_else(|| "\"stop_grace_secs\" must be a number".to_string())
                .and_then(grace)
                .map_err(|e| format!("\"stop_grace_secs\": {e}"))?,
        };
        let max_output_bytes = match map.get("max_output_bytes") {
            None => 8 * 1024 * 1024,
            Some(v) => v
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or("\"max_output_bytes\" must be a positive integer")?,
        };
        Ok(Self {
            cwd: string("cwd")?,
            shell: string("shell")?.unwrap_or_else(|| crate::exec::DEFAULT_SHELL.to_string()),
            timeout,
            stop_grace,
            max_output_bytes: usize::try_from(max_output_bytes).unwrap_or(usize::MAX),
            env: parse_env(map.get("env"))?,
        })
    }
}

/// A YAML `PORT: 8080` arrives as a JSON number; an environment value is text
/// either way, so a scalar is taken in its own spelling. A nested value is
/// refused rather than flattened into something nobody wrote.
fn parse_env(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let map = value
        .as_object()
        .ok_or("\"env\" must be a mapping of names to values")?;
    map.iter()
        .map(|(name, v)| {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => {
                    return Err(format!(
                        "env.{name} must be a string, a number or a boolean"
                    ));
                }
            };
            if name.is_empty() || name.contains(['=', '\0']) || text.contains('\0') {
                return Err(format!("env.{name:?} is not a valid environment variable"));
            }
            Ok((name.clone(), text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_body_takes_every_default() {
        let c = InstanceConfig::parse(&json!({})).expect("defaults");
        assert_eq!(
            c,
            InstanceConfig {
                cwd: None,
                shell: crate::exec::DEFAULT_SHELL.into(),
                timeout: Duration::from_secs(30),
                stop_grace: Duration::from_secs(2),
                max_output_bytes: 8 * 1024 * 1024,
                env: vec![],
            }
        );
    }

    #[test]
    fn every_key_is_read() {
        let c = InstanceConfig::parse(&json!({
            "cwd": "/srv/app", "shell": "bash", "timeout_secs": 0.5, "stop_grace_secs": 15,
            "max_output_bytes": 10, "env": {"APP_ENV": "test", "PORT": 8080, "DEBUG": true},
        }))
        .expect("valid");
        assert_eq!(c.cwd.as_deref(), Some("/srv/app"));
        assert_eq!(c.shell, "bash");
        assert_eq!(c.timeout, Duration::from_millis(500));
        assert_eq!(c.stop_grace, Duration::from_secs(15));
        assert_eq!(c.max_output_bytes, 10);
        assert!(c.env.contains(&("PORT".into(), "8080".into())));
        assert!(c.env.contains(&("DEBUG".into(), "true".into())));
    }

    #[test]
    fn an_unknown_key_is_refused_naming_it() {
        let e = InstanceConfig::parse(&json!({"runner": "ssh"})).unwrap_err();
        assert!(e.contains("\"runner\""), "{e}");
    }

    #[test]
    fn malformed_values_are_refused() {
        for body in [
            json!({"timeout_secs": 0}),
            json!({"timeout_secs": 1e20}),
            json!({"stop_grace_secs": -1}),
            json!({"stop_grace_secs": 1e20}),
            json!({"stop_grace_secs": "2"}),
            json!({"timeout_secs": "30"}),
            json!({"max_output_bytes": -1}),
            json!({"shell": ""}),
            json!({"env": ["A=1"]}),
            json!({"env": {"A": {"nested": 1}}}),
            json!({"env": {"A=B": "x"}}),
        ] {
            assert!(InstanceConfig::parse(&body).is_err(), "{body} was accepted");
        }
    }

    #[test]
    fn a_zero_grace_is_allowed_and_means_kill_at_once() {
        let c = InstanceConfig::parse(&json!({"stop_grace_secs": 0})).expect("valid");
        assert_eq!(c.stop_grace, Duration::ZERO);
    }

    #[test]
    fn a_nonscalar_field_carries_no_string_example() {
        let fields = fields_json();
        let env = fields
            .as_array()
            .expect("array")
            .iter()
            .find(|f| f["name"] == "env")
            .expect("env");
        assert_eq!(
            (env["name"].as_str(), env.get("example")),
            (Some("env"), None)
        );
    }
}
