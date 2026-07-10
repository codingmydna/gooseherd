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

/// Two selected exemplars whose task token sets overlap above this Jaccard are
/// treated as near-duplicates; the lower-ranked one is dropped and the next
/// candidate backfills its slot.
const NEAR_DUP_JACCARD: f64 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InjectionMode {
    Always,
    Never,
    Auto,
}

/// Shared configuration for one exemplar store. The four `GOOSE_*` knobs are all
/// derived from `key_prefix`, so plan/review stores are one struct literal each.
pub(super) struct StoreConfig {
    pub(super) dir: &'static str,
    pub(super) key_prefix: &'static str,
    pub(super) default_k: usize,
    pub(super) default_char_limit: usize,
}

impl StoreConfig {
    pub(super) fn enabled(&self) -> bool {
        Config::global()
            .get_param::<bool>(self.key_prefix)
            .unwrap_or(true)
    }

    pub(super) fn injection_mode(&self) -> InjectionMode {
        let raw = Config::global()
            .get_param::<String>(&format!("{}_INJECT", self.key_prefix))
            .unwrap_or_else(|_| "auto".to_string());
        parse_injection_mode(&raw)
    }

    pub(super) fn k(&self) -> usize {
        Config::global()
            .get_param::<usize>(&format!("{}_K", self.key_prefix))
            .ok()
            .filter(|k| *k > 0)
            .unwrap_or(self.default_k)
    }

    pub(super) fn char_limit(&self) -> usize {
        Config::global()
            .get_param::<usize>(&format!("{}_CHAR_LIMIT", self.key_prefix))
            .ok()
            .filter(|limit| *limit > 0)
            .unwrap_or(self.default_char_limit)
    }
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
    fn run_id(&self) -> &str;
    fn path(&self) -> &str;

    fn label(&self) -> Option<&str> {
        None
    }

