//! Spawning, capturing and stopping commands, the same on every platform.
//! What differs between them — how a whole process tree is held and ended,
//! how a shell takes a command string — lives in one adapter per OS,
//! `unix.rs` and `windows.rs`, chosen at compile time. Both expose the same
//! names; this module is their only caller.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::InstanceConfig;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as os;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as os;

/// `sh` on Unix, `cmd` on Windows.
pub use os::DEFAULT_SHELL;

/// How long a finished run waits for its pipes to drain.
const DRAIN: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, PartialEq)]
pub enum Program {
    /// A command string, handed to the shell the way that shell takes one.
    Shell { shell: String, line: String },
    /// No shell; `argv[0]` is looked up on `PATH`.
    Argv(Vec<String>),
}

/// Everything needed to start a command, and to describe it in evidence.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: Program,
    pub cwd: PathBuf,
    /// Instance keys, shown by name only: their values come from the config,
    /// which is where `${SECRET}` expansions live.
    pub instance_env: Vec<(String, String)>,
    /// Scenario keys, shown with values: the feature file wrote them.
    pub scenario_env: Vec<(String, String)>,
    pub stdin: Option<String>,
}

impl Spec {
    /// One line for a shell string, ready to paste into that shell's
    /// parent; one argv element per line otherwise.
    pub fn describe(&self) -> String {
        match &self.program {
            Program::Shell { shell, line } => os::describe_shell(shell, line),
            Program::Argv(argv) => argv.join("\n"),
        }
    }

    pub fn describe_env(&self) -> String {
        let instance = self
            .instance_env
            .iter()
            .map(|(k, _)| format!("{k} (from the instance config)"));
        let scenario = self.scenario_env.iter().map(|(k, v)| format!("{k}={v}"));
        instance.chain(scenario).collect::<Vec<_>>().join("\n")
    }

    fn command(&self) -> Command {
        let mut command = match &self.program {
            Program::Shell { shell, line } => os::shell_command(shell, line),
            Program::Argv(argv) => {
                let mut c = Command::new(&argv[0]);
                c.args(&argv[1..]);
                c
            }
        };
        command
            .current_dir(&self.cwd)
            .envs(
                self.instance_env
                    .iter()
                    .chain(&self.scenario_env)
                    .map(|(k, v)| (k, v)),
            )
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        os::prepare(&mut command);
        command
    }
}

/// `<shell> -c '<line>'` with the line single-quoted for a POSIX shell.
fn describe_posix(shell: &str, line: &str) -> String {
    format!("{shell} -c '{}'", line.replace('\'', r"'\''"))
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Exit {
    Code(i32),
    /// Unix: ended by a signal, and so without an exit code.
    #[cfg(unix)]
    Signal(i32),
    /// Windows: ended by this plugin's `TerminateJobObject`.
    #[cfg(windows)]
    Terminated,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // A negative code is a Windows NTSTATUS, which reads in hex:
            // 0xC000013A is what CTRL_BREAK ends a console program with.
            Exit::Code(c) if *c < 0 => write!(f, "exit code {c:#010X}"),
            Exit::Code(c) => write!(f, "exit code {c}"),
            #[cfg(unix)]
            Exit::Signal(s) => write!(f, "killed by {}", os::signal_name(*s)),
            #[cfg(windows)]
            Exit::Terminated => write!(f, "terminated by bddkit-exec"),
        }
    }
}

/// One captured stream. The reader keeps draining past the cap so the child
/// never blocks on a full pipe; what it drops is recorded, not hidden.
#[derive(Debug, Default)]
pub struct Capture {
    pub bytes: Vec<u8>,
    pub overflowed: bool,
}

impl Capture {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// The text so far, whether or not a reader panicked holding the lock.
pub fn text_of(capture: &Mutex<Capture>) -> String {
    capture.lock().unwrap_or_else(|e| e.into_inner()).text()
}

fn reader(
    mut pipe: impl Read + Send + 'static,
    cap: usize,
) -> (Arc<Mutex<Capture>>, JoinHandle<()>) {
    let capture = Arc::new(Mutex::new(Capture::default()));
    let sink = Arc::clone(&capture);
    // Detached: a member that escaped the tree can hold the pipe open past
    // every drain, and the thread then lives until the process exits.
    let handle = thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            let n = match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let mut c = sink.lock().unwrap_or_else(|e| e.into_inner());
            let room = cap.saturating_sub(c.bytes.len());
            c.bytes.extend_from_slice(&chunk[..n.min(room)]);
            if n > room {
                c.overflowed = true;
            }
        }
    });
    (capture, handle)
}

