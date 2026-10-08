//! Commit message trailer rules.
//!
//! A message must carry no attribution trailer and must carry a `Refs:` trailer, either
//! `Refs: #<n>` for an agent-hypervisor issue or `Refs: RFC-<n>/<run>` for a vault run, where the
//! run may end in a round letter (`RFC-36/7b`). A message whose subject is `wip` is exempt.

use std::fmt;

const ATTRIBUTION_PREFIXES: [&str; 7] = [
    "assisted-by",
    "co-authored-by",
    "generated-by",
    "generated with",
    "made with",
    "written-by",
    "authored-by",
];

const SCISSORS: &str = "# ------------------------ >8 ------------------------";

/// A rule the message breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The line that carries an attribution trailer.
    Attribution(String),
    /// No `Refs:` trailer in an accepted form.
    MissingRefs,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attribution(line) => {
                write!(f, "attribution trailer found; commits carry none: {line}")
            }
            Self::MissingRefs => write!(
                f,
                "missing a 'Refs: #<issue>' or 'Refs: RFC-<n>/<run>' trailer"
            ),
        }
    }
}

/// The outcome of checking one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The subject is `wip`, a throwaway snapshot that never reaches shared history.
    Exempt,
    /// The problems found; empty means the message passes.
    Checked(Vec<Problem>),
}

/// Checks a commit message as git hands it to the commit-msg hook.
#[must_use]
pub fn check(message: &str) -> Verdict {
    let lines: Vec<&str> = message
        .lines()
        .take_while(|line| *line != SCISSORS)
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.starts_with('#'))
        .collect();
    if lines.first().is_some_and(|subject| subject.trim() == "wip") {
        return Verdict::Exempt;
    }
    let mut problems: Vec<Problem> = lines
        .iter()
        .filter(|line| is_attribution(line))
        .map(|line| Problem::Attribution((*line).to_owned()))
        .collect();
    if !lines.iter().any(|line| is_refs(line)) {
        problems.push(Problem::MissingRefs);
    }
    Verdict::Checked(problems)
}

fn is_attribution(line: &str) -> bool {
    let start = line
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '>' | '*' | '-'))
        .to_lowercase();
    ATTRIBUTION_PREFIXES
        .iter()
        .any(|prefix| start.starts_with(prefix))
}

fn is_refs(line: &str) -> bool {
    let Some(value) = line.trim_end().strip_prefix("Refs: ") else {
        return false;
    };
    if let Some(issue) = value.strip_prefix('#') {
        return is_number(issue);
    }
    value
        .strip_prefix("RFC-")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(rfc, run)| {
            let digits = run.trim_end_matches(|c: char| c.is_ascii_lowercase());
            is_number(rfc) && is_number(digits)
        })
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{Problem, Verdict, check};
    use proptest::prelude::*;

    #[test]
    fn accepts_a_vault_run_trailer() {
        let message = "chore(repo): bootstrap\n\nBody.\n\nRefs: RFC-36/6\n";
        assert_eq!(check(message), Verdict::Checked(vec![]));
    }

    #[test]
    fn accepts_an_issue_trailer_and_a_round_letter() {
        assert_eq!(check("fix: x\n\nRefs: #12\n"), Verdict::Checked(vec![]));
        assert_eq!(
            check("fix: x\n\nRefs: RFC-36/7b\n"),
            Verdict::Checked(vec![])
        );
    }

    #[test]
    fn rejects_a_missing_trailer() {
        assert_eq!(
            check("fix: x\n\nBody only.\n"),
            Verdict::Checked(vec![Problem::MissingRefs])
        );
        assert_eq!(
            check("fix: x\n\nRefs: RFC-36\n"),
            Verdict::Checked(vec![Problem::MissingRefs])
        );
    }

    #[test]
    fn rejects_an_attribution_trailer() {
        let message = "fix: x\n\nRefs: #1\nCo-authored-by: Someone <a@b.c>\n";
        assert_eq!(
            check(message),
            Verdict::Checked(vec![Problem::Attribution(
                "Co-authored-by: Someone <a@b.c>".to_owned()
            )])
        );
    }

    #[test]
    fn ignores_comments_and_the_verbose_diff() {
        let message = "fix: x\n\nRefs: #1\n# Co-authored-by: in a comment\n# ------------------------ >8 ------------------------\n+Assisted-by: in the diff\n";
        assert_eq!(check(message), Verdict::Checked(vec![]));
    }

    #[test]
    fn wip_is_exempt() {
        assert_eq!(check("wip\n"), Verdict::Exempt);
    }

    proptest! {
        #[test]
        fn any_issue_or_run_number_is_accepted(issue in 0u32.., rfc in 0u32.., run in 0u32.., round in "[a-z]?") {
            prop_assert_eq!(check(&format!("fix: x\n\nRefs: #{issue}\n")), Verdict::Checked(vec![]));
            prop_assert_eq!(check(&format!("fix: x\n\nRefs: RFC-{rfc}/{run}{round}\n")), Verdict::Checked(vec![]));
        }

        #[test]
        fn an_attribution_line_is_always_reported(
            prefix in prop::sample::select(vec!["Assisted-by", "CO-AUTHORED-BY", "Generated with", "written-by"]),
            lead in "[ >*-]{0,3}",
            rest in "[ -~]{0,20}",
        ) {
            let line = format!("{lead}{prefix}{rest}");
            let verdict = check(&format!("fix: x\n\n{line}\nRefs: #1\n"));
            prop_assert_eq!(verdict, Verdict::Checked(vec![Problem::Attribution(line.trim_end_matches('\r').to_owned())]));
        }

        #[test]
        fn wip_subject_is_exempt_whatever_follows(body in "[ -~\n]{0,60}") {
            prop_assert_eq!(check(&format!("wip\n{body}")), Verdict::Exempt);
        }
    }
}