    /// Canonicalized repo root the exemplar was archived from, or `None` for
    /// legacy records written before repo scoping. Same-repo records are
    /// preferred during selection; cross-repo ones only backfill.
    fn repo_root(&self) -> Option<&str> {
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

/// Canonicalized repo-root key used to scope exemplars to the repo they came
/// from. Falls back to the lexical path when canonicalization fails (e.g. the
/// path no longer exists) so writers and readers still derive the same key.
pub(super) fn repo_scope_key(repo_root: &Path) -> String {
    std::fs::canonicalize(repo_root)
        .unwrap_or_else(|_| repo_root.to_path_buf())
        .display()
        .to_string()
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

/// Read every parsable record from a store index. Corrupt lines are skipped with
/// a single aggregated warning rather than aborting the whole index — one bad
/// line must never silently disable an entire exemplar store.
pub(super) fn read_index<T: DeserializeOwned>(
    state_dir: &Path,
    exemplars_dir_name: &str,
) -> Vec<T> {
    let path = index_path(state_dir, exemplars_dir_name);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    let mut records = Vec::new();
    let mut skipped = 0usize;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<T>(line) {
            Ok(record) => records.push(record),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            index = %path.display(),
            "skipped unparsable exemplar index lines"
        );
    }
    records
}

/// Top-`k` records by task similarity, preferring same-repo exemplars and
/// suppressing near-duplicates (see [`take_scoped`]).
pub(super) fn select_similar_records<T: SimilarityRecord>(
    records: &[T],
    task: &str,
    k: usize,
    repo_root: Option<&str>,
) -> Vec<T> {
    take_scoped(&scored_records(records, task), k, repo_root)
}

/// Up to `per_label` records for each label in `labels`, each pool ranked by
/// task similarity with the same repo-preference and near-dup suppression.
pub(super) fn select_similar_records_by_label<T: SimilarityRecord>(
    records: &[T],
    task: &str,
    per_label: usize,
    labels: &[&str],
    repo_root: Option<&str>,
) -> Vec<T> {
    if per_label == 0 || labels.is_empty() {
        return Vec::new();
    }

    let scored = scored_records(records, task);
    let mut selected = Vec::new();
    for label in labels {
        let label_scored = scored
            .iter()
            .filter(|(_, record)| {
                record
                    .label()
                    .is_some_and(|record_label| record_label.eq_ignore_ascii_case(label))
            })
            .map(|(score, record)| (*score, *record))
            .collect::<Vec<_>>();
        selected.extend(take_scoped(&label_scored, per_label, repo_root));
    }
    selected
}

/// Greedily pick up to `k` records from `ranked` (already ordered best-first).
///
/// Repo scoping: when `repo_root` is set, same-repo records fill slots first and
/// cross-repo records only backfill the remainder. Near-dup: a candidate whose
/// task overlaps an already-picked task above [`NEAR_DUP_JACCARD`] is skipped so
/// the next distinct candidate takes the slot.
fn take_scoped<T: SimilarityRecord>(
    ranked: &[(f64, &T)],
    k: usize,
    repo_root: Option<&str>,
) -> Vec<T> {
    if k == 0 {
        return Vec::new();
    }

    let mut picked = Vec::new();
    let mut picked_tokens: Vec<HashSet<String>> = Vec::new();
    // With a known repo root, take same-repo records first (pass `true`), then
    // cross-repo backfill (pass `false`); otherwise a single pass over all.
    let passes: &[bool] = if repo_root.is_some() {
        &[true, false]
    } else {
        &[false]
    };
    for &same_repo_only in passes {
        for (_, record) in ranked {
            if picked.len() >= k {
                return picked;
            }
            if repo_root.is_some() && same_repo(*record, repo_root) != same_repo_only {
                continue;
            }
            let tokens = tokenize(record.task());
            if picked_tokens
                .iter()
                .any(|seen| jaccard(&tokens, seen) > NEAR_DUP_JACCARD)
            {
                continue;
            }
            picked_tokens.push(tokens);
            picked.push((*record).clone());
        }
    }
    picked
}

fn same_repo<T: SimilarityRecord>(record: &T, repo_root: Option<&str>) -> bool {
    matches!((repo_root, record.repo_root()), (Some(want), Some(have)) if want == have)
}

/// Assemble the injection block from already-selected records: read each
/// exemplar's artifact, skip any that no longer read, and format the rest with
/// `format_example`. Empty when nothing readable remains.
pub(super) fn assemble_injection<T: SimilarityRecord>(
    selected: &[T],
    header: &str,
    mut format_example: impl FnMut(usize, &T, &str) -> String,
) -> ExemplarInjection {
    let mut selected_run_ids = Vec::new();
    let mut body = header.to_string();
    for record in selected {
        let Ok(content) = std::fs::read_to_string(record.path()) else {
            continue;
        };
        selected_run_ids.push(record.run_id().to_string());
        body.push_str(&format_example(selected_run_ids.len(), record, &content));
    }

    if selected_run_ids.is_empty() {
        return ExemplarInjection::default();
    }

    ExemplarInjection {
        injected: true,
        selected_run_ids,
        prompt_section: Some(body.trim_end().to_string()),
    }
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
        repo_root: Option<&'static str>,
    }

    impl TestRecord {
        fn new(run_id: &'static str, task: &'static str, seen_at_ms: u128) -> Self {
            Self {
                run_id,
                task,
                seen_at_ms,
                label: None,
                repo_root: None,
            }
        }

        fn with_label(mut self, label: &'static str) -> Self {
            self.label = Some(label);
            self
        }

        fn with_repo(mut self, repo_root: &'static str) -> Self {
            self.repo_root = Some(repo_root);
            self
        }
    }

    impl SimilarityRecord for TestRecord {
        fn task(&self) -> &str {
            self.task
        }

        fn recency_ms(&self) -> u128 {
            self.seen_at_ms
        }

        fn run_id(&self) -> &str {
            self.run_id
        }

        fn path(&self) -> &str {
            self.run_id
        }

        fn label(&self) -> Option<&str> {
            self.label
        }

        fn repo_root(&self) -> Option<&str> {
            self.repo_root
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
        // Two records with equal similarity to the query (each overlaps a
        // disjoint half of it, so they are not near-dups of each other) plus one
        // unrelated record: the more recent of the tied pair ranks first.
        let records = vec![
            TestRecord::new("old-tied", "gamma delta epsilon", 100),
            TestRecord::new("new-tied", "alpha beta epsilon", 300),
            TestRecord::new("unrelated", "zeta eta theta", 900),
        ];

        let selected = select_similar_records(&records, "alpha beta gamma delta", 2, None);

        assert_eq!(
            selected
                .iter()
                .map(|record| record.run_id)
                .collect::<Vec<_>>(),
            vec!["new-tied", "old-tied"]
        );
    }

    #[test]
    fn select_similar_records_suppresses_near_duplicates() {
        // Two effectively identical tasks plus one distinct: near-dup collapses
        // the pair to its higher-ranked member and backfills with the distinct
        // candidate instead of emitting the duplicate.
        let records =
            vec![
            TestRecord::new("dup-new", "add plan exemplar injection to orch planner", 300),
            TestRecord::new("dup-old", "add plan exemplar injection to orch planner", 100),
            TestRecord::new(
                "distinct",
                "add plan exemplar injection to orch planner prompt with ledger fingerprint rows",
                50,
            ),
        ];

        let selected = select_similar_records(
            &records,
            "add plan exemplar injection to orch planner",
            2,
            None,
        );

        assert_eq!(
            selected
                .iter()
                .map(|record| record.run_id)
                .collect::<Vec<_>>(),
            vec!["dup-new", "distinct"]
        );
    }

    #[test]
    fn select_similar_records_prefers_same_repo_then_backfills_cross_repo() {
        let records = vec![
            // Highest raw similarity (exact query) but a different repo, and a
            // near-dup of the same-repo pick — it must lose to the same-repo
            // record and then be suppressed as a duplicate.
            TestRecord::new("other-exact", "alpha beta gamma delta", 400).with_repo("/repos/other"),
            // Weaker match, but same repo — must be preferred for the first slot.
            TestRecord::new("mine-weaker", "alpha beta gamma", 300).with_repo("/repos/mine"),
            // Distinct cross-repo record that backfills the second slot.
            TestRecord::new("other-distinct", "delta epsilon zeta", 200).with_repo("/repos/other"),
        ];

        let selected =
            select_similar_records(&records, "alpha beta gamma delta", 2, Some("/repos/mine"));

        // Same-repo record leads over the higher-similarity cross-repo one; the
        // cross-repo near-dup is dropped and the distinct cross-repo backfills.
        assert_eq!(
            selected
                .iter()
                .map(|record| record.run_id)
                .collect::<Vec<_>>(),
            vec!["mine-weaker", "other-distinct"]
        );
    }

    #[test]
    fn select_similar_records_by_label_picks_best_approved_and_revise_examples() {
        let records = vec![
            TestRecord::new(
                "approved-best",
                "archive orch review exemplars after approved implementation",
                100,
            )
            .with_label("APPROVED"),
            TestRecord::new("approved-weak", "desktop menu polish", 900).with_label("APPROVED"),
            TestRecord::new(
                "revise-best",
                "review exemplar archive missing revise verdict",
                200,
            )
            .with_label("REVISE"),
            TestRecord::new(
                "revise-second",
                "review exemplar archive missing revise verdict",
                100,
            )
            .with_label("REVISE"),
        ];

        let selected = select_similar_records_by_label(
            &records,
            "archive review exemplars for approved and revise verdicts",
            1,
            &["APPROVED", "REVISE"],
            None,
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
