//! The step vocabulary and its dispatch. `STEPS` and the `match` in `route`
//! are the plugin's contract with itself: an index into this table is what
//! the host passes back, so a step is added at the END, never in the middle.
//!
//! Every pattern carries a domain word, `command` or `process`: plugin
//! patterns share one namespace with the host and every other plugin, and
//! `the output contains "x"` is exactly what the next plugin would also pick.
//!
//! Free text — a command, a text, a regex, an env value — is captured with a
//! greedy `(.*)` up to the closing `"$`, so it may itself contain `"`:
//! `I run the command "grep "a b" log"` is one command. Names stay `[^"]+`.

use std::path::Path;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::config;
use crate::exec::{self, Exit, Job, Program, TimedOut};
use crate::instance::Instance;
use crate::matchers::{self, Delimited};
use crate::reply::{self, Diagnostic};

/// The four subjects of a stream assertion in one pattern. The host passes
/// the unmatched `name` group as `""`, which is how "the last command" is
/// told from a process.
macro_rules! stream {
    ($rest:literal) => {
        concat!(
            r#"^the (?:command|"(?P<name>[^"]+)" process) (?P<stream>output|error output) "#,
            $rest
        )
    };
}

/// Same subjects, inside an `extract` sentence.
macro_rules! extract {
    ($head:literal, $tail:literal) => {
        concat!(
            "^extract ",
            $head,
            r#" from the (?:command|"(?P<name>[^"]+)" process) (?P<stream>output|error output) "#,
            $tail,
            r#""(?P<var>[^"]+)"$"#
        )
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    Env,
    Cwd,
    Stdin,
    Timeout,
    RunShell,
    RunArgv,
    StartShell,
    StartArgv,
    Stop,
    ExitCodeIs,
    ExitCodeIsNot,
    Running,
    ExitedWith,
    Contains,
    NotContains,
    Matches,
    NotMatches,
    IsEmpty,
    Equals,
    Lines,
    ContainsJson,
    EqualsJson,
    NotContainsJson,
    TableContains,
    TableEquals,
    ExtractRegex,
    ExtractJson,
}

const ACTION: &str = "action";
const ASSERTION: &str = "assertion";

const STEPS: &[(Step, &str, &str, &str)] = &[
    (
        Step::Env,
        r#"^the command environment variable "(?P<name>[^"]+)" is "(?P<value>.*)"$"#,
        ACTION,
        "sets an environment variable for every command and process started later in this scenario",
    ),
    (
        Step::Cwd,
        r#"^the command working directory is "(?P<path>[^"]+)"$"#,
        ACTION,
        "working directory for the rest of the scenario, relative to the instance cwd",
    ),
    (
        Step::Stdin,
        r"^the command standard input is:$",
        ACTION,
        "the doc string is the standard input of the next one-shot command only",
    ),
    (
        Step::Timeout,
        r#"^the command timeout is "(?P<seconds>[^"]+)" seconds$"#,
        ACTION,
        "overrides the instance timeout_secs for one-shot commands for the rest of the scenario",
    ),
    (
        Step::RunShell,
        r#"^I run the command "(?P<command>.*)"$"#,
        ACTION,
        "runs `<shell> -c <command>` to completion; publishes exec_exit_code, exec_stdout, exec_stderr",
    ),
    (
        Step::RunArgv,
        r#"^I run the command "(?P<program>[^"]+)" with arguments:$"#,
        ACTION,
        "runs a program with no shell, one table row (header `argument`) per argument",
    ),
    (
        Step::StartShell,
        r#"^I start the "(?P<name>[^"]+)" process running "(?P<command>.*)"$"#,
        ACTION,
        "starts `<shell> -c <command>` in the background; it is stopped at the end of the scenario",
    ),
    (
        Step::StartArgv,
        r#"^I start the "(?P<name>[^"]+)" process running "(?P<program>[^"]+)" with arguments:$"#,
        ACTION,
        "starts a program in the background with no shell, one table row (header `argument`) per argument",
    ),
    (
        Step::Stop,
        r#"^I stop the "(?P<name>[^"]+)" process$"#,
        ACTION,
        "asks the whole process tree to stop, waits stop_grace_secs (default 2), then kills it; its exit becomes readable",
    ),
    (
        Step::ExitCodeIs,
        r"^the command exit code is (?P<code>\d+)$",
        ASSERTION,
        "asserts the exit code of the last command",
    ),
    (
        Step::ExitCodeIsNot,
        r"^the command exit code is not (?P<code>\d+)$",
        ASSERTION,
        "asserts the last command did not exit with this code",
    ),
    (
        Step::Running,
        r#"^the "(?P<name>[^"]+)" process should be running$"#,
        ASSERTION,
        "asserts the process has not exited",
    ),
    (
        Step::ExitedWith,
        r#"^the "(?P<name>[^"]+)" process should have exited with code (?P<code>\d+)$"#,
        ASSERTION,
        "asserts the process has exited with this code; while it runs, the answer is not yet",
    ),
    (
        Step::Contains,
        stream!(r#"contains "(?P<text>.*)"$"#),
        ASSERTION,
        "asserts the stream contains the text",
    ),
    (
        Step::NotContains,
        stream!(r#"does not contain "(?P<text>.*)"$"#),
        ASSERTION,
        "asserts the stream does not contain the text",
    ),
    (
        Step::Matches,
        stream!(r#"matches "(?P<regex>.*)"$"#),
        ASSERTION,
        "asserts the regex (unanchored) matches the stream",
    ),
    (
        Step::NotMatches,
        stream!(r#"does not match "(?P<regex>.*)"$"#),
        ASSERTION,
        "asserts the regex (unanchored) does not match the stream",
    ),
    (
        Step::IsEmpty,
        stream!(r"is empty$"),
        ASSERTION,
        "asserts the stream is empty",
    ),
    (
        Step::Equals,
        stream!(r"equals:$"),
        ASSERTION,
        "asserts the stream equals the doc string; one trailing newline of the stream is ignored",
    ),
    (
        Step::Lines,
        stream!(r"has (?P<count>\d+) lines?$"),
        ASSERTION,
        "asserts the number of lines in the stream",
    ),
    (
        Step::ContainsJson,
        stream!(r"contains JSON:$"),
        ASSERTION,
        "asserts the stream, parsed as JSON, contains the doc string: object subset, unordered arrays",
    ),
    (
        Step::EqualsJson,
        stream!(r"equals JSON:$"),
        ASSERTION,
        "asserts the stream, parsed as JSON, equals the doc string exactly",
    ),
    (
        Step::NotContainsJson,
        stream!(r"does not contain JSON:$"),
        ASSERTION,
        "asserts the stream, parsed as JSON, does not contain the doc string",
    ),
    (
        Step::TableContains,
        stream!(r"as (?P<format>CSV|TSV) contains:$"),
        ASSERTION,
        "asserts every table row appears in the stream; the table header names the columns",
    ),
    (
        Step::TableEquals,
        stream!(r"as (?P<format>CSV|TSV) equals:$"),
        ASSERTION,
        "asserts the stream has exactly the table's columns and rows, in order",
    ),
    (
        Step::ExtractRegex,
        extract!(r#""(?P<regex>.*)""#, "as "),
        ACTION,
        "stores the first capture group of the first regex match as a variable",
    ),
    (
        Step::ExtractJson,
        extract!(r#""(?P<path>[^"]*)""#, "as JSON as "),
        ACTION,
        "parses the stream as JSON and stores the value at the path as a variable",
    ),
];

pub fn steps_json() -> String {
    Value::Array(
        STEPS
            .iter()
            .map(|(_, pattern, kind, description)| {
                json!({"pattern": pattern, "group": "exec", "kind": kind, "description": description})
            })
            .collect(),
    )
    .to_string()
}

/// bddkit 0.2.1 hands a doc string over with the newline after the opening
/// `"""` and the one before the closing `"""` still attached: `"\npear\n"`
/// for a one-line `pear`, and `"\n"` for an empty one. Both are taken off,
/// and only as a pair, so a host that stops sending them — or a doc string
/// whose own first line is blank — is left as written.
fn unframe(text: &str) -> String {
    if text == "\n" {
        return String::new();
    }
    match text.strip_prefix('\n').and_then(|t| t.strip_suffix('\n')) {
        Some(inner) => inner.to_string(),
        None => text.to_string(),
    }
}

/// A dispatch request, as the host sends it.
pub struct Request {
    pub args: Vec<String>,
    pub docstring: Option<String>,
    pub table: Option<Vec<Vec<String>>>,
    pub artifacts_dir: String,
    pub workspace_dir: String,
    pub debug: bool,
}

impl Request {
    pub fn parse(v: &Value) -> Self {
        let text = |key: &str| v[key].as_str().unwrap_or_default().to_string();
        Self {
            args: serde_json::from_value(v["args"].clone()).unwrap_or_default(),
            docstring: v["docstring"].as_str().map(unframe),
            table: serde_json::from_value(v["table"].clone()).ok(),
            artifacts_dir: text("artifacts_dir"),
            workspace_dir: text("workspace_dir"),
            debug: v["debug"].as_bool().unwrap_or(false),
        }
    }

    fn arg(&self, i: usize) -> &str {
        self.args.get(i).map_or("", String::as_str)
    }
}

pub fn route(instance: &mut Instance, index: u32, req: &Request) -> String {
    let Some(&(step, ..)) = STEPS.get(index as usize) else {
        return reply::fatal(&format!("unknown step index {index}"), &[]);
    };
    match step {
        Step::Env => set_env(instance, req.arg(0), req.arg(1)),
        Step::Cwd => match refuse_nul(&[req.arg(0)]) {
            Ok(()) => {
                instance.context.cwd = Some(req.arg(0).to_string());
                reply::passed()
            }
            Err(e) => reply::fatal(&e, &[]),
        },
        Step::Stdin => match &req.docstring {
            Some(text) if !text.contains('\0') => {
                // A doc string is lines, and a line ends with a newline:
                // without it `read` in a shell drops the last one.
                let lines = if text.is_empty() {
                    String::new()
                } else {
                    format!("{text}\n")
                };
                instance.context.stdin = Some(lines);
                reply::passed()
            }
            Some(_) => reply::fatal(&nul_refusal(), &[]),
            None => reply::fatal("this step takes a doc string", &[]),
        },
        Step::Timeout => {
            let parsed = req
                .arg(0)
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("{:?} is not a number of seconds", req.arg(0)));
            match parsed.and_then(config::timeout) {
                Ok(timeout) => {
                    instance.context.timeout = Some(timeout);
                    reply::passed()
                }
                Err(e) => reply::fatal(&e, &[]),
            }
        }
        Step::RunShell => {
            let program = shell(instance, req.arg(0));
            run(instance, req, program)
        }
        Step::RunArgv => match argv(req.arg(0), req) {
            Ok(program) => run(instance, req, program),
            Err(e) => reply::fatal(&e, &[]),
        },
        Step::StartShell => {
            let program = shell(instance, req.arg(1));
            start(instance, req, req.arg(0), program)
        }
        Step::StartArgv => match argv(req.arg(1), req) {
            Ok(program) => start(instance, req, req.arg(0), program),
            Err(e) => reply::fatal(&e, &[]),
        },
        Step::Stop => {
            let grace = instance.config.stop_grace;
            match instance.process(req.arg(0)) {
                Some(job) => {
                    exec::stop_all([&mut *job], grace);
                    trace(
                        req,
                        format_args!("stopped {:?}: {}", req.arg(0), describe_exit(job.exit())),
                    );
                    reply::passed()
                }
                None => unknown_process(instance, req.arg(0)),
            }
        }
        Step::ExitCodeIs | Step::ExitCodeIsNot => {
            exit_code(instance, req, step == Step::ExitCodeIs)
        }
        Step::Running => match instance.process(req.arg(0)) {
            Some(job) => match job.poll() {
                None => reply::passed(),
                Some(exit) => {
                    let msg = format!(
                        "the {:?} process is not running: it ended with {exit}",
                        req.arg(0)
                    );
                    fail(job, req, &msg)
                }
            },
            None => unknown_process(instance, req.arg(0)),
        },
        Step::ExitedWith => exited_with(instance, req),
        Step::Contains => on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
            let want = req.arg(2);
            check(text.contains(want), || {
                format!("{subject} does not contain {want:?}")
            })
        }),
        Step::NotContains => on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
            let unwanted = req.arg(2);
            check(!text.contains(unwanted), || {
                format!("{subject} contains {unwanted:?}")
            })
        }),
        Step::Matches | Step::NotMatches => {
            on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
                let re = Regex::new(req.arg(2))
                    .map_err(|e| Failure::Fatal(format!("invalid regex: {e}")))?;
                let want = step == Step::Matches;
                check(re.is_match(text) == want, || {
                    let verb = if want { "does not match" } else { "matches" };
                    format!("{subject} {verb} {:?}", req.arg(2))
                })
            })
        }
        Step::IsEmpty => on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
            check(text.is_empty(), || {
                format!("{subject} is not empty ({} bytes)", text.len())
            })
        }),
        Step::Equals => on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
            let want = docstring(req)?;
            // One trailing line ending, whichever the platform writes.
            let got = text
                .strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(text);
            check(got == want, || {
                format!(
                    "{subject} is not equal to the doc string: {}",
                    first_difference(want, got)
                )
            })
        }),
        Step::Lines => on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
            let want: usize = req
                .arg(2)
                .parse()
                .map_err(|_| Failure::Fatal(format!("{:?} is not a line count", req.arg(2))))?;
            let got = text.lines().count();
            check(got == want, || {
                format!("{subject} has {got} lines, expected {want}")
            })
        }),
        Step::ContainsJson | Step::EqualsJson | Step::NotContainsJson => {
            on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
                let expected: Value = serde_json::from_str(docstring(req)?)
                    .map_err(|e| Failure::Fatal(format!("the doc string is not JSON: {e}")))?;
                let actual: Value = serde_json::from_str(text)
                    .map_err(|e| Failure::Mismatch(format!("{subject} is not JSON: {e}")))?;
                let outcome =
                    matchers::json_cmp(&actual, &expected, "root", step == Step::EqualsJson);
                match (step, outcome) {
                    (Step::NotContainsJson, Ok(())) => {
                        Err(Failure::Mismatch(format!("{subject} contains the JSON")))
                    }
                    (Step::NotContainsJson, Err(_)) | (_, Ok(())) => Ok(None),
                    (_, Err(why)) => Err(Failure::Mismatch(format!(
                        "{subject} does not match the JSON:\n  {why}"
                    ))),
                }
            })
        }
        Step::TableContains | Step::TableEquals => {
            on_stream(instance, req, req.arg(0), req.arg(1), |text, subject| {
                let format = req.arg(2);
                let table = req
                    .table
                    .as_deref()
                    .ok_or_else(|| Failure::Fatal("this step takes a table".into()))?;
                let rows = Delimited::parse(format)
                    .rows(text)
                    .map_err(|e| Failure::Mismatch(format!("{subject}: {e}")))?;
                matchers::table_cmp(&rows, table, step == Step::TableEquals)
                    .map(|()| None)
                    .map_err(|why| Failure::Mismatch(format!("{subject} as {format}: {why}")))
            })
        }
        Step::ExtractRegex => extract(instance, req, |text, subject| {
            let re = Regex::new(req.arg(0)).map_err(|e| format!("invalid regex: {e}"))?;
            if re.captures_len() < 2 {
                return Err("the regex has no capture group; wrap what to extract in (…)".into());
            }
            let caps = re
                .captures(text)
                .ok_or_else(|| format!("{subject} does not match {:?}", req.arg(0)))?;
            Ok(caps.get(1).map_or("", |m| m.as_str()).to_string())
        }),
        Step::ExtractJson => extract(instance, req, |text, subject| {
            let v: Value =
                serde_json::from_str(text).map_err(|e| format!("{subject} is not JSON: {e}"))?;
            matchers::json_read(&v, req.arg(0)).map(matchers::scalar)
        }),
    }
}

