use goose::config::Config;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const INDEX_FILE: &str = "exemplars.jsonl";

/// Config key holding the comma-separated substrings that mark a serving model
/// as frontier (uplift injection is redundant and auto-skipped for these).
const FRONTIER_PATTERNS_KEY: &str = "GOOSE_UPLIFT_FRONTIER_PATTERNS";
const DEFAULT_FRONTIER_PATTERN: &str = "fable";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InjectionMode {
    Always,
    Never,
    Auto,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(super) struct ExemplarInjection {
    pub(super) injected: bool,
    pub(super) selected_run_ids: Vec<String>,
    pub(super) prompt_section: Option<String>,
}

impl ExemplarInjection {
    pub(super) fn banner_fragment(&self) -> String {
        self.banner_fragment_with_label("exemplars")
    }

    pub(super) fn banner_fragment_with_label(&self, label: &str) -> String {
        if self.injected {
            format!(
                " · {} injected [{}]",
                label,
                self.selected_run_ids.join(", ")
            )
        } else {
            format!(" · {} skipped", label)
        }
    }
}

pub(super) trait SimilarityRecord: Clone {
    fn task(&self) -> &str;
    fn recency_ms(&self) -> u128;

    fn label(&self) -> Option<&str> {
        None
    }
}

pub(super) fn parse_injection_mode(raw: &str) -> InjectionMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "always" => InjectionMode::Always,
        "never" => InjectionMode::Never,
        _ => InjectionMode::Auto,
    }
}

pub(super) fn is_generic_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.is_empty() || model == "default" || model == "current" || model == "unknown"
}

/// Why a serving model is treated as frontier — i.e. why uplift injection is
/// auto-skipped for it. Carried so callers can print an honest skip reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FrontierMatch {
    /// The model string contains this configured frontier substring.
    Pattern(String),
    /// A `claude-acp` role with an unset/default model alias, which usually
    /// fronts the user's own frontier model.
    ClaudeAcpDefault,
}

/// Parse the comma-separated `GOOSE_UPLIFT_FRONTIER_PATTERNS` value into trimmed,
/// non-empty, lowercased substrings. Empty input yields no patterns.
pub(super) fn parse_frontier_patterns(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|pattern| pattern.trim().to_ascii_lowercase())
        .filter(|pattern| !pattern.is_empty())
        .collect()
}

/// The configured frontier patterns, defaulting to `["fable"]` when the knob is
/// unset or resolves to an empty list.
pub(super) fn frontier_patterns() -> Vec<String> {
    Config::global()
        .get_param::<String>(FRONTIER_PATTERNS_KEY)
        .ok()
        .map(|raw| parse_frontier_patterns(&raw))
        .filter(|patterns| !patterns.is_empty())
        .unwrap_or_else(|| vec![DEFAULT_FRONTIER_PATTERN.to_string()])
}

/// Decide whether a serving model is frontier given an explicit pattern list.
/// Pure and unit-testable; callers pass [`frontier_patterns`].
pub(super) fn frontier_match(
    provider_name: &str,
    model: &str,
    patterns: &[String],
) -> Option<FrontierMatch> {
    let model_lower = model.to_ascii_lowercase();
    if let Some(pattern) = patterns
        .iter()
        .find(|pattern| !pattern.is_empty() && model_lower.contains(pattern.as_str()))
    {
        return Some(FrontierMatch::Pattern(pattern.clone()));
    }
    if provider_name.trim().eq_ignore_ascii_case("claude-acp") && is_generic_model(model) {
        return Some(FrontierMatch::ClaudeAcpDefault);
    }
    None
}

pub(super) fn is_frontier_model(provider_name: &str, model: &str, patterns: &[String]) -> bool {
    frontier_match(provider_name, model, patterns).is_some()
}

pub(super) fn should_inject(provider_name: &str, model: &str, mode: InjectionMode) -> bool {
    match mode {
        InjectionMode::Always => true,
        InjectionMode::Never => false,
        InjectionMode::Auto => !is_frontier_model(provider_name, model, &frontier_patterns()),
    }
}

