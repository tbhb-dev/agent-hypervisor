//! Pure agent state decisions from ordered hook reports and optional screen observations.

use std::time::Duration;

/// The harness that produced a hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
    /// agy (Gemini CLI).
    Agy,
}

/// A field-free hook kind. Prompt and tool input never enter the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookKind {
    SessionStart,
    UserPromptSubmit,
    MessageDisplay,
    PreToolUse,
    PostToolUse,
    PermissionRequest,
    NotificationPermission,
    Stop { fully_idle: bool },
    Interrupt,
    SessionEnd,
    PreInvocation,
    PostInvocation,
    Other,
}

impl HookKind {
    /// Select only state-bearing fields from a harness hook payload.
    #[must_use]
    pub fn from_fields(
        harness: Harness,
        name: &str,
        notification_type: Option<&str>,
        fully_idle: bool,
    ) -> Self {
        match (harness, name) {
            (_, "SessionStart") => Self::SessionStart,
            (_, "SessionEnd") => Self::SessionEnd,
            (_, "PreToolUse") => Self::PreToolUse,
            (_, "PostToolUse") => Self::PostToolUse,
            (_, "Stop") => Self::Stop { fully_idle },
            (Harness::Claude | Harness::Codex, "UserPromptSubmit") => Self::UserPromptSubmit,
            (Harness::Claude, "MessageDisplay") => Self::MessageDisplay,
            (Harness::Claude | Harness::Codex, "PermissionRequest") => Self::PermissionRequest,
            (Harness::Claude, "Notification") if notification_type == Some("permission_prompt") => {
                Self::NotificationPermission
            }
            (Harness::Codex, "Interrupt") => Self::Interrupt,
            (Harness::Agy, "PreInvocation") => Self::PreInvocation,
            (Harness::Agy, "PostInvocation") => Self::PostInvocation,
            _ => Self::Other,
        }
    }
}

/// A report received from one session's socket. The listener, not a payload session ID,
/// determines which holder gets the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookReport {
    pub harness: Harness,
    pub kind: HookKind,
    /// Strictly increasing within the source, including across client restarts.
    pub seq: u64,
    /// Monotonic receive time supplied by the shell.
    pub at: Duration,
}

/// Why the agent is waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockReason {
    Approval,
    Input,
    Unknown,
}

/// Observable agent state, distinct from the holder's lifecycle phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Unknown,
    Idle,
    Working,
    Blocked { reason: BlockReason },
    Exited,
}

/// agy's `PreToolUse` precedes the approval screen by about 140 ms in run 5. A 500 ms
/// deadline admits that screen transition without leaving an unanswered tool call working.
pub const AGY_BLOCK_TIMEOUT: Duration = Duration::from_millis(500);

/// One source's ordered reports and the last state inferred from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateMachine {
    state: AgentState,
    last_seq: Option<u64>,
    pending_agy_tool: Option<Duration>,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self {
            state: AgentState::Unknown,
            last_seq: None,
            pending_agy_tool: None,
        }
    }
}