/// A started command: a one-shot run once it has finished, or a background
/// process for as long as the scenario keeps it.
pub struct Job {
    pub spec: Spec,
    child: Child,
    tree: os::Tree,
    pub stdout: Arc<Mutex<Capture>>,
    pub stderr: Arc<Mutex<Capture>>,
    readers: Vec<JoinHandle<()>>,
    exit: Option<Exit>,
    reaped: bool,
}

impl Job {
    pub fn spawn(spec: Spec, cap: usize) -> Result<Self, String> {
        let mut child = spec.command().spawn().map_err(|e| {
            format!(
                "cannot start `{}` in {}: {e}",
                spec.describe(),
                spec.cwd.display()
            )
        })?;
        let tree = match os::Tree::adopt(&mut child) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot contain `{}`: {e}", spec.describe()));
            }
        };
        if let (Some(mut pipe), Some(text)) = (child.stdin.take(), spec.stdin.clone()) {
            // A thread, because a child that reads nothing would otherwise
            // block this write forever once the pipe buffer fills; EPIPE from
            // a child that exits early is not our business.
            thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            });
        }
        let (stdout, out_reader) = reader(child.stdout.take().expect("piped stdout"), cap);
        let (stderr, err_reader) = reader(child.stderr.take().expect("piped stderr"), cap);
        Ok(Self {
            spec,
            child,
            tree,
            stdout,
            stderr,
            readers: vec![out_reader, err_reader],
            exit: None,
            reaped: false,
        })
    }

    /// Non-blocking: the exit, if the process has ended — read without
    /// releasing whatever keeps the tree addressable (see the adapters).
    pub fn poll(&mut self) -> Option<Exit> {
        if self.exit.is_none() && !self.reaped {
            self.exit = os::peek_exit(&mut self.child);
        }
        self.exit
    }

    /// Kills whatever is left of the tree, then reaps the process. The only
    /// place a child is reaped, and idempotent.
    fn finish(&mut self) {
        if self.reaped {
            return;
        }
        self.tree.kill();
        if let Ok(status) = self.child.wait() {
            self.exit.get_or_insert(os::exit_of(status));
        }
        self.reaped = true;
    }

    /// The streams can no longer change: the process is gone and both pipes
    /// reached EOF.
    pub fn finished(&mut self) -> bool {
        self.poll().is_some() && self.readers.iter().all(JoinHandle::is_finished)
    }

    /// Waits for both pipes to reach EOF, at most `limit`: a member that
    /// escaped the tree could hold one open forever.
    fn drain(&mut self, limit: Duration) {
        let deadline = Instant::now() + limit;
        while self.readers.iter().any(|r| !r.is_finished()) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }

    pub fn exit(&self) -> Option<Exit> {
        self.exit
    }
}

/// Whatever path drops a job — a reset, a panic between spawn and the end of
/// a run — its tree dies with it: nothing started may outlive the run.
impl Drop for Job {
    fn drop(&mut self) {
        self.finish();
    }
}

/// How a one-shot run ended when it did not end on its own.
pub struct TimedOut(pub Job);

/// Runs to completion. Whatever the command left behind in its tree is
/// killed once it exits: it would hold our pipes open, and nothing else
/// would ever stop it.
pub fn run(spec: Spec, cap: usize, timeout: Duration) -> Result<Result<Job, TimedOut>, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| format!("a timeout of {timeout:?} is out of range"))?;
    let mut job = Job::spawn(spec, cap)?;
    while job.poll().is_none() {
        if Instant::now() >= deadline {
            job.finish();
            job.drain(DRAIN);
            return Ok(Err(TimedOut(job)));
        }
        thread::sleep(POLL);
    }
    job.finish();
    job.drain(DRAIN);
    Ok(Ok(job))
}

/// A graceful request to every tree (SIGTERM on Unix, CTRL_BREAK on
/// Windows), one shared `grace` period, then a kill of every tree — whether
/// or not its first process already went, since a tree can outlive it.
/// Several jobs share one grace so a reset does not cost the grace once per
/// process; a zero grace skips the request, and no grace is spent when no
/// request could be delivered.
pub fn stop_all<'a>(jobs: impl IntoIterator<Item = &'a mut Job>, grace: Duration) {
    let mut jobs: Vec<&mut Job> = jobs.into_iter().collect();
    let mut asked = false;
    if !grace.is_zero() {
        for job in jobs.iter_mut() {
            if job.poll().is_none() {
                asked |= job.tree.interrupt();
            }
        }
    }
    let deadline = Instant::now() + grace;
    while asked && jobs.iter_mut().any(|j| j.poll().is_none()) && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    for job in jobs.iter_mut() {
        job.finish();
    }
    for job in jobs.iter_mut() {
        job.drain(DRAIN);
    }
}

