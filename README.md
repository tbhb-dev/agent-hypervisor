# agent-hypervisor

Agent hypervisor prototypes: session host, attach, egress broker (RFC-36 to RFC-41)

Workers start with [`AGENTS.md`](AGENTS.md), which covers the workflow, the crate layout, the functional core rule, and the checks. The protocol specs these prototypes exist to produce live in [`specs/`](specs/), one file per spec, each a stub until its phase drafts it.

`terminal-debug` is a throwaway local frame inspector and raw input replayer for run 16, tracked for removal or replacement in [#78](https://github.com/tbhb-dev/agent-hypervisor/issues/78). It is not a terminal renderer.
