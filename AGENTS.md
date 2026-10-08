# Worker instructions

The code here prototypes the agent hypervisor from RFC-36 to RFC-41. The hypervisor is a host daemon that keeps agent and shell terminal sessions alive and lets clients attach to them. The prototypes exist to discover the protocols. Each phase ends with a spec fragment under [`specs/`](specs/), and throwaway code is fine where it reaches the spec faster. Read your run's brief in the coordinator's vault before working, and keep its acceptance criteria and scope in view.

## Workflow

- Work arrives as an RFC-36 run (`RFC-36/<run>`, with an optional round letter such as `RFC-36/7b`) or, later, as an issue in this repository.
- Work on `<type>/rfc36-r<N>-<slug>` in your own `.worktrees/<type>-rfc36-r<N>-<slug>/` checkout inside this clone. Run 6 used `chore/rfc36-r6-bootstrap` in `.worktrees/chore-rfc36-r6-bootstrap/`. Use the branch the coordinator assigns. Never work on `main` or share a checkout with another worker.
- Commit as `tbhb-agent <agent@tonyburns.net>` with `type(scope): imperative subject`, an explanatory body, and one `Refs:` trailer. Use `Refs: RFC-<n>/<run>` for a vault run and `Refs: #<n>` for an issue here. Use Conventional Commits. Do not add attribution or co-author trailers to commits or PR bodies. The commit-msg hook enforces both trailer rules, except on the subject `wip`.
- Keep work committed and push before reporting completion. Push without force: `GH_TOKEN="$(gh auth token --user tbhb-agent)" git push -u origin <branch>`. Scan evidence for secrets and hold back sensitive material before committing it.
- Open one small PR per run with `/Users/tony/Code/github.com/tbhb/agent-orchestration-poc/.holding/bin/gh-as-agent pr create --repo tbhb-dev/agent-hypervisor`. Use the conventional subject as its title. The body has what, why, evidence, docs, and checklist sections and ends with the `Refs:` trailer. The checklist covers green CI, no secrets, docs updated, and evidence linked.
- `tbhb-agent-reviewer` reviews through `/Users/tony/Code/github.com/tbhb/agent-orchestration-poc/.holding/bin/gh-as-reviewer` and posts request-changes and approval verdicts as PR reviews with inline threads. Implementers reply in threads and push fixes without force pushing. A different harness reviews first where practical.
- Workers never merge. The coordinator squash-merges after a current approval and a green `check`. The PR title and body become the squash commit.
- The `main` ruleset requires a PR, one approval of the latest push from someone other than its pusher, and squash merges. A second ruleset blocks force pushes on every branch. Only the operator changes rulesets, required checks, settings, and collaborators.
- Use `gh query` for read-only GitHub API calls. Reserve `gh api` for mutations.

## Crates and naming

- Put every crate at `crates/<name>/`, with a directory name equal to the package name.
- A crate whose name ends in `-core` is pure. Every other crate is a shell crate.
- Product crates: `hypervisor-core` holds the hypervisor's decisions, and `hypervisord` is the host daemon binary. New product crates take a `hypervisor-` prefix, or the binary's own name for a binary.
- Emulator crates: `hypervisor-ghostty` puts the core's `Emulator` trait on ghostty-vt, and `ghostty-vt-sys` is its raw FFI, named by the Rust `-sys` convention. The `-sys` crate builds `libghostty-vt.a` with Zig from the commit in `crates/ghostty-vt-sys/ghostty.pin`; its committed `src/bindings.rs` is regenerated with `mise run ghostty:bindings`, never edited.
- Repository tooling: `xtask-core` holds the pure decisions behind the repository checks, and `xtask` is their shell, run with `cargo run -p xtask -- <command>`.
- The workspace sets edition 2024, `resolver = "3"`, and shared lints in the root `Cargo.toml`. Every crate inherits them with `[lints] workspace = true` and takes its version, edition, and license from `[workspace.package]`. Commit `Cargo.lock`.

## Functional core, imperative shell

- Functional core, imperative shell is a binding operator requirement, carried over from agent-orchestration-poc. Put decisions and data transformations in pure functions in `-core` crates. Put side effects in thin shell crates at the edges. A PR that puts I/O in the core or decisions in the shell cannot merge.
- Every `-core` crate starts its `lib.rs` with `#![forbid(unsafe_code, clippy::disallowed_methods, clippy::disallowed_types, clippy::disallowed_macros)]`, so an `allow` for any of them fails to compile. It has a `clippy.toml` beside its `Cargo.toml` that disallows file, network, process, environment, clock, thread, and standard input and output calls and types, plus the print macros. Copy the existing `crates/hypervisor-core/clippy.toml` unchanged. Clippy reads the `clippy.toml` beside a crate's manifest, so it applies to that crate only.
- `mise run check:boundary` fails when a `-core` crate has a normal dependency that is neither another `-core` crate nor on `[workspace.metadata.boundary] allow` in the root `Cargo.toml`, or when a `-core` crate's `clippy.toml` is missing or lacks a ban in `xtask_core::boundary::REQUIRED_BANS`, or when its crate root lacks that `forbid` attribute. Add a new ban to `REQUIRED_BANS` and every core `clippy.toml` in the same PR. Add a crate to the allowlist only when it doesn't do I/O, and say why in the PR.
- Fix a disallowed-method finding by moving the call into a shell crate and passing the value in. The `forbid` attribute makes suppressing `clippy::disallowed_methods`, `disallowed_types`, or `disallowed_macros` in a core crate a compile error.
- Test the core with plain values and no mocks. Write a unit test for each case and a `proptest` property for each invariant. Put process, socket, and filesystem tests in shell crates.