fn nul_refusal() -> String {
    "<<null>> cannot be part of a command, an argument, the environment or a path: a NUL byte cannot cross exec".into()
}

/// Checked where the value is written, so the failure names the step that
/// carried the NUL rather than a later `I run`.
fn refuse_nul(values: &[&str]) -> Result<(), String> {
    if values.iter().any(|v| v.contains('\0')) {
        Err(nul_refusal())
    } else {
        Ok(())
    }
}

fn set_env(instance: &mut Instance, name: &str, value: &str) -> String {
    if let Err(e) = refuse_nul(&[name, value]) {
        return reply::fatal(&e, &[]);
    }
    if name.contains('=') {
        return reply::fatal(
            &format!("{name:?} is not a valid environment variable name"),
            &[],
        );
    }
    let env = &mut instance.context.env;
    env.retain(|(k, _)| k != name);
    env.push((name.to_string(), value.to_string()));
    reply::passed()
}

fn shell(instance: &Instance, line: &str) -> Program {
    Program::Shell {
        shell: instance.config.shell.clone(),
        line: line.to_string(),
    }
}

fn argv(program: &str, req: &Request) -> Result<Program, String> {
    let table = req
        .table
        .as_deref()
        .ok_or("this step takes a table with the header `argument`")?;
    match table.first().map(Vec::as_slice) {
        Some([header]) if header == "argument" => {}
        _ => {
            return Err(
                "the arguments table must have exactly one column, headed `argument`".into(),
            );
        }
    }
    let mut argv = vec![program.to_string()];
    argv.extend(table[1..].iter().map(|row| row[0].clone()));
    Ok(Program::Argv(argv))
}