pub(super) fn archive_text_and_record<T: Serialize>(
    state_dir: &Path,
    exemplars_dir_name: &str,
    file_name: &str,
    text: &str,
    record: &T,
) -> bool {
    let dir = exemplars_dir(state_dir, exemplars_dir_name);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }

    if std::fs::write(dir.join(file_name), text).is_err() {
        return false;
    }

    let Ok(json) = serde_json::to_string(record) else {
        return false;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(index_path(state_dir, exemplars_dir_name))
    else {
        return false;
    };

    writeln!(file, "{json}").is_ok()
}

pub(super) fn read_index<T: DeserializeOwned>(
    state_dir: &Path,
    exemplars_dir_name: &str,
) -> Option<Vec<T>> {
    let content = match std::fs::read_to_string(index_path(state_dir, exemplars_dir_name)) {
        Ok(content) => content,
        Err(_) => return Some(Vec::new()),
    };

    let mut records = Vec::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<T>(line) else {
            return None;
        };
        records.push(record);
    }
    Some(records)
}

pub(super) fn select_similar_records<T: SimilarityRecord>(
    records: &[T],
    task: &str,
    k: usize,
) -> Vec<T> {
    scored_records(records, task)
        .into_iter()
        .take(k)
        .map(|(_, record)| record.clone())
        .collect()
}

pub(super) fn select_similar_records_by_label<T: SimilarityRecord>(
    records: &[T],
    task: &str,
    per_label: usize,
    labels: &[&str],
) -> Vec<T> {
    if per_label == 0 || labels.is_empty() {
        return Vec::new();
    }

    let scored = scored_records(records, task);
    let mut selected = Vec::new();
    for label in labels {
        selected.extend(
            scored
                .iter()
                .filter(|(_, record)| {
                    record
                        .label()
                        .is_some_and(|record_label| record_label.eq_ignore_ascii_case(label))
                })
                .take(per_label)
                .map(|(_, record)| (**record).clone()),
        );
    }
    selected
}

pub(super) fn artifact_path(
    state_dir: &Path,
    exemplars_dir_name: &str,
    file_name: &str,
) -> PathBuf {
    exemplars_dir(state_dir, exemplars_dir_name).join(file_name)
}

fn scored_records<'a, T: SimilarityRecord>(records: &'a [T], task: &str) -> Vec<(f64, &'a T)> {
    if records.is_empty() {
        return Vec::new();
    }

    let query_tokens = tokenize(task);
    if query_tokens.is_empty() {
        return Vec::new();
    }

    let mut scored = records
        .iter()
        .filter_map(|record| {
            let score = jaccard(&query_tokens, &tokenize(record.task()));
            (score > 0.0).then_some((score, record))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| right.recency_ms().cmp(&left.recency_ms()))
    });
    scored
}

fn exemplars_dir(state_dir: &Path, exemplars_dir_name: &str) -> PathBuf {
    state_dir.join(exemplars_dir_name)
}

fn index_path(state_dir: &Path, exemplars_dir_name: &str) -> PathBuf {
    exemplars_dir(state_dir, exemplars_dir_name).join(INDEX_FILE)
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let intersection = left.intersection(right).count();
    let union = left.union(right).count();
    intersection as f64 / union as f64
}

fn tokenize(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut ascii = String::new();
    let mut cjk = Vec::new();

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut tokens);
            ascii.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut tokens);
            cjk.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut tokens);
            flush_cjk(&mut cjk, &mut tokens);
        }
    }

    flush_ascii(&mut ascii, &mut tokens);
    flush_cjk(&mut cjk, &mut tokens);
    tokens
}

fn flush_ascii(ascii: &mut String, tokens: &mut HashSet<String>) {
    if !ascii.is_empty() {
        tokens.insert(std::mem::take(ascii));
    }
}