## Mise and checks

Run project tools through mise tasks, never as bare tools or global installs. For an ad hoc invocation without a task, use `mise exec -- <tool>`, and add a task when the action becomes repeatable. Non-interactive shells may lack mise shims. Install toolchains only through mise or rustup at user level.

- Run `mise trust`, `mise install`, `mise run vale:sync`, and `mise exec -- prek install` once in each fresh worktree.
- Run `mise run check` before completion. It runs `check:fmt`, `check:clippy`, `check:test`, `check:boundary`, `check:secrets`, `check:actions`, `check:guard-markdown`, `check:rumdl`, `check:vale`, `check:tombi`, and `check:ryl`.
- `check` reports the macOS launchd and seatbelt integration tests as not run. Before merging any PR, run `mise run check:host` on an unsandboxed macOS host at the PR's current head SHA. The reviewer and run brief must name that host and record the command, SHA, result, and fixture cleanup result. A green Linux `check` is not evidence for launchd behavior, and an unavailable host run remains a merge blocker.
- Run `mise run fmt` to apply `cargo fmt` and `tombi format`, then inspect the diff.
- Run `mise run build` to build every crate.
- CI runs one job, `check`, on Linux with the mise and cargo caches. It runs `mise run check` and nothing else. A local pass predicts the CI result.
- The host is shared and memory is tight. Set `CARGO_BUILD_JOBS=2` for local builds when other workers are running.
- Keep pins and their conventions in agreement. The pins match agent-orchestration-poc. Move one deliberately and update `.claude/rules/rust.md` in the same PR.

This repository has fewer checks than agent-orchestration-poc for now. Coverage floors and mutation testing wait until a spec is marked stable. A PR body check job, a macOS runner, and a release workflow wait until a run adds them.

## Worktrees, stashes, and work in progress

Every worktree shares one global `git stash` stack. An unqualified restore can take another worker's entry.

- Merge `main` into a pushed branch to update it. Rebase only unpublished history. Never force push a pushed branch.
- Prefer `git rebase --autostash` when rebasing dirty unpublished history.
- Never run bare `git stash pop` or `git stash apply`. If a manual stash is unavoidable, name it with `git stash push -m`, run `git stash list` immediately before restoring, match your own message, and pop the explicit `stash@{n}`.

Prefer a throwaway work-in-progress commit to preserve state across a rebase or branch switch.

```sh
git commit -am wip --no-verify
# Rebase unpublished history, switch, or perform the required operation.
git reset --soft HEAD~1
```

This incomplete snapshot is the only permitted `--no-verify` use. Remove it with the soft reset before it reaches shared history.

## Documentation and specs

- The worker that builds a phase writes its spec fragment under `specs/` in the same PR. The agent-orchestration-poc rule that only Codex writes documentation does not apply here unless the operator says so.
- Write Markdown with one line per paragraph and sentence case headings. `guard-markdown` checks paragraph wrapping, Vale with the `ai-tells` style checks prose and commit messages, and rumdl checks Markdown structure.
- A spec moves from `stub` to `draft` in the phase the spec table names, and to `stable` after the phase it names. Record what a phase disproved in the spec, not only in the PR.
- Cite vault findings as `tbhb-dev/agent-orchestration-poc.internal` paths at a commit. That repository is private and separate from this one.
- Code comments and doc comments stay with the coder.

## Evidence

Record the command, file, or source behind every claim in a PR, and label it:

| Label | Meaning |
| --- | --- |
| verified | A targeted check confirmed the stated claim at recorded versions. |
| observed | A run exhibited the behavior, within the recorded conditions. |
| help-text | CLI help advertises the option or behavior. |
| schema | A source or configuration schema defines it. |
| documented | Versioned documentation describes it. |
| inference | The conclusion follows from cited evidence but lacks a direct test. |
| untested | The proposed behavior still needs a test. |

Do not treat help text or schema support as proof of runtime behavior. Record versions and the limits of a test. Read dependency source and versioned documentation before relying on model knowledge, and record the commit or version read.

## Operator boundaries

- Sandbox escapes and system changes go to the operator with the exact proposed change. Report blocked operations instead of bypassing the sandbox. Host packages, Apple Containers setup, launch agents, Tailscale, and harness user settings need operator approval. Creating VMs and images needs none, but record CPU, memory, disk, and VM count for each.
- Run nothing that opens a macOS dialog, such as a Keychain access prompt, a privacy (TCC) consent, an Xcode or Apple ID sign-in, a 1Password Touch ID prompt, or a browser sign-in. If one appears, do not answer it. Record the command and what appeared, leave the dialog for the operator, and stop that step.
- Get credentials only through `ao-op-run`. Never print, log, or write a token. The only other credential use is the inline `gh auth token --user tbhb-agent` push form above. If a step needs a secret you do not have, stop and report it.
