//! Data-driven screen observations. The actor calls this only when detection is enabled.

use crate::emulator::Grid;
use crate::state::{AgentState, BlockReason, Harness};

/// The part of a terminal that a rule reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Title,
    BottomNonEmpty(usize),
}

/// A literal screen pattern, its state, and its precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub harness: Harness,
    pub region: Region,
    pub pattern: &'static str,
    pub state: AgentState,
    pub priority: u16,
}

/// A pure detector that turns a title and grid into an observation.
pub trait ScreenDetector {
    fn detect(&self, harness: Harness, title: Option<&str>, grid: &Grid) -> Option<AgentState>;
}

/// Rules taken from RFC-36 runs 3 and 5, with priorities after herdr's manifests.
pub const STARTER_RULES: &[Rule] = &[
    Rule {
        harness: Harness::Codex,
        region: Region::Title,
        pattern: "Action Required",
        state: AgentState::Blocked {
            reason: BlockReason::Approval,
        },
        priority: 1000,
    },
    Rule {
        harness: Harness::Codex,
        region: Region::BottomNonEmpty(12),
        pattern: "? for shortcuts",
        state: AgentState::Idle,
        priority: 100,
    },
    Rule {
        harness: Harness::Codex,
        region: Region::BottomNonEmpty(12),
        pattern: "Ask Codex to do anything",
        state: AgentState::Idle,
        priority: 100,
    },
    Rule {
        harness: Harness::Claude,
        region: Region::BottomNonEmpty(12),
        pattern: "Do you want to proceed?",
        state: AgentState::Blocked {
            reason: BlockReason::Approval,
        },
        priority: 1000,
    },
    Rule {
        harness: Harness::Claude,
        region: Region::BottomNonEmpty(12),
        pattern: "? for shortcuts",
        state: AgentState::Idle,
        priority: 100,
    },
    Rule {
        harness: Harness::Agy,
        region: Region::BottomNonEmpty(12),
        pattern: "Run this command?",
        state: AgentState::Blocked {
            reason: BlockReason::Approval,
        },
        priority: 1000,
    },
    Rule {
        harness: Harness::Agy,
        region: Region::BottomNonEmpty(12),
        pattern: "esc to cancel",
        state: AgentState::Working,
        priority: 500,
    },
    Rule {
        harness: Harness::Agy,
        region: Region::BottomNonEmpty(12),
        pattern: "? for shortcuts",
        state: AgentState::Idle,
        priority: 100,
    },
    Rule {
        harness: Harness::Claude,
        region: Region::BottomNonEmpty(12),
        pattern: "esc to interrupt",
        state: AgentState::Working,
        priority: 500,
    },
];

/// A detector over explicit rules. The default uses [`STARTER_RULES`].
pub struct RuleDetector<'a> {
    pub rules: &'a [Rule],
}

impl Default for RuleDetector<'static> {
    fn default() -> Self {
        Self {
            rules: STARTER_RULES,
        }
    }
}

impl ScreenDetector for RuleDetector<'_> {
    fn detect(&self, harness: Harness, title: Option<&str>, grid: &Grid) -> Option<AgentState> {
        let rows = grid.text_rows();
        self.rules
            .iter()
            .filter(|rule| rule.harness == harness)
            .filter(|rule| match rule.region {
                Region::Title => title.is_some_and(|title| title.contains(rule.pattern)),
                Region::BottomNonEmpty(count) => rows
                    .iter()
                    .rev()
                    .filter(|row| !row.is_empty())
                    .take(count)
                    .any(|row| row.contains(rule.pattern)),
            })
            .max_by_key(|rule| rule.priority)
            .map(|rule| rule.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{Cell, Cursor, Size};
    use proptest::prelude::*;

    fn grid(text: &str) -> Grid {
        let size = Size::new(40, 1).unwrap();
        let mut cells = vec![Cell::blank(); 40];
        for (i, character) in text.chars().enumerate() {
            cells[i].text = character.to_string();
        }
        Grid::new(size, cells, Cursor::default()).unwrap()
    }

    #[test]
    fn priority_keeps_approval_above_working_footer() {
        let rules = [
            Rule {
                harness: Harness::Agy,
                region: Region::BottomNonEmpty(1),
                pattern: "Run",
                state: AgentState::Working,
                priority: 1,
            },
            Rule {
                harness: Harness::Agy,
                region: Region::BottomNonEmpty(1),
                pattern: "Run",
                state: AgentState::Blocked {
                    reason: BlockReason::Approval,
                },
                priority: 2,
            },
        ];
        let detector = RuleDetector { rules: &rules };
        assert_eq!(
            detector.detect(Harness::Agy, None, &grid("Run this command?")),
            Some(AgentState::Blocked {
                reason: BlockReason::Approval
            })
        );
    }

    #[test]
    fn title_rule_requires_the_target_harness() {
        let detector = RuleDetector::default();
        let title = Some("[ ! ] Action Required | workspace");
        assert_eq!(
            detector.detect(Harness::Codex, title, &grid("")),
            Some(AgentState::Blocked {
                reason: BlockReason::Approval
            })
        );
        assert_eq!(detector.detect(Harness::Claude, title, &grid("")), None);
    }

    proptest! {
        #[test]
        fn higher_priority_matching_rule_takes_precedence(lower in 0u16..1000) {
            let rules = [
                Rule { harness: Harness::Agy, region: Region::BottomNonEmpty(1), pattern: "Run", state: AgentState::Working, priority: lower },
                Rule { harness: Harness::Agy, region: Region::BottomNonEmpty(1), pattern: "Run", state: AgentState::Blocked { reason: BlockReason::Approval }, priority: 1000 },
            ];
            let detector = RuleDetector { rules: &rules };
            prop_assert_eq!(detector.detect(Harness::Agy, None, &grid("Run this command?")), Some(AgentState::Blocked { reason: BlockReason::Approval }));
        }
    }
}
