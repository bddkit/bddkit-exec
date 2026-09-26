# bddkit-exec

A `bddkit` plugin that runs local commands, keeps long-running ones streaming in the background, and validates what they print — as text, JSON, CSV or TSV — in the same scenario as the API and DB steps `bddkit` already provides. Resource group: `exec`.

Use it to test the CLI a product ships, invoke a migration or a cron entry point, or follow a log while a request is made.

Runs on **Linux, macOS and Windows**. Every command's whole process tree is held together — by a process group on Linux and macOS, by a Job Object on Windows — and every stop and kill reaches all of it; see [Platforms](#platforms) for where the three differ.

## Building

```bash
cargo build --release
```

The library is written to `target/release/`.

## Installing

Tested against `bddkit` 0.2.1 (the version `ci.yml` pins for the end-to-end job, and the first that reads `implicit_instance`). Point a plugin lock file at the built library:

```yaml
# .bddkit/plugins.yaml, beside your config
plugin:
  - name: exec
    path: ../../bddkit-exec/target/release/libbddkit_exec.so
```

- On macOS the library is `libbddkit_exec.dylib`, on Windows `bddkit_exec.dll`.
- `name` must equal the plugin's manifest name, `exec`.
- `path` is absolute, or **relative to the lock file's own directory** (`.bddkit/`, not the project root).
- **`~` is not expanded** in `path`.

## Configuring an instance

Every key has a default, so the block is optional: a suite with no `resources.exec` at all runs commands on an implicit instance named `default`. Declare instances when you need several, or a non-default setting:

```yaml
resources:
  exec:
    app-host:
      cwd: /srv/app                 # default: the feature file's workspace directory
      shell: bash                   # default: sh; a string command runs as `<shell> -c <string>`
      timeout_secs: 30              # default 30, for one-shot commands
      stop_grace_secs: 15           # default 2: a heavy app gets longer to shut down
      max_output_bytes: 8388608     # default 8 MiB, per captured stream
      env:                          # on top of bddkit's own environment
        APP_ENV: test
        DATABASE_URL: ${TEST_DATABASE_URL}
    tools:
      cwd: /opt/tools
default_exec: app-host
```