fn flush_cjk(cjk: &mut Vec<char>, tokens: &mut HashSet<String>) {
    match cjk.len() {
        0 => {}
        1 => {
            tokens.insert(cjk[0].to_string());
        }
        _ => {
            for pair in cjk.windows(2) {
                tokens.insert(pair.iter().collect());
            }
        }
    }
    cjk.clear();
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestRecord {
        run_id: &'static str,
        task: &'static str,
        seen_at_ms: u128,
        label: Option<&'static str>,
    }

    impl SimilarityRecord for TestRecord {
        fn task(&self) -> &str {
            self.task
        }

        fn recency_ms(&self) -> u128 {
            self.seen_at_ms
        }

        fn label(&self) -> Option<&str> {
            self.label
        }
    }

    fn default_patterns() -> Vec<String> {
        vec![DEFAULT_FRONTIER_PATTERN.to_string()]
    }

    #[test]
    fn frontier_identity_uses_model_not_transport() {
        let patterns = default_patterns();
        assert!(!is_frontier_model("claude-acp", "opus", &patterns));
        assert!(is_frontier_model("claude-acp", "default", &patterns));
        assert!(is_frontier_model("claude-acp", "claude-fable-5", &patterns));
        assert!(!is_frontier_model("codex-acp", "default", &patterns));
        assert!(!is_frontier_model("anthropic", "claude-opus", &patterns));
        assert!(is_frontier_model("anthropic", "claude-fable-5", &patterns));
        assert!(!is_frontier_model("openai", "gpt-5.5", &patterns));
    }

    #[test]
    fn frontier_match_reports_matched_pattern_and_alias_reason() {
        let patterns = default_patterns();
        assert_eq!(
            frontier_match("anthropic", "claude-fable-5", &patterns),
            Some(FrontierMatch::Pattern("fable".to_string()))
        );
        assert_eq!(
            frontier_match("claude-acp", "default", &patterns),
            Some(FrontierMatch::ClaudeAcpDefault)
        );
        assert_eq!(frontier_match("openai", "gpt-5.5", &patterns), None);
    }

    #[test]
    fn frontier_patterns_are_configurable() {
        // A claude-acp opus user is NOT frontier under the default "fable"
        // pattern (so it receives uplift), but becomes frontier once "opus" is
        // added to the pattern list.
        assert!(!is_frontier_model(
            "claude-acp",
            "opus",
            &parse_frontier_patterns("fable")
        ));
        assert!(is_frontier_model(
            "claude-acp",
            "opus",
            &parse_frontier_patterns("fable, opus")
        ));
        assert!(parse_frontier_patterns("  ,  ").is_empty());
        assert_eq!(
            parse_frontier_patterns("Fable, GPT-5"),
            vec!["fable".to_string(), "gpt-5".to_string()]
        );
    }

    #[test]
    fn should_inject_explicit_modes_override_model_identity() {
        assert!(should_inject(
            "claude-acp",
            "claude-fable-5",
            InjectionMode::Always
        ));
        assert!(!should_inject("claude-acp", "opus", InjectionMode::Never));
    }

    #[test]
    fn select_similar_records_prioritizes_similarity_then_recency() {
        let records = vec![
            TestRecord {
                run_id: "old-close",
                task: "inject review exemplars into orch review prompt",
                seen_at_ms: 100,
                label: None,
            },
            TestRecord {
                run_id: "new-close",
                task: "inject review exemplars into orch review prompt",
                seen_at_ms: 300,
                label: None,
            },
            TestRecord {
                run_id: "unrelated",
                task: "fix desktop window resizing",
                seen_at_ms: 900,
                label: None,
            },
        ];

        let selected = select_similar_records(&records, "review exemplar prompt injection", 2);

        assert_eq!(
            selected
                .iter()
                .map(|record| record.run_id)
                .collect::<Vec<_>>(),
            vec!["new-close", "old-close"]
        );
    }

    #[test]
    fn select_similar_records_by_label_picks_best_approved_and_revise_examples() {
        let records = vec![
            TestRecord {
                run_id: "approved-best",
                task: "archive orch review exemplars after approved implementation",
                seen_at_ms: 100,
                label: Some("APPROVED"),
            },
            TestRecord {
                run_id: "approved-weak",
                task: "desktop menu polish",
                seen_at_ms: 900,
                label: Some("APPROVED"),
            },
            TestRecord {
                run_id: "revise-best",
                task: "review exemplar archive missing revise verdict",
                seen_at_ms: 200,
                label: Some("REVISE"),
            },
            TestRecord {
                run_id: "revise-second",
                task: "review exemplar archive missing revise verdict",
                seen_at_ms: 100,
                label: Some("REVISE"),
            },
        ];

        let selected = select_similar_records_by_label(
            &records,
            "archive review exemplars for approved and revise verdicts",
            1,
            &["APPROVED", "REVISE"],
        );

        assert_eq!(
            selected
                .iter()
                .map(|record| record.run_id)
                .collect::<Vec<_>>(),
            vec!["approved-best", "revise-best"]
        );
    }
}
