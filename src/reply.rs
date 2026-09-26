//! Every reply the plugin sends is built here, so the shape the host parses
//! has one author.

use serde_json::{Value, json};

/// One piece of evidence in a failure dump: inline `content`, or a `path` to
/// a file under the dispatch's `artifacts_dir` when the text is too big to
/// render.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub title: String,
    pub content: Option<String>,
    pub path: Option<String>,
}

impl Diagnostic {
    pub fn text(title: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            content: Some(content.into()),
            path: None,
        }
    }

    pub fn file(title: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            content: None,
            path: Some(path.into()),
        }
    }
}

/// The envelope shared by validate / init / drop / reset / probe.
pub fn ok() -> String {
    r#"{"ok":true}"#.to_string()
}

pub fn err(error: impl Into<String>) -> String {
    json!({"ok": false, "error": error.into()}).to_string()
}

pub fn passed() -> String {
    r#"{"status":"passed"}"#.to_string()
}

pub fn passed_with(vars: Value) -> String {
    json!({"status": "passed", "vars": vars}).to_string()
}

/// One fresh observation says the condition is not met yet. Only an
/// assertion may answer this; without an armed eventual assertion the host
/// treats it as a failure, so the message says what was observed.
pub fn not_yet(error: &str, diagnostics: &[Diagnostic]) -> String {
    failed("not_yet", error, diagnostics)
}

/// Retrying cannot help.
pub fn fatal(error: &str, diagnostics: &[Diagnostic]) -> String {
    failed("fatal", error, diagnostics)
}

fn failed(status: &str, error: &str, diagnostics: &[Diagnostic]) -> String {
    let diagnostics: Vec<Value> = diagnostics
        .iter()
        .map(|d| json!({"title": d.title, "kind": "text", "content": d.content, "path": d.path}))
        .collect();
    json!({"status": status, "error": error, "diagnostics": diagnostics}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passed_reply_carries_vars_and_no_diagnostics() {
        let v: Value =
            serde_json::from_str(&passed_with(json!({"exec_exit_code": "0"}))).expect("JSON");
        assert_eq!(v["status"], "passed");
        assert_eq!(v["vars"]["exec_exit_code"], "0");
        assert!(v.get("diagnostics").is_none());
    }

    #[test]
    fn a_failure_carries_inline_and_file_evidence() {
        let d = [
            Diagnostic::text("stdout", "hi"),
            Diagnostic::file("stderr", "/tmp/a/stderr.txt"),
        ];
        let v: Value = serde_json::from_str(&fatal("boom", &d)).expect("JSON");
        assert_eq!(v["status"], "fatal");
        assert_eq!(v["diagnostics"][0]["content"], "hi");
        assert!(v["diagnostics"][1]["content"].is_null());
        assert_eq!(v["diagnostics"][1]["path"], "/tmp/a/stderr.txt");
    }
}