fn trace(req: &Request, message: std::fmt::Arguments) {
    // stderr, never stdout: stdout belongs to the host's per-file dump.
    if req.debug {
        eprintln!("[exec] {message}");
    }
}

fn describe_exit(exit: Option<Exit>) -> String {
    exit.map_or_else(|| "still running".to_string(), |e| e.to_string())
}

fn run(instance: &mut Instance, req: &Request, program: Program) -> String {
    let command = match &program {
        Program::Shell { line, .. } => vec![line.as_str()],
        Program::Argv(argv) => argv.iter().map(String::as_str).collect(),
    };
    if let Err(e) = refuse_nul(&command) {
        return reply::fatal(&e, &[]);
    }
    let spec = instance.spec(program, Path::new(&req.workspace_dir), true);
    trace(
        req,
        format_args!("$ {} (in {})", spec.describe(), spec.cwd.display()),
    );
    instance.last = None;
    let timeout = instance.timeout();
    match exec::run(spec, instance.config.max_output_bytes, timeout) {
        Err(e) => reply::fatal(&e, &[]),
        Ok(Err(TimedOut(mut job))) => {
            trace(req, format_args!("timed out after {timeout:?}"));
            let msg = format!(
                "`{}` did not finish within {} seconds; its process tree was killed",
                job.spec.describe(),
                timeout.as_secs_f64()
            );
            fail(&mut job, req, &msg)
        }
        Ok(Ok(job)) => {
            trace(req, format_args!("{}", describe_exit(job.exit())));
            let code = match job.exit() {
                Some(Exit::Code(c)) => c.to_string(),
                _ => String::new(),
            };
            let vars = json!({
                "exec_exit_code": code,
                "exec_stdout": exec::text_of(&job.stdout),
                "exec_stderr": exec::text_of(&job.stderr),
            });
            instance.last = Some(job);
            reply::passed_with(vars)
        }
    }
}

