//! Shared verdict-line protocol for the orchestration reviewer, goal evaluator,
//! and arena judge.
//!
//! All three ask a model to end its reply with a marker line (`VERDICT:
//! APPROVED`, `GOAL_MET`, `RANKING: ...`). The rules are the same everywhere:
//! the token is exact-matched (so `NOT APPROVED` does not count as `APPROVED`),
//! and a missing or malformed verdict earns exactly one bounded reprompt before
//! falling back to no-verdict. When a reply carries several verdict-marker lines
//! that resolve to *conflicting* tokens (a real `VERDICT: REVISE` plus a quoted
//! `VERDICT: APPROVED` in the defect list), it is treated as malformed so the
//! reprompt — which asks for only the verdict line — can disambiguate, rather
//! than letting either position silently win. Duplicate lines that agree on one
//! token still parse.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FinalToken {
    Parsed(String),
    Missing,
    Malformed(String),
}

/// Scan `text` from the end and return the last line (in reading order) for
/// which `f` yields a value, paired with its line index.
pub(super) fn last_line_match<'a, T>(
    text: &'a str,
    mut f: impl FnMut(&'a str) -> Option<T>,
) -> Option<(usize, T)> {
    text.lines()
        .collect::<Vec<_>>()
        .into_iter()
        .enumerate()
        .rev()
        .find_map(|(index, line)| f(line).map(|value| (index, value)))
}

/// First whitespace-delimited word of `s`, stripped of surrounding punctuation
/// (keeping `_` so tokens like `GOAL_MET` survive intact).
pub(super) fn first_token(s: &str) -> &str {
    s.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
}

fn canonical_match(token: &str, allowed: &[&str]) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    allowed
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(token))
        .map(|candidate| (*candidate).to_string())
}

/// Parse a model's final verdict token. Collects every line containing `marker`
/// and, for each, exact-matches (case-insensitively) the first token after the
/// marker against `allowed`. If the resolving lines agree on a single token it
/// is [`FinalToken::Parsed`]; if two or more resolve to conflicting tokens the
/// reply is [`FinalToken::Malformed`] (so a quoted opposite verdict cannot win);
/// if no line resolves the last marker line is reported as malformed. Text
/// before the marker on a line, and marker lines that resolve to nothing, are
/// ignored unless no line resolves at all.
pub(super) fn parse_final_token(text: &str, marker: &str, allowed: &[&str]) -> FinalToken {
    let marker_lines: Vec<&str> = text.lines().filter(|line| line.contains(marker)).collect();
    let Some(last_line) = marker_lines.last() else {
        return FinalToken::Missing;
    };
    let resolved: Vec<String> = marker_lines
        .iter()
        .filter_map(|line| line.split_once(marker))
        .filter_map(|(_, after)| canonical_match(first_token(after), allowed))
        .collect();
    let distinct: std::collections::BTreeSet<String> = resolved
        .iter()
        .map(|token| token.to_ascii_uppercase())
        .collect();
    match distinct.len() {
        1 => FinalToken::Parsed(resolved[0].clone()),
        _ => FinalToken::Malformed(last_line.trim().to_string()),
    }
}

const REVIEW_MARKER: &str = "VERDICT:";
const REVIEW_APPROVED: &str = "APPROVED";
const REVIEW_REVISE: &str = "REVISE";

/// The one-line reprompt sent to a reviewer that omitted or malformed its
/// verdict line.
pub(super) const REVIEW_REPROMPT: &str =
    "Output ONLY the verdict line and nothing else: `VERDICT: APPROVED` or `VERDICT: REVISE`.";

/// The one-line reprompt sent to a goal evaluator that omitted its verdict line.
pub(super) const GOAL_REPROMPT: &str =
    "Output ONLY the verdict line and nothing else, starting with `GOAL_MET` or `GOAL_NOT_MET`.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewVerdict {
    Approved,
    Revise,
    NoVerdict,
}

impl ReviewVerdict {
    pub(super) fn approved(self) -> bool {
        matches!(self, ReviewVerdict::Approved)
    }

    pub(super) fn ledger_str(self) -> &'static str {
        match self {
            ReviewVerdict::Approved => "APPROVED",
            ReviewVerdict::Revise => "REVISE",
            ReviewVerdict::NoVerdict => "NO_VERDICT",
        }
    }
}