/// `doctor --live`: the shell starts and exits cleanly, in `cwd` when that is
/// absolute (a relative one depends on a workspace only a run has).
pub fn probe(config: &InstanceConfig) -> Result<(), String> {
    let cwd = match &config.cwd {
        Some(dir) if PathBuf::from(dir).is_absolute() => {
            let path = PathBuf::from(dir);
            if !path.is_dir() {
                return Err(format!("cwd {dir} is not a directory"));
            }
            path
        }
        _ => std::env::temp_dir(),
    };
    let spec = Spec {
        program: Program::Shell {
            shell: config.shell.clone(),
            line: "exit 0".into(),
        },
        cwd,
        instance_env: config.env.clone(),
        scenario_env: Vec::new(),
        stdin: None,
    };
    let described = spec.describe();
    match run(spec, 64 * 1024, Duration::from_secs(10))? {
        Ok(job) if job.exit() == Some(Exit::Code(0)) => Ok(()),
        Ok(job) => Err(format!(
            "`{described}` ended with {}: {}",
            job.exit().map_or("no exit".into(), |e| e.to_string()),
            text_of(&job.stderr).trim()
        )),
        Err(_) => Err(format!("`{described}` did not finish within 10 seconds")),
    }
}

/// Command lines the tests run, in the default shell of each platform.
#[cfg(test)]
pub mod script {
    #[cfg(unix)]
    mod lines {
        pub const STREAMS: &str = "echo out; echo err >&2; exit 3";
        pub const STDIN_ENV: &str = "read line; echo \"$line-$GREETING\"";
        pub const SLEEP: &str = "sleep 30";
        pub const LEFTOVER: &str = "sleep 30 & echo done";
        pub const HANG_WITH_CHILD: &str = "sleep 30 & sleep 30; echo never";
        pub const PIPELINE: &str = "sleep 30 | cat";
        pub const EXIT_7: &str = "exit 7";
        pub const PRINT_DIGITS: &str = "printf 0123456789";
    }
    #[cfg(windows)]
    mod lines {
        pub const STREAMS: &str = "echo out& echo err 1>&2& exit /b 3";
        // `call` re-expands after `set /p` ran; plain %line% is expanded
        // when the whole line is parsed, before it has a value.
        pub const STDIN_ENV: &str = "set /p line=& call echo %line%-%GREETING%";
        pub const SLEEP: &str = "ping -n 30 127.0.0.1 >nul";
        pub const LEFTOVER: &str = "start /b ping -n 30 127.0.0.1 >nul& echo done";
        pub const HANG_WITH_CHILD: &str =
            "start /b ping -n 30 127.0.0.1 >nul& ping -n 30 127.0.0.1 >nul& echo never";
        pub const PIPELINE: &str = "ping -n 30 127.0.0.1 | findstr never";
        pub const EXIT_7: &str = "exit /b 7";
        pub const PRINT_DIGITS: &str = "echo 0123456789";
    }
    pub use lines::*;

    /// A spec running `line` in the platform's default shell.
    pub fn shell(line: &str) -> super::Spec {
        super::Spec {
            program: super::Program::Shell {
                shell: super::DEFAULT_SHELL.into(),
                line: line.into(),
            },
            cwd: std::env::temp_dir(),
            instance_env: vec![],
            scenario_env: vec![],
            stdin: None,
        }
    }