impl StateMachine {
    #[must_use]
    pub const fn state(&self) -> AgentState {
        self.state
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<Duration> {
        self.pending_agy_tool
    }

    /// Apply one ordered hook. Returns the new state only on change.
    pub fn report(&mut self, report: HookReport) -> Option<AgentState> {
        if self.state == AgentState::Exited || self.last_seq.is_some_and(|seq| report.seq <= seq) {
            return None;
        }
        self.last_seq = Some(report.seq);
        // Any subsequent event cancels an outstanding agy PreToolUse timeout.
        self.pending_agy_tool = None;
        let next = match (report.harness, report.kind) {
            // Claude sends SessionEnd on /clear and /resume while the child keeps running.
            // Only the holder's child exit is terminal.
            (_, HookKind::SessionEnd) => AgentState::Unknown,
            (Harness::Claude, HookKind::SessionStart)
            | (Harness::Claude | Harness::Codex, HookKind::Stop { .. })
            | (Harness::Codex, HookKind::Interrupt)
            | (Harness::Agy, HookKind::Stop { fully_idle: true }) => AgentState::Idle,
            (
                Harness::Claude | Harness::Codex,
                HookKind::UserPromptSubmit | HookKind::PreToolUse,
            )
            | (Harness::Claude, HookKind::MessageDisplay)
            | (Harness::Agy, HookKind::PreInvocation | HookKind::PostInvocation) => {
                AgentState::Working
            }
            (Harness::Claude | Harness::Codex, HookKind::PermissionRequest)
            | (Harness::Claude, HookKind::NotificationPermission) => AgentState::Blocked {
                reason: BlockReason::Approval,
            },
            (Harness::Agy, HookKind::PreToolUse) => {
                self.pending_agy_tool = Some(report.at + AGY_BLOCK_TIMEOUT);
                AgentState::Working
            }
            _ => self.state,
        };
        self.change(next)
    }

    /// Fire agy's pending approval timeout using the holder's monotonic time.
    pub fn tick(&mut self, now: Duration) -> Option<AgentState> {
        if self.pending_agy_tool.is_some_and(|at| now >= at) && self.state != AgentState::Exited {
            self.pending_agy_tool = None;
            return self.change(AgentState::Blocked {
                reason: BlockReason::Unknown,
            });
        }
        None
    }

    /// The holder's exit is authoritative even if hooks went silent.
    pub fn exit(&mut self) -> Option<AgentState> {
        self.pending_agy_tool = None;
        self.change(AgentState::Exited)
    }

    /// An enabled screen rule can correct a stale, non-authoritative hook state.
    pub fn screen(&mut self, state: AgentState) -> Option<AgentState> {
        if self.state == AgentState::Exited || state == AgentState::Exited {
            return None;
        }
        self.pending_agy_tool = None;
        self.change(state)
    }

    fn change(&mut self, next: AgentState) -> Option<AgentState> {
        if self.state == next {
            None
        } else {
            self.state = next;
            Some(next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn report(harness: Harness, kind: HookKind, seq: u64, ms: u64) -> HookReport {
        HookReport {
            harness,
            kind,
            seq,
            at: Duration::from_millis(ms),
        }
    }

    #[test]
    fn codex_start_is_not_launch_idle() {
        let mut s = StateMachine::default();
        assert_eq!(
            s.report(report(Harness::Codex, HookKind::SessionStart, 1, 0)),
            None
        );
        assert_eq!(s.state(), AgentState::Unknown);
    }

    #[test]
    fn agy_tool_blocks_only_after_unanswered_timeout() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Agy, HookKind::PreToolUse, 1, 0));
        assert_eq!(s.tick(Duration::from_millis(499)), None);
        assert_eq!(
            s.tick(Duration::from_millis(500)),
            Some(AgentState::Blocked {
                reason: BlockReason::Unknown
            })
        );
        s.report(report(
            Harness::Agy,
            HookKind::Stop { fully_idle: true },
            2,
            600,
        ));
        assert_eq!(s.state(), AgentState::Idle);
    }

    #[test]
    fn later_agy_event_cancels_timeout() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Agy, HookKind::PreToolUse, 1, 0));
        s.report(report(Harness::Agy, HookKind::PostToolUse, 2, 100));
        assert_eq!(s.tick(Duration::from_secs(1)), None);
    }

    #[test]
    fn exit_and_sequence_are_final() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Claude, HookKind::UserPromptSubmit, 2, 0));
        assert_eq!(
            s.report(report(
                Harness::Claude,
                HookKind::Stop { fully_idle: true },
                2,
                1
            )),
            None
        );
        assert_eq!(s.exit(), Some(AgentState::Exited));
        assert_eq!(
            s.report(report(Harness::Claude, HookKind::UserPromptSubmit, 3, 2)),
            None
        );
    }

    #[test]
    fn visible_blocker_corrects_stale_working() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Codex, HookKind::UserPromptSubmit, 1, 0));
        assert_eq!(
            s.screen(AgentState::Blocked {
                reason: BlockReason::Approval
            }),
            Some(AgentState::Blocked {
                reason: BlockReason::Approval
            })
        );
    }

    #[test]
    fn late_subagent_and_post_tool_events_do_not_revive_working() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Claude, HookKind::UserPromptSubmit, 1, 0));
        s.report(report(
            Harness::Claude,
            HookKind::Stop { fully_idle: true },
            2,
            1,
        ));
        s.report(report(Harness::Claude, HookKind::Other, 3, 2)); // SubagentStop
        s.report(report(Harness::Claude, HookKind::PostToolUse, 4, 3));
        assert_eq!(s.state(), AgentState::Idle);
    }

    #[test]
    fn claude_clear_restarts_a_live_session() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Claude, HookKind::UserPromptSubmit, 1, 0));
        s.report(report(Harness::Claude, HookKind::SessionEnd, 2, 1));
        assert_eq!(s.state(), AgentState::Unknown);
        s.report(report(Harness::Claude, HookKind::SessionStart, 3, 2));
        assert_eq!(s.state(), AgentState::Idle);
        s.report(report(Harness::Claude, HookKind::UserPromptSubmit, 4, 3));
        assert_eq!(s.state(), AgentState::Working);
    }

    #[test]
    fn claude_ctrl_c_at_approval_has_no_hook() {
        let mut s = StateMachine::default();
        for (seq, kind) in [
            HookKind::UserPromptSubmit,
            HookKind::PreToolUse,
            HookKind::PermissionRequest,
        ]
        .into_iter()
        .enumerate()
        {
            s.report(report(Harness::Claude, kind, seq as u64 + 1, seq as u64));
        }
        s.tick(Duration::from_secs(60)); // Ctrl-C produced no hook in run 3.
        assert_eq!(
            s.state(),
            AgentState::Blocked {
                reason: BlockReason::Approval
            }
        );
    }

    #[test]
    fn agy_ctrl_c_at_approval_has_no_hook() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Agy, HookKind::PreInvocation, 1, 0));
        s.report(report(Harness::Agy, HookKind::PreToolUse, 2, 1));
        s.tick(Duration::from_millis(501));
        s.tick(Duration::from_secs(60)); // Ctrl-C produced no hook in run 5.
        assert_eq!(
            s.state(),
            AgentState::Blocked {
                reason: BlockReason::Unknown
            }
        );
    }

    #[test]
    fn silent_source_after_working_remains_stale() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Codex, HookKind::UserPromptSubmit, 1, 0));
        s.tick(Duration::from_secs(60));
        assert_eq!(s.state(), AgentState::Working);
    }

    #[test]
    fn notification_can_be_first_approval_signal() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Claude, HookKind::UserPromptSubmit, 1, 0));
        s.report(report(
            Harness::Claude,
            HookKind::NotificationPermission,
            2,
            1,
        ));
        assert_eq!(
            s.state(),
            AgentState::Blocked {
                reason: BlockReason::Approval
            }
        );
    }

    #[test]
    fn codex_interrupt_ends_a_turn() {
        let mut s = StateMachine::default();
        s.report(report(Harness::Codex, HookKind::UserPromptSubmit, 1, 0));
        s.report(report(Harness::Codex, HookKind::PermissionRequest, 2, 1));
        s.report(report(Harness::Codex, HookKind::Interrupt, 3, 2));
        assert_eq!(s.state(), AgentState::Idle);
    }

    proptest! {
        #[test]
        fn non_increasing_seq_never_changes_state(seq in any::<u64>(), lower in any::<u64>()) {
            let mut s = StateMachine::default();
            let first = seq.max(lower);
            s.report(report(Harness::Claude, HookKind::UserPromptSubmit, first, 0));
            prop_assert_eq!(s.report(report(Harness::Claude, HookKind::Stop { fully_idle: true }, lower.min(first), 1)), None);
            prop_assert_eq!(s.state(), AgentState::Working);
        }

        #[test]
        fn holder_exit_is_absorbing(seq in any::<u64>()) {
            let mut s = StateMachine::default();
            s.exit();
            s.report(report(Harness::Agy, HookKind::PreInvocation, seq, 0));
            prop_assert_eq!(s.state(), AgentState::Exited);
        }
    }
}
