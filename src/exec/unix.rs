//! Linux and macOS: the process tree is a process group. Every child leads
//! its own group and every signal goes to the group, since signalling only
//! the `sh` in `sh -c "tail -f x | grep y"` would orphan the `tail`.

use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus};

use super::Exit;

pub const DEFAULT_SHELL: &str = "sh";

pub fn shell_command(shell: &str, line: &str) -> Command {
    let mut command = Command::new(shell);
    command.arg("-c").arg(line);
    command
}

pub fn describe_shell(shell: &str, line: &str) -> String {
    super::describe_posix(shell, line)
}

pub fn prepare(command: &mut Command) {
    command.process_group(0);
}

/// A process group, addressed by its leader's pid. Valid for as long as the
/// leader is not reaped: `peek_exit` leaves it a zombie so that the number
/// cannot be reused, and `Job::finish` reaps only after the last signal.
pub struct Tree {
    pgid: libc::pid_t,
}

impl Tree {
    pub fn adopt(child: &mut Child) -> io::Result<Self> {
        Ok(Self {
            pgid: child.id() as libc::pid_t,
        })
    }

    /// SIGTERM to the group; always deliverable.
    pub fn interrupt(&self) -> bool {
        self.signal(libc::SIGTERM);
        true
    }

    pub fn kill(&self) {
        self.signal(libc::SIGKILL);
    }

    fn signal(&self, signal: i32) {
        unsafe { libc::kill(-self.pgid, signal) };
    }
}

/// `waitid(WNOWAIT)`: reads the leader's exit and leaves it a zombie.
pub fn peek_exit(child: &mut Child) -> Option<Exit> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    if unsafe { libc::waitid(libc::P_PID, child.id() as libc::id_t, &mut info, flags) } != 0 {
        return None;
    }
    // Linux exposes the union members through methods, macOS as fields.
    #[cfg(target_os = "linux")]
    let (exited, status) = unsafe { (info.si_pid(), info.si_status()) };
    #[cfg(not(target_os = "linux"))]
    let (exited, status) = (info.si_pid, info.si_status);
    let exit = if info.si_code == libc::CLD_EXITED {
        Exit::Code(status)
    } else {
        Exit::Signal(status)
    };
    // WNOHANG with nothing to report leaves si_pid zero.
    (exited != 0).then_some(exit)
}

pub fn exit_of(status: ExitStatus) -> Exit {
    match status.code() {
        Some(code) => Exit::Code(code),
        None => Exit::Signal(status.signal().unwrap_or(0)),
    }
}

pub fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGABRT => "SIGABRT",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGTERM => "SIGTERM",
        _ => return format!("signal {signal}"),
    };
    format!("{name} ({signal})")
}

/// For tests: whether a pid names a live (or zombie) process.
#[cfg(test)]
pub fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