fn start(instance: &mut Instance, req: &Request, name: &str, program: Program) -> String {
    let mut values = vec![name];
    match &program {
        Program::Shell { line, .. } => values.push(line),
        Program::Argv(argv) => values.extend(argv.iter().map(String::as_str)),
    }
    if let Err(e) = refuse_nul(&values) {
        return reply::fatal(&e, &[]);
    }
    if instance.process(name).is_some() {
        return reply::fatal(
            &format!("a process named {name:?} was already started in this scenario"),
            &[],
        );
    }
    let spec = instance.spec(program, Path::new(&req.workspace_dir), false);
    trace(
        req,
        format_args!(
            "start {name:?}: $ {} (in {})",
            spec.describe(),
            spec.cwd.display()
        ),
    );
    match Job::spawn(spec, instance.config.max_output_bytes) {
        Ok(job) => {
            instance.processes.push((name.to_string(), job));
            reply::passed()
        }
        Err(e) => reply::fatal(&e, &[]),
    }
}

fn no_such_process(instance: &Instance, name: &str) -> String {
    let started: Vec<&str> = instance.processes.iter().map(|(n, _)| n.as_str()).collect();
    let known = if started.is_empty() {
        "none".to_string()
    } else {
        started.join(", ")
    };
    format!("no process named {name:?} was started in this scenario (started: {known})")
}

