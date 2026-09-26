//! The end-to-end suite: the real `bddkit` binary and this crate's `cdylib`.
//! Nothing else is external — the commands under test are `sh`, `sleep`,
//! `tail` and friends.
//!
//! `bddkit` is found through `BDDKIT_BIN`, then `PATH`, then a sibling
//! `../bddkit` build; without one every test skips itself, saying why.
//! `examples/` is what runs first: the example and the proof are one thing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

fn bddkit_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BDDKIT_BIN") {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("bddkit"))
        .find(|p| p.is_file());
    on_path.or_else(|| {
        [
            root().join("../bddkit/target/release/bddkit"),
            root().join("../bddkit/target/debug/bddkit"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    })
}

macro_rules! require_bddkit {
    () => {
        match bddkit_bin() {
            Some(bin) => bin,
            None => {
                eprintln!("SKIP: no bddkit binary — set BDDKIT_BIN, put bddkit on PATH, or build ../bddkit");
                return;
            }
        }
    };
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Builds the plugin once and writes a lock file for it into a directory
/// handed to `--bddkit-dir`, so the committed `examples/.bddkit` (which
/// points at a release build) is never consulted.
fn lock_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let out = Command::new(env!("CARGO"))
            .arg("build")
            .current_dir(root())
            .output()
            .expect("cargo build");
        assert!(
            out.status.success(),
            "plugin build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let library = root().join("target/debug").join(format!(
            "{}bddkit_exec{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        let dir = std::env::temp_dir().join(format!("bddkit-exec-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("plugins.yaml"),
            format!("plugin:\n  - name: exec\n    path: {}\n", library.display()),
        )
        .expect("lock");
        dir
    })
}

/// `bddkit <args> --bddkit-dir <lock>` from the repository root, where the
/// configs' `paths` resolve.
fn bddkit(bin: &Path, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(bin);
    command
        .args(args)
        .arg("--bddkit-dir")
        .arg(lock_dir())
        .current_dir(root());
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("bddkit")
}

fn show(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_examples_pass() {
    let bin = require_bddkit!();
    let out = bddkit(&bin, &["run", "--config", "examples/exec.yaml"], &[]);
    assert!(out.status.success(), "{}", show(&out));
}

#[test]
fn a_suite_with_no_exec_section_runs_on_the_implicit_instance() {
    let bin = require_bddkit!();
    let out = bddkit(&bin, &["run", "--config", "examples/zero-config.yaml"], &[]);
    assert!(out.status.success(), "{}", show(&out));
}

#[test]
fn every_failure_says_what_happened_and_carries_the_evidence() {
    let bin = require_bddkit!();
    let out = bddkit(
        &bin,
        &[
            "run",
            "--config",
            "tests/exec.yaml",
            "tests/features/failing.feature",
        ],
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "{}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for expected in [
        // The shell's own path differs by platform; the line does not.
        "-c 'echo started; sleep 5'` did not finish within 0.3 seconds; its process tree was killed",
        #[cfg(unix)]
        "killed by SIGKILL (9)",
        #[cfg(windows)]
        "terminated by bddkit-exec",
        "the command output reached max_output_bytes (16 bytes)",
        "<<null>> cannot be part of a command",
        "did not pass within 1s",
        "the \"quiet\" process output does not contain \"never printed\"",
        "the command exit code is 4, expected 0",
        "to-stderr",
        "no process named \"two\" was started in this scenario (started: one)",
        "--- command (text) ---",
        "--- cwd (text) ---",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
    assert!(stdout.contains("scenarios: 6, failed: 6"), "{stdout}");
}

#[test]
fn nothing_started_outlives_its_scenario_or_its_file() {
    let bin = require_bddkit!();
    let pids = std::env::temp_dir().join(format!("bddkit-exec-e2e-pids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&pids);
    std::fs::create_dir_all(&pids).expect("mkdir");
    // Forward slashes: on Windows the features run under Git Bash.
    let piddir = PathBuf::from(pids.to_string_lossy().replace('\\', "/"));
    // The second scenario asserts the first one's tree is gone: the reset.
    let out = bddkit(
        &bin,
        &[
            "run",
            "--config",
            "tests/exec.yaml",
            "tests/features/leftovers.feature",
        ],
        &[("PIDDIR", &piddir)],
    );
    assert!(out.status.success(), "{}", show(&out));
    // The last scenario's process is killed when the file ends: the drop.
    // Under Git Bash `$$` is an MSYS pid, not a Windows one, so the check
    // from outside is Unix only; the reset check inside the feature runs
    // everywhere.
    for name in ["leader", "member", "last"] {
        let pid: i32 = std::fs::read_to_string(pids.join(name))
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        #[cfg(unix)]
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .expect("kill")
            .success();
        #[cfg(unix)]
        assert!(!alive, "{name} ({pid}) outlived the run");
        #[cfg(windows)]
        let _ = pid;
    }
}

#[test]
fn debug_traces_the_command_and_its_exit_to_stderr() {
    let bin = require_bddkit!();
    let out = bddkit(
        &bin,
        &[
            "run",
            "--config",
            "tests/exec.yaml",
            "tests/features/debug.feature",
        ],
        &[],
    );
    assert!(out.status.success(), "{}", show(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("-c 'echo traced; exit 2'") && stderr.contains("[exec] $ "),
        "{stderr}"
    );
    assert!(stderr.contains("[exec] exit code 2"), "{stderr}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("[exec]"),
        "stdout is the host's"
    );
}

#[test]
fn doctor_live_probes_every_instance() {
    let bin = require_bddkit!();
    let out = bddkit(
        &bin,
        &["doctor", "--live", "--config", "examples/exec.yaml"],
        &[],
    );
    assert!(out.status.success(), "{}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for instance in ["plugin exec.local", "plugin exec.tools"] {
        assert!(stdout.contains(instance), "missing {instance}:\n{stdout}");
    }
    assert!(stdout.contains("probed clean"), "{stdout}");
}

/// cmd.exe and PowerShell, the shells a Windows suite reaches without Git
/// Bash.
#[cfg(windows)]
#[test]
fn the_windows_examples_pass() {
    let bin = require_bddkit!();
    let out = bddkit(&bin, &["run", "--config", "examples/windows.yaml"], &[]);
    assert!(out.status.success(), "{}", show(&out));
}
