//! Windows: the process tree is a Job Object. The child starts suspended, is
//! put in its own job and only then resumed, so nothing it starts can escape
//! the job; `TerminateJobObject` ends the whole tree. The job is created with
//! `KILL_ON_JOB_CLOSE`, so the tree also dies if bddkit itself does. A child
//! also leads its own console process group, which is what `CTRL_BREAK` is
//! addressed to on a graceful stop.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

use super::Exit;

pub const DEFAULT_SHELL: &str = "cmd";

/// The exit code `TerminateJobObject` stamps on what it kills, so that an
/// exit this plugin caused is told from one the program chose.
const TERMINATED: u32 = 0xBDD0_0001;

/// How a shell takes a command string, told by its file name.
enum Dialect {
    /// `cmd /S /C "<line>"`, passed raw: cmd.exe does its own parsing, and
    /// the MSVC argument quoting Rust applies would corrupt it.
    Cmd,
    /// `powershell` / `pwsh`: `-NoProfile -Command <line>`.
    PowerShell,
    /// Anything else — Git Bash, MSYS2, Cygwin: `-c <line>`.
    Posix,
}

fn dialect(shell: &str) -> Dialect {
    let stem = Path::new(shell)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match stem.as_str() {
        "cmd" => Dialect::Cmd,
        "powershell" | "pwsh" => Dialect::PowerShell,
        _ => Dialect::Posix,
    }
}

pub fn shell_command(shell: &str, line: &str) -> Command {
    let mut command = Command::new(shell);
    match dialect(shell) {
        Dialect::Cmd => command.raw_arg(format!("/S /C \"{line}\"")),
        Dialect::PowerShell => command.args(["-NoProfile", "-Command", line]),
        Dialect::Posix => command.args(["-c", line]),
    };
    command
}

pub fn describe_shell(shell: &str, line: &str) -> String {
    match dialect(shell) {
        Dialect::Cmd => format!("{shell} /S /C \"{line}\""),
        Dialect::PowerShell => format!("{shell} -NoProfile -Command {line}"),
        Dialect::Posix => super::describe_posix(shell, line),
    }
}

pub fn prepare(command: &mut Command) {
    command.creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP);
}

/// The job the child and all its descendants live in.
pub struct Tree {
    job: HANDLE,
    /// The child's pid, which is also its console process group id.
    group: u32,
}

// SAFETY: a job handle is a kernel object handle, usable from any thread;
// the plugin only ever touches it from one thread at a time anyway.
unsafe impl Send for Tree {}

impl Tree {
    /// Puts the suspended child in a fresh job, then lets it run.
    pub fn adopt(child: &mut Child) -> io::Result<Self> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let tree = Self {
            job,
            group: child.id(),
        };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0
            || unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        resume(child.id())?;
        Ok(tree)
    }

    /// CTRL_BREAK to the child's console process group. Fails — and the
    /// caller then skips the grace period — when bddkit has no console to
    /// share, as under a service manager.
    pub fn interrupt(&self) -> bool {
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.group) != 0 }
    }

    pub fn kill(&self) {
        unsafe { TerminateJobObject(self.job, TERMINATED) };
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE: closing the last handle ends whatever is left.
        unsafe { CloseHandle(self.job) };
    }
}

/// Resumes every thread of a process created suspended. A fresh process has
/// exactly one, but the snapshot is the documented way to find it without
/// the thread handle `std::process` does not expose.
fn resume(pid: u32) -> io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut resumed = 0;
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                if unsafe { ResumeThread(thread) } != u32::MAX {
                    resumed += 1;
                }
                unsafe { CloseHandle(thread) };
            }
        }
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    if resumed == 0 {
        return Err(io::Error::other("the new process has no thread to resume"));
    }
    Ok(())
}

/// A Windows process handle stays valid until `Child` is dropped, and the
/// tree is addressed by the job handle rather than a pid, so there is no
/// reuse to guard against: `try_wait` is enough.
pub fn peek_exit(child: &mut Child) -> Option<Exit> {
    child.try_wait().ok().flatten().map(exit_of)
}

pub fn exit_of(status: ExitStatus) -> Exit {
    match status.code() {
        Some(code) if code as u32 == TERMINATED => Exit::Terminated,
        Some(code) => Exit::Code(code),
        None => Exit::Terminated,
    }
}

/// For tests: whether a pid names a process that has not exited.
#[cfg(test)]
pub fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::STILL_ACTIVE;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let mut code = 0u32;
    let running =
        unsafe { GetExitCodeProcess(process, &mut code) } != 0 && code == STILL_ACTIVE as u32;
    unsafe { CloseHandle(process) };
    running
}