fn unknown_process(instance: &Instance, name: &str) -> String {
    reply::fatal(&no_such_process(instance, name), &[])
}

const NO_COMMAND: &str = "no command has run in this scenario";

/// `\d+` in the pattern guarantees digits, not that they fit.
fn exit_code_arg(arg: &str) -> Result<i32, String> {
    arg.parse()
        .map_err(|_| format!("{arg} is not an exit code a process can have"))
}

fn exit_code(instance: &mut Instance, req: &Request, equal: bool) -> String {
    let Some(job) = instance.last.as_mut() else {
        return reply::fatal(NO_COMMAND, &[]);
    };
    let want = match exit_code_arg(req.arg(0)) {
        Ok(code) => code,
        Err(e) => return reply::fatal(&e, &[]),
    };
    match job.exit() {
        Some(Exit::Code(code)) if (code == want) == equal => reply::passed(),
        Some(Exit::Code(code)) => {
            let msg = if equal {
                format!("the command exit code is {code}, expected {want}")
            } else {
                format!("the command exit code is {code}")
            };
            fail(job, req, &msg)
        }
        exit => {
            let msg = format!(
                "the command has no exit code: it was {}",
                describe_exit(exit)
            );
            fail(job, req, &msg)
        }
    }
}

fn exited_with(instance: &mut Instance, req: &Request) -> String {
    let name = req.arg(0);
    let want = match exit_code_arg(req.arg(1)) {
        Ok(code) => code,
        Err(e) => return reply::fatal(&e, &[]),
    };
    let Some(job) = instance.process(name) else {
        return unknown_process(instance, name);
    };
    match job.poll() {
        None => later(job, &format!("the {name:?} process is still running")),
        Some(Exit::Code(code)) if code == want => reply::passed(),
        Some(exit) => {
            let msg = format!("the {name:?} process ended with {exit}, expected exit code {want}");
            fail(job, req, &msg)
        }
    }
}

