//! One instance per feature file (`per_worker`): the instance config plus
//! everything a scenario accumulates, which `reset` throws away.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::InstanceConfig;
use crate::exec::{self, Job, Program, Spec};

/// Per-scenario preparation, set with `the command … is` steps.
#[derive(Default)]
pub struct Context {
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    /// For the next one-shot command only.
    pub stdin: Option<String>,
    pub timeout: Option<Duration>,
}

pub struct Instance {
    pub config: InstanceConfig,
    pub context: Context,
    pub last: Option<Job>,
    /// In start order, names unique within the scenario.
    pub processes: Vec<(String, Job)>,
}

impl Instance {
    pub fn new(config: InstanceConfig) -> Self {
        Self {
            config,
            context: Context::default(),
            last: None,
            processes: Vec::new(),
        }
    }

    /// Kills every process the scenario started and forgets the rest.
    pub fn reset(&mut self) {
        let grace = self.config.stop_grace;
        exec::stop_all(self.processes.iter_mut().map(|(_, job)| job), grace);
        self.processes.clear();
        self.last = None;
        self.context = Context::default();
    }

    /// The instance `cwd` against the workspace, then the scenario's own
    /// against that. An absolute path at either level wins.
    fn cwd(&self, workspace: &Path) -> PathBuf {
        let base = match &self.config.cwd {
            Some(dir) => workspace.join(dir),
            None => workspace.to_path_buf(),
        };
        match &self.context.cwd {
            Some(dir) => base.join(dir),
            None => base,
        }
    }

    /// Takes the context's stdin: it belongs to one command.
    pub fn spec(&mut self, program: Program, workspace: &Path, with_stdin: bool) -> Spec {
        Spec {
            program,
            cwd: self.cwd(workspace),
            instance_env: self.config.env.clone(),
            scenario_env: self.context.env.clone(),
            stdin: if with_stdin {
                self.context.stdin.take()
            } else {
                None
            },
        }
    }

    pub fn timeout(&self) -> Duration {
        self.context.timeout.unwrap_or(self.config.timeout)
    }

    pub fn process(&mut self, name: &str) -> Option<&mut Job> {
        self.processes
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, job)| job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(cwd: Option<&str>) -> Instance {
        let mut config = InstanceConfig::parse(&serde_json::json!({})).expect("defaults");
        config.cwd = cwd.map(str::to_string);
        Instance::new(config)
    }

    #[test]
    fn cwd_defaults_to_the_workspace_and_nests_relative_paths() {
        let ws = Path::new("/ws/000001");
        assert_eq!(instance(None).cwd(ws), PathBuf::from("/ws/000001"));
        let mut i = instance(Some("app"));
        assert_eq!(i.cwd(ws), PathBuf::from("/ws/000001/app"));
        i.context.cwd = Some("bin".into());
        assert_eq!(i.cwd(ws), PathBuf::from("/ws/000001/app/bin"));
        i.context.cwd = Some("/opt".into());
        assert_eq!(i.cwd(ws), PathBuf::from("/opt"));
    }

    #[test]
    fn stdin_is_consumed_by_one_command() {
        let mut i = instance(None);
        i.context.stdin = Some("x".into());
        let shell = || Program::Shell {
            shell: String::new(),
            line: "cat".into(),
        };
        assert_eq!(
            i.spec(shell(), Path::new("/"), true).stdin.as_deref(),
            Some("x")
        );
        assert_eq!(i.spec(shell(), Path::new("/"), true).stdin, None);
    }

    #[test]
    fn reset_kills_processes_and_clears_the_scenario() {
        let mut i = instance(None);
        i.context.env.push(("A".into(), "1".into()));
        let spec = i.spec(
            Program::Shell {
                shell: crate::exec::DEFAULT_SHELL.into(),
                line: crate::exec::script::SLEEP.into(),
            },
            &std::env::temp_dir(),
            false,
        );
        i.processes
            .push(("sleeper".into(), Job::spawn(spec, 1024).expect("spawn")));
        i.reset();
        assert!(i.processes.is_empty() && i.context.env.is_empty() && i.last.is_none());
    }
}
