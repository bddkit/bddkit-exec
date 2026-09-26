//! The C exports driven in-process, the way the host drives them: JSON in,
//! JSON out, every returned string handed back to `bddkit_free_string`.
//! Needs no `bddkit` binary.

use std::ffi::{CStr, CString, c_char};

use bddkit_exec::*;
use serde_json::{Value, json};

/// Command lines in the platform's default shell: `sh` or `cmd`.
#[cfg(unix)]
mod line {
    pub const ECHO_WHO_EXIT_5: &str = "echo hi $WHO; exit 5";
    pub const ECHO_WHO_OUTPUT: &str = "hi exports\n";
    pub const SLEEP: &str = "sleep 30";
    pub const TRUE: &str = "true";
    pub const BIG_THEN_SLEEP: &str = "head -c 20000 /dev/zero | tr '\\0' x; echo END; sleep 30";
}
#[cfg(windows)]
mod line {
    pub const ECHO_WHO_EXIT_5: &str = "echo hi %WHO%& exit /b 5";
    pub const ECHO_WHO_OUTPUT: &str = "hi exports\r\n";
    pub const SLEEP: &str = "ping -n 30 127.0.0.1 >nul";
    pub const TRUE: &str = "exit /b 0";
    pub const BIG_THEN_SLEEP: &str =
        "(for /L %i in (1,1,2000) do @echo xxxxxxxxxx)& echo END& ping -n 30 127.0.0.1 >nul";
}

fn call(reply: *mut c_char) -> Value {
    assert!(!reply.is_null(), "an export returned NULL");
    let text = unsafe { CStr::from_ptr(reply) }
        .to_string_lossy()
        .into_owned();
    unsafe { bddkit_free_string(reply) };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

fn cstr(v: Value) -> CString {
    CString::new(v.to_string()).expect("no NUL")
}

fn instance_request(config: Value) -> CString {
    cstr(json!({"group": "exec", "instance": "t", "config": config, "options": {}}))
}

fn step_index(pattern_start: &str) -> u32 {
    let steps = call(bddkit_list_steps());
    let index = steps
        .as_array()
        .expect("array")
        .iter()
        .position(|s| {
            s["pattern"]
                .as_str()
                .is_some_and(|p| p.starts_with(pattern_start))
        })
        .unwrap_or_else(|| panic!("no step starts with {pattern_start}"));
    u32::try_from(index).expect("index")
}

/// Where a dispatch's evidence files would go; unique per call, as the
/// host's are.
fn artifacts_dir() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::env::temp_dir().join(format!("bddkit-exec-artifacts-{}-{n}", std::process::id()))
}

fn dispatch(handle: u64, pattern_start: &str, args: &[&str], docstring: Option<&str>) -> Value {
    dispatch_into(handle, pattern_start, args, docstring, &artifacts_dir())
}

fn dispatch_into(
    handle: u64,
    pattern_start: &str,
    args: &[&str],
    docstring: Option<&str>,
    artifacts: &std::path::Path,
) -> Value {
    let workspace = std::env::temp_dir();
    let request = cstr(json!({
        "args": args, "docstring": docstring, "table": null,
        "artifacts_dir": artifacts.display().to_string(),
        "workspace_dir": workspace.display().to_string(),
        "debug": false, "options": {},
    }));
    call(bddkit_dispatch(
        handle,
        step_index(pattern_start),
        request.as_ptr(),
    ))
}

fn init(config: Value) -> u64 {
    let reply = call(bddkit_init_instance(instance_request(config).as_ptr()));
    assert_eq!(reply["ok"], true, "{reply}");
    reply["handle"].as_u64().expect("handle")
}

#[test]
fn the_manifest_and_the_step_list_parse() {
    assert_eq!(bddkit_abi_version(), 1);
    assert_eq!(call(bddkit_manifest())["name"], "exec");
    let steps = call(bddkit_list_steps());
    assert!(
        steps
            .as_array()
            .expect("array")
            .iter()
            .all(|s| s["group"] == "exec")
    );
}

#[test]
fn validate_config_refuses_a_typo_and_accepts_the_implicit_body() {
    let bad = call(bddkit_validate_config(
        instance_request(json!({"timeout": 5})).as_ptr(),
    ));
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"]
            .as_str()
            .expect("error")
            .contains("\"timeout\""),
        "{bad}"
    );
    let good = call(bddkit_validate_config(instance_request(json!({})).as_ptr()));
    assert_eq!(good, json!({"ok": true}));
}

#[test]
fn a_run_publishes_its_variables_and_the_last_command_is_assertable() {
    let handle = init(json!({"env": {"WHO": "exports"}}));
    let ran = dispatch(
        handle,
        "^I run the command \"(?P<command>",
        &[line::ECHO_WHO_EXIT_5],
        None,
    );
    assert_eq!(ran["status"], "passed", "{ran}");
    assert_eq!(
        ran["vars"],
        json!({"exec_exit_code": "5", "exec_stdout": line::ECHO_WHO_OUTPUT, "exec_stderr": ""})
    );
    let code = dispatch(handle, "^the command exit code is (?P<code>", &["5"], None);
    assert_eq!(code["status"], "passed", "{code}");
    let equals = dispatch(
        handle,
        "^the (?:command|\"(?P<name>[^\"]+)\" process) (?P<stream>output|error output) equals:",
        &["", "output"],
        Some("\nhi exports\n"),
    );
    assert_eq!(equals["status"], "passed", "{equals}");
    assert_eq!(call(bddkit_drop_instance(handle)), json!({"ok": true}));
}