enum Failure {
    /// Observed and not (yet) what was asked.
    Mismatch(String),
    /// The step itself is wrong, or the stream can never pass.
    Fatal(String),
}

type Checked = Result<Option<Value>, Failure>;

fn check(ok: bool, why: impl FnOnce() -> String) -> Checked {
    if ok {
        Ok(None)
    } else {
        Err(Failure::Mismatch(why()))
    }
}

fn docstring(req: &Request) -> Result<&str, Failure> {
    req.docstring
        .as_deref()
        .ok_or_else(|| Failure::Fatal("this step takes a doc string".into()))
}

/// Where two texts first part, by line, for an `equals:` failure.
fn first_difference(want: &str, got: &str) -> String {
    let (want, got): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    let shown = |line: Option<&&str>| line.map_or("nothing".to_string(), |l| format!("{l:?}"));
    match (0..want.len().max(got.len())).find(|&i| want.get(i) != got.get(i)) {
        Some(i) => format!(
            "line {} is {}, expected {}",
            i + 1,
            shown(got.get(i)),
            shown(want.get(i))
        ),
        None => "they differ only in line endings or trailing newlines".into(),
    }
}

/// Which job and which of its streams a stream step names, and how to say so.
fn subject<'a>(
    instance: &'a mut Instance,
    name: &str,
    stream: &str,
) -> Result<(&'a mut Job, String), String> {
    if name.is_empty() {
        let job = instance.last.as_mut().ok_or(NO_COMMAND)?;
        Ok((job, format!("the command {stream}")))
    } else if instance.process(name).is_some() {
        Ok((
            instance.process(name).expect("just found"),
            format!("the {name:?} process {stream}"),
        ))
    } else {
        Err(no_such_process(instance, name))
    }
}

/// Reads the stream fresh — every attempt of an eventual assertion comes
/// through here — and hands its text to `check`.
fn on_stream(
    instance: &mut Instance,
    req: &Request,
    name: &str,
    stream: &str,
    check: impl FnOnce(&str, &str) -> Checked,
) -> String {
    let (job, subject) = match subject(instance, name, stream) {
        Ok(x) => x,
        Err(e) => return reply::fatal(&e, &[]),
    };
    let capture = if stream == "error output" {
        &job.stderr
    } else {
        &job.stdout
    };
    let (text, overflowed) = {
        let c = capture.lock().unwrap_or_else(|e| e.into_inner());
        (c.text(), c.overflowed)
    };
    if overflowed {
        let msg = format!(
            "{subject} reached max_output_bytes ({} bytes); what came after it was dropped, so no assertion on it can be trusted",
            text.len()
        );
        return fail(job, req, &msg);
    }
    match check(&text, &subject) {
        Ok(Some(vars)) => reply::passed_with(vars),
        Ok(None) => reply::passed(),
        // Only a stream that can still grow is worth another look: the last
        // command's is final, and so is a process's once it has ended and
        // both pipes are closed.
        Err(Failure::Mismatch(msg)) if !job.finished() => later(job, &msg),
        Err(Failure::Mismatch(msg) | Failure::Fatal(msg)) => fail(job, req, &msg),
    }
}