/// Map a parsed verdict token to a review verdict. Returns `None` when the token
/// is missing or malformed, i.e. when the reviewer should be reprompted.
pub(super) fn review_verdict_from_token(token: &FinalToken) -> Option<ReviewVerdict> {
    match token {
        FinalToken::Parsed(value) if value.eq_ignore_ascii_case(REVIEW_APPROVED) => {
            Some(ReviewVerdict::Approved)
        }
        FinalToken::Parsed(_) => Some(ReviewVerdict::Revise),
        FinalToken::Missing | FinalToken::Malformed(_) => None,
    }
}

/// Parse a reviewer reply into a verdict, or `None` when a reprompt is needed.
pub(super) fn parse_review_verdict(text: &str) -> Option<ReviewVerdict> {
    review_verdict_from_token(&parse_final_token(
        text,
        REVIEW_MARKER,
        &[REVIEW_APPROVED, REVIEW_REVISE],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_approved_does_not_parse_as_approved() {
        assert_eq!(
            parse_final_token(
                "VERDICT: NOT APPROVED",
                REVIEW_MARKER,
                &[REVIEW_APPROVED, REVIEW_REVISE]
            ),
            FinalToken::Malformed("VERDICT: NOT APPROVED".to_string())
        );
        assert_eq!(parse_review_verdict("VERDICT: NOT APPROVED"), None);
    }

    #[test]
    fn conflicting_verdict_lines_are_malformed_and_reprompt() {
        // A real REVISE verdict followed by a quoted APPROVED in the defect list
        // must not silently approve; the ambiguity triggers the reprompt.
        let text =
            "VERDICT: REVISE\n1. Fix the null deref.\n2. Once fixed I would issue VERDICT: APPROVED.";
        assert_eq!(parse_review_verdict(text), None);

        // The mirror image (rubric echo mentions both tokens) is equally ambiguous.
        let echo = "I must reply with VERDICT: APPROVED or VERDICT: REVISE.\n\nVERDICT: REVISE\n1. Fix the null deref.";
        assert_eq!(parse_review_verdict(echo), None);
    }

    #[test]
    fn duplicate_same_token_verdict_lines_parse() {
        let text = "The rubric says VERDICT: APPROVED or VERDICT: APPROVED.\n\nVERDICT: APPROVED";
        assert_eq!(parse_review_verdict(text), Some(ReviewVerdict::Approved));
    }

    #[test]
    fn marker_may_appear_mid_line_after_tool_text() {
        assert_eq!(
            parse_review_verdict(
                "I'll verify the file state first.VERDICT: APPROVED\n\nDetails follow."
            ),
            Some(ReviewVerdict::Approved)
        );
        assert_eq!(
            parse_review_verdict("Checking the diff now.VERDICT: REVISE\n1. Fix foo."),
            Some(ReviewVerdict::Revise)
        );
    }

    #[test]
    fn text_before_marker_on_same_line_is_ignored() {
        assert_eq!(
            parse_review_verdict("The plan said APPROVED is expected.VERDICT: REVISE\n1. Fix foo."),
            Some(ReviewVerdict::Revise)
        );
    }

    #[test]
    fn missing_verdict_reprompts() {
        assert_eq!(
            parse_final_token(
                "no verdict at all",
                REVIEW_MARKER,
                &[REVIEW_APPROVED, REVIEW_REVISE]
            ),
            FinalToken::Missing
        );
        assert_eq!(parse_review_verdict("no verdict at all"), None);
    }

    #[test]
    fn review_verdict_from_token_maps_tokens_and_gaps() {
        assert_eq!(
            review_verdict_from_token(&FinalToken::Parsed("APPROVED".to_string())),
            Some(ReviewVerdict::Approved)
        );
        assert_eq!(
            review_verdict_from_token(&FinalToken::Parsed("REVISE".to_string())),
            Some(ReviewVerdict::Revise)
        );
        assert_eq!(review_verdict_from_token(&FinalToken::Missing), None);
        assert_eq!(
            review_verdict_from_token(&FinalToken::Malformed("VERDICT: MAYBE".to_string())),
            None
        );
    }

    #[test]
    fn first_token_strips_surrounding_punctuation_keeping_underscores() {
        assert_eq!(first_token("  **APPROVED** — all good"), "APPROVED");
        assert_eq!(
            first_token("`GOAL_NOT_MET`: missing coverage"),
            "GOAL_NOT_MET"
        );
        assert_eq!(first_token(""), "");
    }

    #[test]
    fn last_line_match_returns_last_matching_line_with_index() {
        let text = "first\nGOAL_MET here\nnoise\nGOAL_NOT_MET there";
        let hit = last_line_match(text, |line| {
            line.starts_with("GOAL").then(|| line.to_string())
        });
        assert_eq!(hit, Some((3, "GOAL_NOT_MET there".to_string())));
    }
}
