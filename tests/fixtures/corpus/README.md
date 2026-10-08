# Recorded-session corpus

`recordings/` holds raw PTY output, and `expected/` contains one JSON file per recording with its metadata and the final screen it produced. Every crate and later conformance run reads this one copy. The golden test target `crates/hypervisor-ghostty/tests/recordings.rs` replays every recording through ghostty-vt and `alacritty_terminal` 0.26.0 and compares the grids with each other and with the JSON file. [The emulator spec](../../../specs/emulator.md) gives the test contract and the known differences.

## Metadata

Each `expected/<name>.json` file has these fields.

| Field | Meaning |
| --- | --- |
| `name` | The recording's name, and the JSON file's base name |
| `file` | The recording's path, relative to this directory |
| `bytes` and `sha256` | The recording's size in bytes and its SHA-256. The tests check both |
| `cols` and `rows` | The terminal size it was recorded at, and replayed at |
| `harness` and `harness_version` | The program recorded: `claude-code`, `codex`, `agy`, `vim`, `htop`, `cargo`, or `synthetic` for a generated stream |
| `screen_mode` | `alternate` when the session entered the alternate screen, `inline` when it never did |
| `screen` | The active screen at the end of the recording, `primary` or `alternate`. The test checks it |
| `queries_answered` | Whether the capture answered terminal queries while recording |
| `source` | The run that recorded it, and for an imported recording the repository, commit, and path it was copied from. A recording made in this repository has `null` for those three |
| `redactions` | Each byte range replaced with a same-length placeholder before committing, and why. Empty for every recording so far |
| `alacritty_grid_diffs` and `difference_cause` | The accepted count of cells where `alacritty_terminal` differs from ghostty-vt on the final screen, and its cause, or `null` when the count is 0 |
| `cursor` and `text` | ghostty-vt's final cursor, as row and column, and each row's text with trailing spaces trimmed |

## Provenance

| Recordings | Source | Recorded by |
| --- | --- | --- |
| `claude-*` and `codex-*`, nine files | `r3-harness-capture/cells/<name>/output.raw` | RFC-36 run 3, with Claude Code 2.1.293 and Codex CLI 0.157.1, against a loopback fake model |
| `agy-*`, six files | `r5-agy-capture/cells/<name>/output.raw` | RFC-36 run 5, with Antigravity `agy` 1.2.12, against a loopback fake model |
| `synthetic-alt` and `synthetic-exit` | `r2-emulator/fixtures/alt.vt` and `exit.vt` | RFC-36 run 2, as a generated stream that exercises SGR attributes, colors, wide and combining characters, a flag emoji, and a scrolling region |
| `shell-vim-answered`, `shell-htop-answered`, `shell-build-answered` | this repository | RFC-36 run 8, as described below |

Run 7 copied the first 17 recordings unchanged from the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `75f46d5edb48711a9453f711d340d2d9dd6a6885`, under `wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/`. Run 8 checked each one's SHA-256 against the findings tables at vault commit `485c37e54cd69d186a4b5eb0909f5bdee822fe1b`, and all 17 matched. Their `text` and `cursor` fields come from the spike reports beside those cells, which read the grid at the same Ghostty commit.

## Run 8 recordings

Run 8 recorded three shell sessions with run 3's `capture.py` (`r3-harness-capture/tool/capture.py`) at 120 by 40, with `TERM=xterm-256color` and queries answered. Each ran `/bin/bash --noprofile --norc` under `env -i`, in a disposable home under `/private/tmp/rfc36-capture-<cell>-*` that was deleted afterwards, with run 3's `security` stub first on `PATH`. No model, tmux, or credential was involved. The environment held `HOME`, `TMPDIR`, `XDG_CONFIG_HOME`, `PATH`, `TERM`, `LANG`, `PS1`, `HISTFILE`, and `BASH_SILENCE_DEPRECATION_WARNING`, and the build added `CARGO_HOME`, `CARGO_TARGET_DIR`, and `CARGO_BUILD_JOBS`.

- `shell-vim-answered`: `vim -u NONE -N notes.txt` on a two-line file, then appending three lines in insert mode, moving with `gg`, `w`, `jj`, `$`, and `b`, then `:split`, an edit in the lower window, and `:wqa` to quit. `/usr/bin/vim` 9.1, compiled Apr 18 2026.
- `shell-htop-answered`: `htop -p $$`, limited to the recording's own shell, with an `htoprc` whose only screen shows the PID, state, CPU, memory, and time columns, so no user name or command line is drawn. It quits with `q` after about 4.5 seconds at a one-second refresh. htop 3.5.2.
- `shell-build-answered`: a cold `cargo build --color=always` of a scratch crate depending on `clap`, `regex`, `serde`, and `serde_json`, with a disposable `CARGO_HOME` and target directory. It downloads and compiles 41 units, and the log scrolls past one screen. cargo 1.98.1.

## Size and content

The largest recording is 25,133 bytes, and the 20 recordings total 263,921 bytes. The harness sessions used fake API keys and disposable homes, and runs 3, 5, and 7 didn't find a user name, home path, or secret in them. Run 8 searched every recording for the operator's user name, home path, host name, email, and account names without a match, so it didn't redact anything. `gitleaks detect --no-git` 8.30.1 reported 0 leaks. The recordings still contain disposable paths under `/private/tmp/rfc36-capture-*`, random Claude Code session IDs, and the build's crate versions and timings.

The recordings are marked `binary` in `.gitattributes`, and `prek.toml` excludes them from the whitespace hooks, so no tool rewrites their bytes.