/// An action over a stream: its failure is always fatal.
fn extract(
    instance: &mut Instance,
    req: &Request,
    read: impl FnOnce(&str, &str) -> Result<String, String>,
) -> String {
    let var = req.arg(3).to_string();
    on_stream(instance, req, req.arg(1), req.arg(2), |text, subject| {
        let value = read(text, subject).map_err(Failure::Fatal)?;
        let mut vars = Map::new();
        vars.insert(var, Value::String(value));
        Ok(Some(Value::Object(vars)))
    })
}

/// Above this a stream is not rendered inline.
const INLINE_LIMIT: usize = 16 * 1024;

/// A failure: the evidence in full, a big stream written to `artifacts_dir`.
fn fail(job: &mut Job, req: &Request, msg: &str) -> String {
    reply::fatal(msg, &evidence(job, Some(req)))
}

/// Not yet: the same evidence with a big stream cut to its tail and nothing
/// written. An eventual assertion may make hundreds of attempts, each with a
/// fresh `artifacts_dir` nobody deletes; only the last one is ever shown.
fn later(job: &mut Job, msg: &str) -> String {
    reply::not_yet(msg, &evidence(job, None))
}

/// Everything a tester needs to reproduce the run by hand. `files` is the
/// request whose `artifacts_dir` a big stream goes to; `None` keeps its tail.
fn evidence(job: &mut Job, files: Option<&Request>) -> Vec<Diagnostic> {
    job.poll();
    let mut out = vec![
        Diagnostic::text("command", job.spec.describe()),
        Diagnostic::text("cwd", job.spec.cwd.display().to_string()),
    ];
    let env = job.spec.describe_env();
    if !env.is_empty() {
        out.push(Diagnostic::text("env", env));
    }
    out.push(Diagnostic::text("exit", describe_exit(job.exit())));
    for (title, capture) in [("stdout", &job.stdout), ("stderr", &job.stderr)] {
        let text = exec::text_of(capture);
        out.push(stream_evidence(title, text, files));
    }
    out
}

