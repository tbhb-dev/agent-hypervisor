# Emulator conformance fixtures

`recordings/` holds raw PTY output, and `expected/` holds the final screen each one produced, as JSON. The test target `tests/recordings.rs` replays every recording and compares the result against its JSON file and against `alacritty_terminal` 0.26.0.

## Provenance

The recordings and screens were copied unchanged from the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `75f46d5edb48711a9453f711d340d2d9dd6a6885`, under `wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/`. Each JSON file records its source path and the SHA-256 of the recording, and the test checks the recording's length against it.

| Fixtures | Source | Recorded by |
| --- | --- | --- |
| `claude-*` and `codex-*`, nine files | `r3-harness-capture/cells/<name>/output.raw` | RFC-36 run 3, with Claude Code 2.1.293 and Codex CLI 0.157.1 at 120 by 40, against a loopback fake model |
| `agy-*`, six files | `r5-agy-capture/cells/<name>/output.raw` | RFC-36 run 5, with Antigravity `agy` 1.2.12 at 120 by 40, against a loopback fake model |
| `synthetic-alt` and `synthetic-exit` | `r2-emulator/fixtures/alt.vt` and `exit.vt` | RFC-36 run 2, as a generated stream that exercises SGR attributes, colors, wide and combining characters, a flag emoji, and a scrolling region |

The `text` and `cursor` fields come from the `ghostty_text` and `cursor.ghostty` fields of the matching spike report (`r3-harness-capture/replay/<name>.json`, `r5-agy-capture/replay/<name>.json`, or `r2-emulator/report-alt.json` and `report-exit.json`). That spike crate read the grid with its own code at the same Ghostty commit. `alacritty_grid_diffs` is the spike's `grid_diff_count`. It is 30 for `synthetic-alt`, all caused by the flag emoji's width, and 0 for the rest.

## Size and content

The largest recording is 25,133 bytes, and the directory totals about 340 KB. The sessions used fake API keys and disposable homes. Runs 3 and 5 scanned them with gitleaks 8.30.1 and searched them for user names and home paths, and found neither. Run 7 repeated both checks on these copies with the same result. They still contain the disposable workspace path under `/private/tmp/rfc36-capture-*` and random Claude Code session IDs.

The recordings are marked `binary` in `.gitattributes`, and `prek.toml` excludes them from the whitespace hooks, so no tool rewrites their bytes.