| Key | Default | Notes |
|---|---|---|
| `cwd` | the feature file's workspace | A relative `cwd` is resolved against the workspace, so two feature files running in parallel never share a directory by accident. |
| `shell` | `sh`; `cmd` on Windows | Used for commands written as a string; the argv form uses no shell. How the string is handed over depends on the shell — see [Platforms](#platforms). |
| `timeout_secs` | `30` | A number of seconds, fractions allowed, at most 86400. Overridden per scenario by `the command timeout is "<seconds>" seconds`, under the same bound. |
| `stop_grace_secs` | `2` | How long a stopped process gets to shut down after the graceful request (SIGTERM; CTRL_BREAK on Windows) before it is killed — by `I stop`, at the end of a scenario and at the end of the file. Raise it for an application that takes long to shut down cleanly; `0` kills at once without asking. Fractions allowed, at most 86400. When a scenario ends with several processes running they share one grace period rather than waiting one after another. |
| `max_output_bytes` | `8388608` | Cap on each stream of each command and process. |
| `env` | none | A map; numbers and booleans are taken in their own spelling. |

Declaring any `resources.exec` section — even `exec: {}` — replaces the implicit instance: a suite that names its instances gets exactly the ones it named. Select one with `I use "<name>" exec`; the selection returns to the default at every scenario boundary, like the API does. Each instance keeps its own scenario state, so after a switch "the command" and every process name refer to what was run on the instance now selected. `bddkit doctor --live` probes each instance by starting its shell (in its `cwd`, when that is absolute).

## Steps

Every pattern carries a domain word — `command` or `process` — which keeps it clear of the host's steps and of anything another plugin is likely to pick.

| Step | Kind | Effect |
|---|---|---|
| `the command environment variable "<name>" is "<value>"` | action | set for every command and process started later in the scenario |
| `the command working directory is "<path>"` | action | relative to the instance `cwd`, for the rest of the scenario |
| `the command standard input is:` (doc string) | action | stdin of the next one-shot command only |
| `the command timeout is "<seconds>" seconds` | action | overrides `timeout_secs` for the rest of the scenario |
| `I run the command "<command>"` | action | `<shell> -c <command>`, waits for exit; publishes `exec_exit_code`, `exec_stdout`, `exec_stderr` |
| `I run the command "<program>" with arguments:` (table, header `argument`) | action | argv form, no shell, one row per argument |
| `I start the "<name>" process running "<command>"` | action | background; the name is unique within the scenario |
| `I start the "<name>" process running "<program>" with arguments:` | action | background, argv form |
| `I stop the "<name>" process` | action | a graceful request to the whole tree (SIGTERM; CTRL_BREAK on Windows), `stop_grace_secs` (default 2 s) to shut down, then a kill; the exit becomes readable |
| `the command exit code is <code>` / `is not <code>` | assertion | the last command |
| `the "<name>" process should be running` | assertion | |
| `the "<name>" process should have exited with code <code>` | assertion | `not_yet` while it runs, so it can be awaited |
| `… contains "<text>"` / `does not contain "<text>"` | assertion | plain substring |
| `… matches "<regex>"` / `does not match "<regex>"` | assertion | [`regex`](https://docs.rs/regex) syntax, unanchored |
| `… is empty` | assertion | |
| `… equals:` (doc string) | assertion | exact; one trailing newline of the stream is ignored, nothing else |
| `… has <n> lines` | assertion | a trailing newline does not start another line |
| `… contains JSON:` / `equals JSON:` / `does not contain JSON:` (doc string) | assertion | the host's semantics: `contains` is an object subset with order-independent arrays and extras allowed, `equals` is exact |
| `… as CSV contains:` / `as CSV equals:` (table), and the same with `TSV` | assertion | see below |
| `extract "<regex>" from … as "<name>"` | action | the first capture group of the first match, into a variable |
| `extract "<json path>" from … as JSON as "<name>"` | action | the host's path syntax, `data.items[0].id` |

`…` is one of four subjects, all written in the same step: `the command output`, `the command error output`, `the "<name>" process output`, `the "<name>" process error output`. "The command" is always the last one-shot command of the scenario.

A command, a text, a regex and an environment value may contain double quotes: the capture runs to the closing `"` at the end of the line, so `I run the command "grep "a b" app.log"` is one command. For anything longer, the argv form needs no quoting at all.

**CSV and TSV.** The stream's first line is its header; the table's header names the columns it cares about. `contains` means every table row appears in some stream row on those columns, in any order, with extra columns and rows allowed. `equals` means the same columns in the same order and the same rows in the same order. CSV is RFC 4180 (`"` quotes a cell, `""` is a literal quote, a quoted cell may span lines); TSV has no quoting — a tab separates, a newline ends a row.

**JSON** is the whole stream as one document; JSON Lines is not read line by line.

The `exec_*` variables are ordinary host variables, so `variable "exec_stdout" should be equal to …` and anything else the host does with a variable works on them. `exec_exit_code` is empty for a command killed by a signal (or, on Windows, terminated by the plugin). When a stream reached `max_output_bytes`, its variable holds what was captured up to the cap; only the stream assertions report the overflow.

## Streaming processes

A background process's output accumulates in per-stream buffers, and every assertion reads the buffer afresh. Every assertion may answer `not_yet`, so the host's eventual-assertion modifier is what makes a live stream testable — the host owns the loop, the interval and the timeout; the plugin never sleeps:

```gherkin
Given I start the "access" process running "tail -n0 -F /var/log/app/access.log"
And I start the "error" process running "tail -n0 -F /var/log/app/error.log"
When I request "/users?legacy=1"
Then I expect the next assertion to pass within "5" seconds
And the "access" process output contains "GET /users?legacy=1 200"
And I expect the next assertion to pass within "5" seconds
And the "error" process output matches "WARN .*legacy parameter"
And the "error" process output does not contain "ERROR"
```

Three things the plugin cannot decide for you:

- **`-n0` is yours, not the plugin's.** The plugin runs what is written. Without it, `does not contain "ERROR"` fails on yesterday's error and `contains "GET /users"` passes on the previous run's line.
- **A negative assertion on a live stream is a race.** `not_yet` cannot wait for an absence, and against an empty buffer `does not contain` passes instantly and proves nothing. Put it after a positive about the same event, as above, or after `I stop the "<name>" process`, when the buffer is final.
- **`I start` returns when the process is spawned, not when `tail` has opened the file.** The window is short but not zero. When it matters, prove the reader is reading before the event — write a probe line until it shows up, then stop the writer:

```gherkin
Given set variable "probe" to "probe-<<unique()>>"
And I start the "prober" process running "while :; do echo <<probe>> >> /var/log/app/access.log; sleep 0.1; done"
And I expect the next assertion to pass within "5" seconds
And the "access" process output contains "<<probe>>"
And I stop the "prober" process
```

(A single `echo` of the probe is not enough: with `-n0`, a probe written before `tail` opened the file is skipped, and the await then times out.)

## Semantics worth knowing

- **Processes live for one scenario.** The scenario boundary kills every process the scenario started, clears the context (environment, working directory, stdin, timeout) and forgets the last command; the end of the feature file does the same. Variables follow the host's own per-file rule.
- **…as long as `bddkit` itself gets to finish.** The cleanup runs through the host's end-of-scenario and end-of-file calls. If `bddkit` is killed by a signal mid-run — Ctrl-C, a CI job timeout — those calls never happen, and because every command runs in its own process group, the signal that reached `bddkit` does not reach them either. Find the leftovers with `ps -o pid,pgid,command` and end each group with `kill -- -<pgid>`. On Windows this does not happen: the Job Object is closed with the `bddkit` process, and closing it kills the tree. Cleaning up on an interrupted run is tracked in [bddkit/bddkit#62](https://github.com/bddkit/bddkit/issues/62) (the host side) and [#1](https://github.com/bddkit/bddkit-exec/issues/1) (stopping a command that is still running when the interruption comes).
- **A one-shot command owns its whole tree.** When it exits, anything it left running in its process group or job is killed: a `sleep 30 &` would otherwise hold the output pipes open and outlive the run. Use `I start` for anything that should keep running.
- **A non-zero exit is not a failure by itself**, exactly as an HTTP 500 is not — it is asserted. **A timeout is a failure**, exactly as a transport error is, and kills the whole tree.
- **A mismatch waits only while the stream can still change.** On a running process it answers `not_yet`, so an armed eventual assertion looks again; on the last command, or on a process that has ended with both pipes closed, it fails at once instead of spending the whole timeout.
- **A process killed by a signal has no exit code.** `the "<name>" process should have exited with code …` on it fails naming the signal — which is the usual outcome of `I stop` for a process that does not handle SIGTERM. A process that traps SIGTERM and exits cleanly reports its own code. On Windows the same holds for a process the plugin terminated.
- **Reaching `max_output_bytes` does not kill the process.** The reader keeps draining the pipe so the process never blocks, and the next assertion on that stream fails naming the cap — the failure says what happened instead of an assertion passing or failing on a silently truncated stream.
- **Output is decoded as UTF-8 with lossy replacement.** Binary output is not a use case.
- **`<<null>>` is refused** in a command, an argument, an environment variable and a path: a NUL byte cannot cross `exec`.
- **Standard input is sent as lines**: the doc string plus a final newline, so `read` in a shell sees the last line.
- **`I stop` on a process that already exited is not an error**; its exit was already recorded. Two `I start` with one name in a scenario is.

## Platforms

The plugin has one adapter per OS family (`src/exec/unix.rs`, `src/exec/windows.rs`); everything else — the steps, the buffers, the evidence — is shared. What a tester sees differ:

| | Linux, macOS | Windows |
|---|---|---|
| Process tree | a process group | a Job Object; the child starts suspended and is placed in the job before it runs, so nothing it starts escapes |
| `I stop` | SIGTERM to the group, `stop_grace_secs`, SIGKILL | CTRL_BREAK to the child's console process group, `stop_grace_secs`, `TerminateJobObject`. CTRL_BREAK needs a console shared with `bddkit`; without one (a service, some CI agents) the grace is skipped |
| A stop the process did not survive | `killed by SIGTERM (15)` | `terminated by bddkit-exec`, or `exit code 0xC000013A` when the program ended on the CTRL_BREAK itself |
| `bddkit` itself killed mid-run | the commands survive — see above | the commands die with it |
| Default `shell` | `sh`: `sh -c <command>` | `cmd`: `cmd /S /C "<command>"`, passed as written — cmd.exe parses its own command line |
| Other shells | `<shell> -c <command>` | `powershell` / `pwsh`: `-NoProfile -Command <command>`; anything else (Git Bash, MSYS2, Cygwin): `-c <command>` |

A command string is written in its shell's language, so a suite that must run on every platform either uses the argv form, which needs no shell, or sets `shell` to the same shell everywhere — Git Bash on Windows makes `sh` syntax portable:

```yaml
resources:
  exec:
    local:
      shell: ${EXEC_SHELL:-sh}   # EXEC_SHELL="C:/Program Files/Git/bin/bash.exe" on Windows
```

That is how this repository's own examples and end-to-end tests run on all three. `cmd.exe` ends its lines with `\r\n`; `equals:` ignores one trailing line ending of either kind, and `has <n> lines` counts either.

## Evidence and debug

A failed step carries the command (the shell string, quoted ready to paste, or the argv one element per line), the working directory, the environment the instance and the scenario added, the exit, stdout and stderr. A stream above 16 KiB is written to the step's `artifacts_dir` and attached by path — on a final failure only. A `not_yet` attempt keeps the last 16 KiB inline and writes nothing, since an eventual assertion can make hundreds of attempts and only the last is ever shown. Instance environment variables are listed by name only — their values come from the config, which is where `${SECRET}` expansions live; scenario variables are shown with their values, since the feature file wrote them.

Under `I am in debug mode` the command line and its exit are traced to stderr; as with the host's own debug steps, that interleaves under `concurrency > 1`.

## Running the examples and the tests

`examples/README.md` describes the demo suite. `cargo test --lib --test exports` needs nothing but the platform's own shell; `cargo test --test e2e` also needs a `bddkit` binary (`BDDKIT_BIN`, `PATH`, or a sibling `../bddkit` build) and skips itself, saying why, without one.

## Not in v1

- A `runner` field for ssh or `docker exec` (the name `exec` was chosen to survive it).
- Writing to a running process's stdin, a pty, `I send signal`.
- JSON Lines, a CSV cell into a variable, the host's `@variableType`-style matchers inside `contains JSON:`.
