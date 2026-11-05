//! The step vocabulary. An index into `STEPS` is what the host passes back,
//! and `Step` is what `route` matches on, so a step is added at the END, never
//! in the middle.
//!
//! Every pattern carries a domain word, `command` or `process`: plugin
//! patterns share one namespace with the host and every other plugin, and
//! `the output contains "x"` is exactly what the next plugin would also pick.
//!
//! Free text — a command, a text, a regex, an env value — is captured with a
//! greedy `(.*)` up to the closing `"$`, so it may itself contain `"`:
//! `I run the command "grep "a b" log"` is one command. Names stay `[^"]+`.

use serde_json::{Value, json};

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
pub(super) enum Step {
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

pub(super) const STEPS: &[(Step, &str, &str, &str)] = &[
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

#[cfg(test)]
mod tests {
    use regex::Regex;

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