#[test]
fn a_mismatch_on_a_live_process_is_not_yet_and_the_reset_forgets_it() {
    let handle = init(json!({}));
    let started = dispatch(
        handle,
        "^I start the \"(?P<name>[^\"]+)\" process running \"(?P<command>",
        &["p", line::SLEEP],
        None,
    );
    assert_eq!(started["status"], "passed", "{started}");
    let contains =
        "^the (?:command|\"(?P<name>[^\"]+)\" process) (?P<stream>output|error output) contains \"";
    let observed = dispatch(handle, contains, &["p", "output", "anything"], None);
    assert_eq!(observed["status"], "not_yet", "{observed}");
    assert_eq!(call(bddkit_reset_scenario(handle)), json!({"ok": true}));
    let forgotten = dispatch(handle, contains, &["p", "output", "anything"], None);
    assert_eq!(forgotten["status"], "fatal");
    assert!(
        forgotten["error"]
            .as_str()
            .expect("error")
            .contains("started: none"),
        "{forgotten}"
    );
    call(bddkit_drop_instance(handle));
}

#[test]
fn every_handle_is_distinct_and_an_unknown_one_is_refused() {
    let (a, b) = (init(json!({})), init(json!({})));
    assert_ne!(a, b);
    call(bddkit_drop_instance(a));
    call(bddkit_drop_instance(b));
    assert_eq!(call(bddkit_drop_instance(a))["ok"], false);
    let reply = dispatch(a, "^I run the command \"(?P<command>", &[line::TRUE], None);
    assert_eq!(
        (reply["status"].clone(), reply["error"].clone()),
        (json!("fatal"), json!("unknown handle"))
    );
}

const CONTAINS: &str =
    "^the (?:command|\"(?P<name>[^\"]+)\" process) (?P<stream>output|error output) contains \"";

#[test]
fn an_out_of_range_timeout_is_refused_not_a_panic() {
    let handle = init(json!({}));
    let reply = dispatch(handle, "^the command timeout is", &["1e19"], None);
    assert_eq!(reply["status"], "fatal");
    assert!(
        reply["error"]
            .as_str()
            .expect("error")
            .contains("at most 86400"),
        "{reply}"
    );
    let config = call(bddkit_validate_config(
        instance_request(json!({"timeout_secs": 1e20})).as_ptr(),
    ));
    assert!(
        config["error"]
            .as_str()
            .expect("error")
            .contains("at most 86400"),
        "{config}"
    );
    call(bddkit_drop_instance(handle));
}

#[test]
fn not_yet_on_a_big_stream_keeps_its_tail_inline_and_writes_nothing() {
    let handle = init(json!({}));
    let start = "^I start the \"(?P<name>[^\"]+)\" process running \"(?P<command>";
    let big = line::BIG_THEN_SLEEP;
    assert_eq!(
        dispatch(handle, start, &["big", big], None)["status"],
        "passed"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while dispatch(handle, CONTAINS, &["big", "output", "END"], None)["status"] != "passed" {
        assert!(
            std::time::Instant::now() < deadline,
            "the output never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let artifacts = artifacts_dir();
    let reply = dispatch_into(
        handle,
        CONTAINS,
        &["big", "output", "never"],
        None,
        &artifacts,
    );
    assert_eq!(reply["status"], "not_yet", "{reply}");
    assert!(
        !artifacts.exists(),
        "a not_yet wrote {}",
        artifacts.display()
    );
    let stdout = &reply["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .find(|d| d["title"].as_str().is_some_and(|t| t.starts_with("stdout")))
        .expect("stdout")
        .clone();
    assert!(stdout["path"].is_null());
    assert!(
        stdout["content"]
            .as_str()
            .expect("inline")
            .trim_end()
            .ends_with("END"),
        "{stdout}"
    );
    call(bddkit_drop_instance(handle));
}

#[test]
fn a_mismatch_on_a_process_that_has_ended_is_fatal_not_not_yet() {
    let handle = init(json!({}));
    let start = "^I start the \"(?P<name>[^\"]+)\" process running \"(?P<command>";
    assert_eq!(
        dispatch(handle, start, &["brief", "echo done"], None)["status"],
        "passed"
    );
    // Until the exit and both EOFs are seen the answer may still be not_yet;
    // after that, another look can never help.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let reply = dispatch(handle, CONTAINS, &["brief", "output", "never"], None);
        if reply["status"] == "fatal" {
            break;
        }
        assert_eq!(reply["status"], "not_yet", "{reply}");
        assert!(
            std::time::Instant::now() < deadline,
            "still not_yet after the process ended"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    call(bddkit_drop_instance(handle));
}
