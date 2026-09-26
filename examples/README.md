# Running the examples

Three configs, run from the repository root — `bddkit` resolves `paths` against the working directory, not the config file's location. Both need `cargo build --release` first (the lock file in `examples/.bddkit/` points at `target/release/`) and a `bddkit` binary, 0.2.1 or later (`cargo install bddkit`). Nothing else: the commands under test are `sh`, `bash`, `tail`, `sort` and friends. On Windows, point `EXEC_SHELL` at Git Bash for the POSIX features (`EXEC_SHELL="C:/Program Files/Git/bin/bash.exe"`); `windows.yaml` needs nothing but Windows.

```bash
bddkit run --config examples/exec.yaml
bddkit run --config examples/zero-config.yaml
bddkit run --config examples/windows.yaml   # Windows only
```

`bddkit` looks for `.bddkit/` beside the config, so `examples/.bddkit/plugins.yaml` is found for both. To point at a debug build instead, put a `plugins.local.yaml` (gitignored) next to it with the same `name: exec` and another `path`.

| File | What it demonstrates |
|---|---|
| `exec.yaml` | Two instances: `local` (every default, plus an `env` map) and `tools` (`bash`, a shorter timeout, a smaller output cap), `local` the default |
| `features/commands.feature` | A non-zero exit asserted rather than failing, `equals:` with the trailing newline ignored, `exec_exit_code` read with the host's own variable step, a command carrying its own double quotes, the scenario's environment and working directory, standard input for one command only, the argv form, `extract` with a regex, a per-scenario timeout, `I use "tools" exec` |
| `features/structured.feature` | `contains JSON:` / `equals JSON:` / `does not contain JSON:` and `extract … as JSON`; CSV with RFC 4180 quoting, `contains` over a subset of columns in any order and `equals`; TSV, which does not quote, with `equals` and with `contains` finding one row by two of its columns |
| `features/processes.feature` | An eventual assertion re-reading a live process until it passes, awaiting an exit code, two `tail -F` readers and one event with the positive before the negative, `I stop` on a process that traps SIGTERM (and SIGINT, which is what CTRL_BREAK becomes under Git Bash) and the final buffer it leaves |
| `zero-config.yaml` + `zero-config/hello.feature` | No `resources.exec` at all: the plugin's implicit instance runs the command in the platform's default shell |
| `windows.yaml` + `windows/shells.feature` | Windows only: cmd.exe with its CRLF output and `%VAR%`, PowerShell selected by name, a background `ping` stopped with its tree, the argv form |

The feature files run in parallel (the host's default `concurrency` is 8) and need nothing from each other: every command runs in its own feature file's workspace directory, and every process a scenario starts is stopped when it ends (provided `bddkit` itself is not killed mid-run — see the main README).

## Seeing a failure dump

```bash
bddkit run --config tests/exec.yaml --bddkit-dir examples/.bddkit tests/features/failing.feature
```

`tests/` has no `.bddkit/` of its own, so `--bddkit-dir` borrows the examples' lock file.

Six scenarios that each fail in their own way — a timeout, a stream past its cap, `<<null>>` in a command, an eventual assertion that gives up, a wrong exit code, an unknown process — kept in `tests/` to prove the evidence rather than as part of the demo. Each failure prints the command ready to paste, the working directory, the environment the config and the scenario added, the exit, stdout and stderr.