    /// cmd.exe ends its lines with `\r\n`; the assertions are about content.
    pub fn lf(text: String) -> String {
        text.replace("\r\n", "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::script::{self, lf, shell};
    use super::*;

    fn text(c: &Arc<Mutex<Capture>>) -> String {
        lf(text_of(c))
    }

    fn completed(spec: Spec, cap: usize) -> Job {
        run(spec, cap, Duration::from_secs(10))
            .expect("spawn")
            .unwrap_or_else(|_| panic!("timed out"))
    }

    #[test]
    fn a_run_captures_both_streams_and_the_exit_code() {
        let job = completed(shell(script::STREAMS), 1024);
        assert_eq!(job.exit(), Some(Exit::Code(3)));
        assert_eq!(text(&job.stdout), "out\n");
        assert_eq!(text(&job.stderr).trim_end(), "err");
    }

    #[test]
    fn stdin_and_env_reach_the_command() {
        let mut spec = shell(script::STDIN_ENV);
        spec.stdin = Some("hello\n".into());
        spec.scenario_env = vec![("GREETING".into(), "world".into())];
        assert_eq!(text(&completed(spec, 1024).stdout), "hello-world\n");
    }

    #[test]
    fn a_timeout_kills_the_whole_tree() {
        let started = Instant::now();
        let outcome = run(
            shell(script::HANG_WITH_CHILD),
            1024,
            Duration::from_millis(500),
        )
        .expect("spawn");
        let Err(TimedOut(job)) = outcome else {
            panic!("it finished")
        };
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the background child held the pipes"
        );
        #[cfg(unix)]
        assert_eq!(job.exit(), Some(Exit::Signal(libc::SIGKILL)));
        #[cfg(windows)]
        assert_eq!(job.exit(), Some(Exit::Terminated));
    }

    #[test]
    fn a_leftover_background_child_does_not_hold_a_run_open() {
        let started = Instant::now();
        let job = completed(shell(script::LEFTOVER), 1024);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(text(&job.stdout), "done\n");
    }

    #[test]
    fn output_past_the_cap_is_dropped_and_recorded() {
        let job = completed(shell(script::PRINT_DIGITS), 4);
        let c = job.stdout.lock().expect("capture");
        assert_eq!((c.text().as_str(), c.overflowed), ("0123", true));
    }

    #[test]
    fn stop_all_ends_a_pipeline_within_the_grace_period() {
        let mut job = Job::spawn(shell(script::PIPELINE), 1024).expect("spawn");
        let started = Instant::now();
        stop_all([&mut job], Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(job.exit().is_some());
        // SIGTERM is always deliverable; CTRL_BREAK needs a shared console,
        // so on Windows the tree may have been terminated instead.
        #[cfg(unix)]
        assert_eq!(job.exit(), Some(Exit::Signal(libc::SIGTERM)));
    }

    #[test]
    fn a_zero_grace_kills_without_asking() {
        let mut job = Job::spawn(shell(script::SLEEP), 1024).expect("spawn");
        let started = Instant::now();
        stop_all([&mut job], Duration::ZERO);
        assert!(started.elapsed() < Duration::from_secs(1));
        #[cfg(unix)]
        assert_eq!(job.exit(), Some(Exit::Signal(libc::SIGKILL)));
        #[cfg(windows)]
        assert_eq!(job.exit(), Some(Exit::Terminated));
    }

    /// A process that ignores the request is killed when its grace runs
    /// out, not before and not much after.
    #[cfg(unix)]
    #[test]
    fn a_process_that_ignores_sigterm_is_killed_when_its_grace_runs_out() {
        let spec = shell("trap '' TERM; echo ready; while :; do sleep 0.05; done");
        let mut job = Job::spawn(spec, 1024).expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !text(&job.stdout).contains("ready") && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        let started = Instant::now();
        stop_all([&mut job], Duration::from_millis(600));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(600), "killed early: {took:?}");
        assert!(took < Duration::from_secs(3), "killed late: {took:?}");
        assert_eq!(job.exit(), Some(Exit::Signal(libc::SIGKILL)));
    }

    #[test]
    fn poll_sees_the_exit_before_the_process_is_reaped() {
        let mut job = Job::spawn(shell(script::EXIT_7), 1024).expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(5);
        while job.poll().is_none() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        assert_eq!(job.poll(), Some(Exit::Code(7)));
        assert!(!job.reaped);
        job.finish();
        assert_eq!(job.exit(), Some(Exit::Code(7)), "finish keeps the exit");
    }

    /// The zombie is what keeps the group id from being reused.
    #[cfg(unix)]
    #[test]
    fn on_unix_a_polled_leader_stays_a_zombie_until_finish() {
        let mut job = Job::spawn(shell(script::EXIT_7), 1024).expect("spawn");
        let pid = job.child.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        while job.poll().is_none() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        assert!(os::alive(pid), "a zombie still answers kill(pid, 0)");
        job.finish();
        assert!(!os::alive(pid), "finish reaps it");
    }

    #[test]
    fn dropping_a_job_kills_its_tree() {
        let job = Job::spawn(shell(script::SLEEP), 1024).expect("spawn");
        let pid = job.child.id();
        drop(job);
        assert!(!os::alive(pid));
    }

    #[test]
    fn a_missing_program_is_an_error_naming_it() {
        let spec = Spec {
            program: Program::Argv(vec!["no-such-binary-bddkit".into()]),
            ..shell("")
        };
        let e = Job::spawn(spec, 1024).err().expect("refused");
        assert!(e.contains("no-such-binary-bddkit"), "{e}");
    }

    #[test]
    fn a_posix_shell_command_is_described_ready_to_paste() {
        let spec = Spec {
            program: Program::Shell {
                shell: "sh".into(),
                line: "echo 'a b'; exit 1".into(),
            },
            ..shell("")
        };
        assert_eq!(spec.describe(), r"sh -c 'echo '\''a b'\''; exit 1'");
    }

    #[test]
    fn exits_render_codes_in_decimal_and_ntstatus_in_hex() {
        assert_eq!(Exit::Code(2).to_string(), "exit code 2");
        assert_eq!(
            Exit::Code(0xC000_013A_u32 as i32).to_string(),
            "exit code 0xC000013A"
        );
        #[cfg(unix)]
        assert_eq!(Exit::Signal(9).to_string(), "killed by SIGKILL (9)");
    }
}