fn stream_evidence(title: &str, text: String, files: Option<&Request>) -> Diagnostic {
    if text.len() <= INLINE_LIMIT {
        return Diagnostic::text(title, text);
    }
    let written = files.map(|req| {
        let path = Path::new(&req.artifacts_dir).join(format!("{title}.txt"));
        std::fs::create_dir_all(&req.artifacts_dir)
            .and_then(|()| std::fs::write(&path, &text))
            .map(|()| path)
    });
    match written {
        Some(Ok(path)) => Diagnostic::file(
            format!("{title} ({} bytes)", text.len()),
            path.display().to_string(),
        ),
        failed => {
            let mut cut = text.len() - INLINE_LIMIT;
            while !text.is_char_boundary(cut) {
                cut += 1;
            }
            let why = match failed {
                Some(Err(e)) => format!("; writing it to a file failed: {e}"),
                _ => String::new(),
            };
            Diagnostic::text(
                format!(
                    "{title} (the last {} of {} bytes{why})",
                    text.len() - cut,
                    text.len()
                ),
                &text[cut..],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled() -> Vec<Regex> {
        STEPS
            .iter()
            .map(|(_, p, ..)| Regex::new(p).expect("pattern compiles"))
            .collect()
    }

    /// One sample sentence per step, in table order.
    const SAMPLES: &[&str] = &[
        r#"the command environment variable "APP_ENV" is "test""#,
        r#"the command working directory is "bin""#,
        "the command standard input is:",
        r#"the command timeout is "5" seconds"#,
        r#"I run the command "echo hi""#,
        r#"I run the command "echo" with arguments:"#,
        r#"I start the "log" process running "tail -F x""#,
        r#"I start the "log" process running "tail" with arguments:"#,
        r#"I stop the "log" process"#,
        "the command exit code is 0",
        "the command exit code is not 0",
        r#"the "log" process should be running"#,
        r#"the "log" process should have exited with code 0"#,
        r#"the command output contains "x""#,
        r#"the "log" process error output does not contain "x""#,
        r#"the command error output matches "x+""#,
        r#"the command output does not match "x+""#,
        "the command error output is empty",
        "the command output equals:",
        "the command output has 1 line",
        "the command output contains JSON:",
        "the command output equals JSON:",
        "the command output does not contain JSON:",
        r#"the "log" process output as CSV contains:"#,
        "the command output as TSV equals:",
        r#"extract "id=(\d+)" from the command output as "id""#,
        r#"extract "data.id" from the "log" process output as JSON as "id""#,
    ];

    #[test]
    fn every_sample_matches_its_own_step_and_no_other() {
        let patterns = compiled();
        assert_eq!(SAMPLES.len(), STEPS.len(), "one sample per step");
        for (i, sample) in SAMPLES.iter().enumerate() {
            let matched: Vec<usize> = (0..patterns.len())
                .filter(|&j| patterns[j].is_match(sample))
                .collect();
            assert_eq!(matched, vec![i], "{sample:?} matched {matched:?}");
        }
    }

    #[test]
    fn every_pattern_carries_a_domain_word_and_names_its_groups() {
        for (_, pattern, ..) in STEPS {
            assert!(
                pattern.contains("command") || pattern.contains("process"),
                "{pattern}"
            );
            assert!(
                pattern.starts_with('^') && pattern.ends_with('$'),
                "{pattern}"
            );
            assert!(!pattern.contains("(["), "an unnamed group in {pattern}");
        }
    }

    #[test]
    fn the_first_difference_names_the_line() {
        assert_eq!(
            first_difference("a\nb", "a\nc"),
            "line 2 is \"c\", expected \"b\""
        );
        assert_eq!(
            first_difference("a\nb", "a"),
            "line 2 is nothing, expected \"b\""
        );
        assert_eq!(
            first_difference("a", "a\n"),
            "they differ only in line endings or trailing newlines"
        );
    }

    #[test]
    fn a_big_stream_keeps_its_tail_inline_when_no_file_may_be_written() {
        let text = format!("{}END", "x".repeat(INLINE_LIMIT));
        let d = stream_evidence("stdout", text, None);
        assert!(d.path.is_none());
        assert!(d.content.as_deref().expect("inline").ends_with("END"));
        assert!(
            d.title.contains("the last 16384 of 16387 bytes"),
            "{}",
            d.title
        );
    }

    #[test]
    fn a_framed_doc_string_is_unframed_and_nothing_else_is() {
        assert_eq!(unframe("\npear\napple\n"), "pear\napple");
        assert_eq!(unframe("\n\n"), "");
        assert_eq!(unframe("\n"), "", "an empty doc string");
        assert_eq!(unframe("pear"), "pear");
        assert_eq!(unframe("\nblank first line"), "\nblank first line");
    }

    #[test]
    fn free_text_may_contain_quotes() {
        let run = Regex::new(STEPS[Step::RunShell as usize].1).expect("pattern");
        assert_eq!(
            &run.captures(r#"I run the command "grep "a b" log""#)
                .expect("match")["command"],
            r#"grep "a b" log"#
        );
        let extract = Regex::new(STEPS[Step::ExtractRegex as usize].1).expect("pattern");
        let caps = extract
            .captures(r#"extract "id="(\d+)"" from the "p" process output as "id""#)
            .expect("match");
        assert_eq!(
            (&caps["regex"], &caps["name"], &caps["var"]),
            (r#"id="(\d+)""#, "p", "id")
        );
    }

    #[test]
    fn the_command_subject_leaves_the_name_group_empty() {
        let re = Regex::new(STEPS[Step::Contains as usize].1).expect("pattern");
        let caps = re
            .captures(r#"the command error output contains "x""#)
            .expect("match");
        assert!(caps.name("name").is_none());
        assert_eq!(&caps["stream"], "error output");
    }

    #[test]
    fn the_enum_order_is_the_table_order() {
        for (i, (step, ..)) in STEPS.iter().enumerate() {
            assert_eq!(*step as usize, i, "{step:?}");
        }
    }
}
