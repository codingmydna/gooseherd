use goose::config::GooseMode;
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper, Result};
use std::borrow::Cow;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::Arc;
use strum::VariantNames;

use super::commands_registry;
use super::{CompletionCache, HintStatus};

const FILE_MENTION_LIMIT: usize = 50;

/// One-line description for an exact command or alias, from the registry.
fn command_desc(token: &str) -> Option<&'static str> {
    commands_registry::completion_pairs()
        .into_iter()
        .find(|(name, _)| *name == token)
        .map(|(_, desc)| desc)
}

fn at_mention_token(line: &str) -> Option<(usize, &str)> {
    let token_start = line
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(index, character)| index + character.len_utf8());
    let token = line.get(token_start..)?;

    token.starts_with('@').then_some((token_start, token))
}

fn file_mention_candidates(root: &Path, token: &str) -> Vec<Pair> {
    let path_query = token.strip_prefix('@').unwrap_or(token);
    let (dir_part, name_query) = path_query.rfind('/').map_or(("", path_query), |separator| {
        path_query.split_at(separator + 1)
    });
    let base = root.join(dir_part);
    if !base.is_dir() {
        return Vec::new();
    }

    let mut builder = ignore::WalkBuilder::new(&base);
    builder.max_depth(Some(1));
    builder.git_ignore(true);
    builder.git_exclude(true);
    builder.git_global(true);
    builder.require_git(false);
    builder.ignore(true);
    builder.hidden(false);

    let lowercase_query = name_query.to_lowercase();
    let mut matches: Vec<(String, bool)> = builder
        .build()
        .flatten()
        .filter(|entry| entry.path() != base)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy();
            if name == ".git" || !name.to_lowercase().contains(&lowercase_query) {
                return None;
            }

            Some((
                name.into_owned(),
                entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_dir()),
            ))
        })
        .collect();

    matches.sort_by(|(left_name, left_is_dir), (right_name, right_is_dir)| {
        right_is_dir.cmp(left_is_dir).then_with(|| {
            left_name
                .to_lowercase()
                .cmp(&right_name.to_lowercase())
                .then_with(|| left_name.cmp(right_name))
        })
    });
    matches.truncate(FILE_MENTION_LIMIT);

    matches
        .into_iter()
        .map(|(name, is_dir)| {
            let suffix = if is_dir { "/" } else { "" };
            Pair {
                display: format!("{name}{suffix}"),
                replacement: format!("@{dir_part}{name}{suffix}"),
            }
        })
        .collect()
}

fn slash_command_candidates(token: &str, limit: usize) -> Vec<(&'static str, &'static str)> {
    let query = token.trim_start_matches('/');
    let pairs = commands_registry::completion_pairs();
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for (command, description) in pairs.iter().copied() {
        if command.starts_with(token) && seen.insert(command) {
            result.push((command, description));
        }
    }
    for (command, description) in pairs.iter().copied() {
        if !command.starts_with(token)
            && !query.is_empty()
            && command.trim_start_matches('/').contains(query)
            && seen.insert(command)
        {
            result.push((command, description));
        }
    }

    result.truncate(limit);
    result
}

fn slash_command_hint(line: &str) -> Option<String> {
    let (token, has_args) = match line.split_once(' ') {
        Some((cmd, _)) => (cmd, true),
        None => (line, false),
    };
    if has_args {
        if token == "/goal" {
            return Some(
                "   ◎ /goal <goal text> [--max N] [--check \"shell cmd\"] · /goal stop".to_string(),
            );
        }
        if token == "/loop" {
            return Some(
                "   ↻ /loop <30s|5m|1h|90> [--max N] [--until-done] <prompt> · /loop stop"
                    .to_string(),
            );
        }
        return match command_desc(token) {
            Some(desc) => Some(format!("   ✓ {}", desc)),
            None => Some("   ✗ unknown command".to_string()),
        };
    }
    if let Some(desc) = command_desc(token) {
        if commands_registry::requires_args(token) {
            return Some(format!("   ✓ {} — needs an argument", desc));
        }
        return Some(format!("   ✓ {}", desc));
    }
    let matches = slash_command_candidates(token, 5);
    match matches.len() {
        0 => Some("   ✗ unknown command".to_string()),
        _ => Some(
            matches
                .into_iter()
                .map(|(command, desc)| format!("   {command} — {desc}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

fn build_input_hint(inline: Option<&str>, status: &str, width: usize) -> String {
    let bottom = super::input::box_bottom(width);
    match inline.filter(|hint| !hint.is_empty()) {
        Some(inline) => format!("{inline}\n{bottom}\n  {status}"),
        None => format!("\n{bottom}\n  {status}"),
    }
}

fn input_hint_status(cache: &CompletionCache) -> String {
    if let Some(flash) = &cache.flash {
        return flash.clone();
    }

    match cache.hint_status {
        HintStatus::Interrupted => "Interrupted, what should goose work on instead?".to_string(),
        HintStatus::MaybeExit => {
            "Press Ctrl+C again to exit, or type new instructions to continue".to_string()
        }
        HintStatus::Default => {
            let controls = newline_controls_from_cache(cache);
            match &cache.status_line {
                Some(status) => format!("{status} · {controls}"),
                None => controls,
            }
        }
    }
}

fn newline_controls_from_cache(cache: &CompletionCache) -> String {
    let state = cache.newline_hint_state;
    super::terminal_setup::newline_control_hint(
        state.terminal,
        state.shift_enter_configured,
        super::input::get_newline_key(),
    )
}

/// Completer for goose CLI commands
pub struct GooseCompleter {
    pub completion_cache: Arc<std::sync::RwLock<CompletionCache>>,
    filename_completer: FilenameCompleter,
}

impl GooseCompleter {
    /// Create a new GooseCompleter with a reference to the Session's completion cache
    pub fn new(completion_cache: Arc<std::sync::RwLock<CompletionCache>>) -> Self {
        Self {
            completion_cache,
            filename_completer: FilenameCompleter::new(),
        }
    }

    /// Complete prompt names for the /prompt command
    fn complete_prompt_names(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        // Get the prefix of the prompt name being typed
        let prefix = line.get(8..).unwrap_or("");

        // Get available prompts from cache
        let cache = self.completion_cache.read().unwrap();

        // Create completion candidates that match the prefix
        let candidates: Vec<Pair> = cache
            .prompts
            .iter()
            .flat_map(|(_, names)| names)
            .filter(|name| name.starts_with(prefix.trim()))
            .map(|name| Pair {
                display: name.clone(),
                replacement: name.clone(),
            })
            .collect();

        Ok((8, candidates))
    }

    /// Complete flags for the /prompt command
    fn complete_prompt_flags(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        // Get the last part of the line
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(last_part) = parts.last() {
            // If the last part starts with '-', it might be a partial flag
            if last_part.starts_with('-') {
                // Define available flags
                let flags = ["--info"];

                // Find flags that match the prefix
                let matching_flags: Vec<Pair> = flags
                    .iter()
                    .filter(|flag| flag.starts_with(last_part))
                    .map(|flag| Pair {
                        display: flag.to_string(),
                        replacement: flag.to_string(),
                    })
                    .collect();

                if !matching_flags.is_empty() {
                    // Return matches for the partial flag
                    // The position is the start of the last word
                    let pos = line.len() - last_part.len();
                    return Ok((pos, matching_flags));
                }
            }
        }

        // No flag completions available
        Ok((line.len(), vec![]))
    }

    /// Complete flags for the /mode command
    fn complete_mode_flags(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        let modes = GooseMode::VARIANTS;

        let parts: Vec<&str> = line.split_whitespace().collect();

        // If we're just after "/mode" with a space, show all options
        if line == "/mode " {
            return Ok((
                line.len(),
                modes
                    .iter()
                    .map(|mode| Pair {
                        display: mode.to_string(),
                        replacement: format!("{} ", mode),
                    })
                    .collect(),
            ));
        }

        // If we're typing a mode name, show the flags for that mode
        if parts.len() == 2 {
            let partial = parts[1].to_lowercase();
            return Ok((
                line.len() - partial.len(),
                modes
                    .iter()
                    .filter(|mode| mode.to_lowercase().starts_with(&partial.to_lowercase()))
                    .map(|mode| Pair {
                        display: mode.to_string(),
                        replacement: format!("{} ", mode),
                    })
                    .collect(),
            ));
        }

        // No completions available
        Ok((line.len(), vec![]))
    }

    /// Complete skill names for the /skills command
    fn complete_skill_names(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        use goose::skills::list_installed_skills;

        let cwd = std::env::current_dir().unwrap_or_default();
        let skills = list_installed_skills(Some(&cwd));
        let skill_names: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();

        let last = line.rsplit_once(' ').map_or("", |(_, w)| w);
        let pos = line.len() - last.len();

        let partial = last.to_lowercase();
        let candidates: Vec<Pair> = skill_names
            .iter()
            .filter(|name| name.to_lowercase().starts_with(&partial))
            .map(|name| Pair {
                display: name.clone(),
                replacement: format!("{} ", name),
            })
            .collect();

        Ok((pos, candidates))
    }

    /// Complete model names for the /model command.
    fn complete_model_names(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        let arg_start = "/model ".len();
        let raw_argument = line.get(arg_start..).unwrap_or("");
        let trimmed_argument = raw_argument.trim_start();
        let pos = arg_start + raw_argument.len().saturating_sub(trimmed_argument.len());
        let cache = self.completion_cache.read().unwrap();

        let candidates =
            if let Some((provider_name, model_prefix)) = trimmed_argument.split_once('/') {
                cache
                    .providers
                    .iter()
                    .find(|(name, _)| name == provider_name)
                    .map(|(_, models)| {
                        models
                            .iter()
                            .filter(|model| model.starts_with(model_prefix))
                            .map(|model| Pair {
                                display: model.clone(),
                                replacement: format!("{provider_name}/{model}"),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                let provider_candidates = cache
                    .providers
                    .iter()
                    .filter(|(name, _)| name.starts_with(trimmed_argument))
                    .map(|(name, _)| Pair {
                        display: format!("{name}/"),
                        replacement: format!("{name}/"),
                    });

                let active_models = cache
                    .active_provider
                    .as_ref()
                    .and_then(|active| {
                        cache
                            .providers
                            .iter()
                            .find(|(name, _)| name == active)
                            .map(|(_, models)| models)
                    })
                    .or_else(|| cache.providers.first().map(|(_, models)| models));

                provider_candidates
                    .chain(active_models.into_iter().flat_map(|models| {
                        models
                            .iter()
                            .filter(|model| model.starts_with(trimmed_argument))
                            .map(|model| Pair {
                                display: model.clone(),
                                replacement: model.clone(),
                            })
                    }))
                    .collect()
            };

        Ok((pos, candidates))
    }

    /// Complete slash commands
    fn complete_slash_commands(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        // Find commands that match the prefix
        let matching_commands: Vec<Pair> = commands_registry::completion_pairs()
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(line))
            .map(|(cmd, _)| Pair {
                display: cmd.to_string(),
                replacement: format!("{} ", cmd), // Add a space after the command
            })
            .collect();

        if !matching_commands.is_empty() {
            return Ok((0, matching_commands));
        }

        // No command completions available
        Ok((line.len(), vec![]))
    }

    fn complete_loop_args(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        if line == "/loop " {
            return Ok((
                line.len(),
                vec![
                    Pair {
                        display: "stop".to_string(),
                        replacement: "stop".to_string(),
                    },
                    Pair {
                        display: "--max".to_string(),
                        replacement: "--max ".to_string(),
                    },
                    Pair {
                        display: "--until-done".to_string(),
                        replacement: "--until-done ".to_string(),
                    },
                ],
            ));
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(last) = parts.last() {
            if last.starts_with('-') {
                let pos = line.len() - last.len();
                let flags = ["--max", "--until-done"];
                let candidates = flags
                    .iter()
                    .filter(|flag| flag.starts_with(last))
                    .map(|flag| Pair {
                        display: flag.to_string(),
                        replacement: format!("{flag} "),
                    })
                    .collect();
                return Ok((pos, candidates));
            }
        }

        Ok((line.len(), vec![]))
    }

    /// Complete argument keys for a specific prompt
    fn complete_argument_keys(&self, line: &str) -> Result<(usize, Vec<Pair>)> {
        let parts: Vec<&str> = line.get(8..).unwrap_or("").split_whitespace().collect();

        // We need at least the prompt name
        if parts.is_empty() {
            return Ok((line.len(), vec![]));
        }

        let prompt_name = parts[0];

        // Get prompt info from cache
        let cache = self.completion_cache.read().unwrap();
        let prompt_info = cache.prompt_info.get(prompt_name).cloned();

        if let Some(info) = prompt_info {
            if let Some(args) = info.arguments {
                // Find required arguments that haven't been provided yet
                let existing_args: Vec<&str> = parts
                    .iter()
                    .skip(1)
                    .filter_map(|part| {
                        if part.contains('=') {
                            Some(part.split('=').next().unwrap())
                        } else {
                            None
                        }
                    })
                    .collect();

                // Check if we're trying to complete a partial argument name
                if let Some(last_part) = parts.last() {
                    // ignore if last_part starts with = / \ for suggestions
                    if let Some(c) = last_part.chars().next() {
                        if matches!(c, '=' | '/' | '\\') {
                            return Ok((line.len(), vec![]));
                        }
                    }

                    // If the last part doesn't contain '=', it might be a partial argument name
                    if !last_part.contains('=') {
                        // Find arguments that match the prefix
                        let matching_args: Vec<Pair> = args
                            .iter()
                            .filter(|arg| {
                                arg.name.starts_with(last_part)
                                    && !existing_args.contains(&arg.name.as_str())
                            })
                            .map(|arg| Pair {
                                display: format!("{}=", arg.name),
                                replacement: format!("{}=", arg.name),
                            })
                            .collect();

                        if !matching_args.is_empty() {
                            // Return matches for the partial argument name
                            // The position is the start of the last word
                            let pos = line.len() - last_part.len();
                            return Ok((pos, matching_args));
                        }

                        // If we have a partial argument that doesn't match anything,
                        // return an empty list rather than suggesting unrelated arguments
                        if !last_part.is_empty() && *last_part != prompt_name {
                            return Ok((line.len(), vec![]));
                        }
                    }
                }

                // If no partial match or no last part, suggest all required arguments
                // Use a reference to avoid moving args
                let mut candidates: Vec<_> = Vec::new();
                for arg in &args {
                    if arg.required.unwrap_or(false) && !existing_args.contains(&arg.name.as_str())
                    {
                        candidates.push(Pair {
                            display: format!("{}=", arg.name),
                            replacement: format!("{}=", arg.name),
                        });
                    }
                }

                if !candidates.is_empty() {
                    return Ok((line.len(), candidates));
                }

                // If no required arguments left, suggest all optional ones
                // Use a reference to avoid moving args
                for arg in &args {
                    if !arg.required.unwrap_or(true) && !existing_args.contains(&arg.name.as_str())
                    {
                        candidates.push(Pair {
                            display: format!("{}=", arg.name),
                            replacement: format!("{}=", arg.name),
                        });
                    }
                }
                return Ok((line.len(), candidates));
            }
        }

        // No completions available
        Ok((line.len(), vec![]))
    }

    /// Complete file paths
    fn complete_file_path(&self, line: &str, ctx: &Context) -> Result<(usize, Vec<Pair>)> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if let Some(last_part) = parts.last() {
            // Skip filename completion for words starting with special characters
            if last_part.starts_with('/') && last_part.len() == 1 {
                // Just a slash - no completion
                return Ok((line.len(), vec![]));
            }

            if last_part.starts_with('-') || last_part.contains('=') {
                // Skip flag or key-value pairs
                return Ok((line.len(), vec![]));
            }

            // Complete the partial path
            let pos = line.len() - last_part.len();
            let (start, candidates) =
                self.filename_completer
                    .complete(last_part, last_part.len(), ctx)?;

            // Return the completion results, with adjusted position
            return Ok((pos + start, candidates));
        }

        Ok((line.len(), vec![]))
    }

    fn complete_file_mention(&self, token_start: usize, token: &str) -> Result<(usize, Vec<Pair>)> {
        let root = std::env::current_dir().unwrap_or_default();
        Ok((token_start, file_mention_candidates(root.as_path(), token)))
    }
}

impl Completer for GooseCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> Result<(usize, Vec<Self::Candidate>)> {
        // If the cursor is not at the end of the line, don't try to complete
        if pos < line.len() {
            return Ok((pos, vec![]));
        }

        // If the line starts with '/', it might be a slash command
        if line.starts_with('/') {
            // If it's just a partial slash command (no space yet)
            if !line.contains(' ') {
                return self.complete_slash_commands(line);
            }

            // Handle /prompt command
            if line.starts_with("/prompt") {
                // If we're just after "/prompt" with or without a space
                if line == "/prompt" || line == "/prompt " {
                    return self.complete_prompt_names(line);
                }

                // Get the parts of the command
                let parts: Vec<&str> = line.split_whitespace().collect();

                // If we're typing a prompt name (only one part after /prompt)
                if parts.len() == 2 && !line.ends_with(' ') {
                    return self.complete_prompt_names(line);
                }

                // Check if we might be typing a flag
                if let Some(last_part) = parts.last() {
                    if last_part.starts_with('-') {
                        return self.complete_prompt_flags(line);
                    }
                }

                // If we have a prompt name and need argument completion
                if parts.len() >= 2 {
                    return self.complete_argument_keys(line);
                }
            }

            // Handle /prompts command
            if line.starts_with("/prompts") {
                // If we're just after "/prompts" with a space
                if line == "/prompts " {
                    // Suggest the --extension flag
                    return Ok((
                        line.len(),
                        vec![Pair {
                            display: "--extension".to_string(),
                            replacement: "--extension ".to_string(),
                        }],
                    ));
                }

                // Check if we might be typing the --extension flag
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 2
                    && parts[1].starts_with('-')
                    && "--extension".starts_with(parts[1])
                {
                    return Ok((
                        line.len() - parts[1].len(),
                        vec![Pair {
                            display: "--extension".to_string(),
                            replacement: "--extension ".to_string(),
                        }],
                    ));
                }
            }

            if line.starts_with("/model") {
                return self.complete_model_names(line);
            }

            if line.starts_with("/mode") {
                return self.complete_mode_flags(line);
            }

            if line.starts_with("/skills ") {
                return self.complete_skill_names(line);
            }

            if line.starts_with("/loop ") {
                return self.complete_loop_args(line);
            }

            return Ok((pos, vec![]));
        }

        if let Some((token_start, token)) = at_mention_token(line) {
            return self.complete_file_mention(token_start, token);
        }

        // For normal text (not slash commands), try file path completion
        self.complete_file_path(line, ctx)
    }
}

// Implement the Helper trait which is required by rustyline
impl Helper for GooseCompleter {}

// Implement required traits with default implementations
impl Hinter for GooseCompleter {
    type Hint = String;

    fn hint(&self, line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<Self::Hint> {
        if io::stdout().is_terminal() {
            if !line.is_empty() {
                let mut cache = self.completion_cache.write().unwrap();
                if cache.hint_status != HintStatus::Default {
                    cache.hint_status = HintStatus::Default;
                }
            }

            let inline = if line.starts_with('/') && _pos == line.len() {
                slash_command_hint(line)
            } else {
                None
            };
            let cache = self.completion_cache.read().unwrap();
            let status = input_hint_status(&cache);
            return Some(build_input_hint(
                inline.as_deref(),
                &status,
                super::input::terminal_input_box_width(),
            ));
        }

        let cache = self.completion_cache.read().unwrap();

        if !line.is_empty() && cache.hint_status != HintStatus::Default {
            drop(cache);
            let mut cache_write = self.completion_cache.write().unwrap();
            cache_write.hint_status = HintStatus::Default;
            return None;
        }

        if !line.is_empty() {
            if line.starts_with('/') && _pos == line.len() {
                return slash_command_hint(line);
            }
            return None;
        }

        if let Some(flash) = &cache.flash {
            return Some(flash.clone());
        }

        match cache.hint_status {
            HintStatus::Interrupted => {
                Some("Interrupted, what should goose work on instead?".to_string())
            }
            HintStatus::MaybeExit => {
                Some("Press Ctrl+C again to exit, or type new instructions to continue".to_string())
            }
            HintStatus::Default => {
                let controls = newline_controls_from_cache(&cache);
                match &cache.status_line {
                    Some(status) => Some(format!("{status} · {controls}")),
                    None => Some(controls),
                }
            }
        }
    }
}

impl Highlighter for GooseCompleter {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        if let Some((top, "│ > ")) = prompt.split_once('\n') {
            let dim = console::Style::new().dim();
            let bold = console::Style::new().bold();
            return Cow::Owned(format!(
                "{}\n{} {} ",
                dim.apply_to(top),
                dim.apply_to("│"),
                bold.apply_to(">")
            ));
        }

        Cow::Borrowed(prompt)
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        // Style the hint text with a dim color
        let styled = console::Style::new().dim().apply_to(hint).to_string();
        Cow::Owned(styled)
    }

    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        Cow::Borrowed(line)
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _cmd_kind: CmdKind) -> bool {
        false
    }
}

impl Validator for GooseCompleter {
    fn validate(
        &self,
        _ctx: &mut rustyline::validate::ValidationContext,
    ) -> Result<rustyline::validate::ValidationResult> {
        Ok(rustyline::validate::ValidationResult::Valid(None))
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::PromptArgument;

    use super::*;
    use crate::session::output;
    use std::fs;
    use std::sync::{Arc, RwLock};
    use tempfile::TempDir;

    // Helper function to create a test completion cache
    fn create_test_cache() -> Arc<RwLock<CompletionCache>> {
        let mut cache = CompletionCache::new();

        // Add some test prompts
        cache.prompts.insert(
            "extension1".to_string(),
            vec!["test_prompt1".to_string(), "test_prompt2".to_string()],
        );

        cache
            .prompts
            .insert("extension2".to_string(), vec!["other_prompt".to_string()]);

        // Add prompt info with arguments
        let test_prompt1_args = vec![
            PromptArgument::new("required_arg")
                .with_description("A required argument")
                .with_required(true),
            PromptArgument::new("optional_arg")
                .with_description("An optional argument")
                .with_required(false),
        ];

        let test_prompt1_info = output::PromptInfo {
            name: "test_prompt1".to_string(),
            description: Some("Test prompt 1 description".to_string()),
            arguments: Some(test_prompt1_args),
            extension: Some("extension1".to_string()),
        };
        cache
            .prompt_info
            .insert("test_prompt1".to_string(), test_prompt1_info);

        let test_prompt2_info = output::PromptInfo {
            name: "test_prompt2".to_string(),
            description: Some("Test prompt 2 description".to_string()),
            arguments: None,
            extension: Some("extension1".to_string()),
        };
        cache
            .prompt_info
            .insert("test_prompt2".to_string(), test_prompt2_info);

        let other_prompt_info = output::PromptInfo {
            name: "other_prompt".to_string(),
            description: Some("Other prompt description".to_string()),
            arguments: None,
            extension: Some("extension2".to_string()),
        };
        cache
            .prompt_info
            .insert("other_prompt".to_string(), other_prompt_info);

        cache.providers = vec![
            (
                "openai".to_string(),
                vec!["gpt-5".to_string(), "gpt-5.5".to_string()],
            ),
            (
                "codex-acp".to_string(),
                vec!["gpt-5.5".to_string(), "gpt-5.6".to_string()],
            ),
        ];

        Arc::new(RwLock::new(cache))
    }

    fn create_file_mention_fixture() -> TempDir {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir_all(fixture.path().join("src")).unwrap();
        fs::create_dir_all(fixture.path().join("docs")).unwrap();
        fs::create_dir_all(fixture.path().join("target/debug")).unwrap();
        fs::create_dir_all(fixture.path().join(".git")).unwrap();
        fs::create_dir_all(fixture.path().join(".github/workflows")).unwrap();
        fs::write(fixture.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(fixture.path().join("src/lib.rs"), "").unwrap();
        fs::write(fixture.path().join("README.md"), "read me\n").unwrap();
        fs::write(fixture.path().join("target/debug/foo"), "").unwrap();
        fs::write(fixture.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(fixture.path().join(".github/workflows/ci.yml"), "").unwrap();
        fs::write(fixture.path().join(".gitignore"), "target/\n").unwrap();
        fixture
    }

    #[test]
    fn recognizes_at_mention_as_the_last_token() {
        assert_eq!(at_mention_token("look at @src"), Some((8, "@src")));
        assert_eq!(at_mention_token("@src"), Some((0, "@src")));
        assert_eq!(at_mention_token("mail a@b.com"), None);
        assert_eq!(at_mention_token("hello "), None);
    }

    #[test]
    fn file_mentions_sort_directories_first_and_append_slashes() {
        let fixture = create_file_mention_fixture();
        let candidates = file_mention_candidates(fixture.path(), "@");
        let replacements: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.replacement.as_str())
            .collect();

        assert_eq!(
            replacements,
            vec!["@.github/", "@docs/", "@src/", "@.gitignore", "@README.md"]
        );
        assert_eq!(candidates[1].display, "docs/");
    }

    #[test]
    fn file_mentions_continue_inside_directories() {
        let fixture = create_file_mention_fixture();
        let candidates = file_mention_candidates(fixture.path(), "@src/ma");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "@src/main.rs");
    }

    #[test]
    fn file_mentions_respect_gitignore_and_exclude_git_directory() {
        let fixture = create_file_mention_fixture();
        let replacements: Vec<_> = file_mention_candidates(fixture.path(), "@")
            .into_iter()
            .map(|candidate| candidate.replacement)
            .collect();

        assert!(!replacements.iter().any(|candidate| candidate == "@target/"));
        assert!(!replacements.iter().any(|candidate| candidate == "@.git/"));
        assert!(replacements
            .iter()
            .any(|candidate| candidate == "@.github/"));
    }

    #[test]
    fn file_mentions_are_deterministically_sorted_and_limited() {
        let fixture = tempfile::tempdir().unwrap();
        for index in (0..60).rev() {
            fs::write(fixture.path().join(format!("f{index:02}")), "").unwrap();
        }

        let first: Vec<_> = file_mention_candidates(fixture.path(), "@")
            .into_iter()
            .map(|candidate| candidate.replacement)
            .collect();
        let second: Vec<_> = file_mention_candidates(fixture.path(), "@")
            .into_iter()
            .map(|candidate| candidate.replacement)
            .collect();

        assert_eq!(first.len(), FILE_MENTION_LIMIT);
        assert_eq!(first, second);
        assert_eq!(first.first().map(String::as_str), Some("@f00"));
        assert_eq!(first.last().map(String::as_str), Some("@f49"));
    }

    #[test]
    fn file_mentions_match_case_insensitive_substrings() {
        let fixture = create_file_mention_fixture();

        let root_matches = file_mention_candidates(fixture.path(), "@readme");
        assert_eq!(root_matches[0].replacement, "@README.md");

        let nested_matches = file_mention_candidates(fixture.path(), "@src/AIN");
        assert_eq!(nested_matches[0].replacement, "@src/main.rs");
    }

    #[test]
    fn non_at_input_keeps_existing_slash_completion() {
        let completer = GooseCompleter::new(create_test_cache());
        let history = rustyline::history::DefaultHistory::new();
        let context = Context::new(&history);
        let candidates = completer.complete("/mo", "/mo".len(), &context).unwrap().1;

        assert_eq!(at_mention_token("plain text"), None);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.replacement.as_str())
                .collect::<Vec<_>>(),
            vec!["/model ", "/mode "]
        );
    }

    #[test]
    fn build_input_hint_places_inline_hint_above_status_box() {
        let hint = build_input_hint(
            Some("del — Delete a saved preset"),
            "anthropic/claude · auto · ctx 12% · Enter to send",
            20,
        );

        assert_eq!(
            hint,
            "del — Delete a saved preset\n╰──────────────────╯\n  anthropic/claude · auto · ctx 12% · Enter to send"
        );
        assert!(!hint.ends_with('\n'));
    }

    #[test]
    fn build_input_hint_omits_blank_inline_line() {
        let hint = build_input_hint(None, "Enter to send", 20);

        assert_eq!(hint, "\n╰──────────────────╯\n  Enter to send");
        assert!(hint.starts_with('\n'));
        assert!(!hint.ends_with('\n'));
    }

    #[test]
    fn slash_command_hint_lists_partial_matches_with_descriptions() {
        let hint = slash_command_hint("/mo").expect("slash command hint");

        assert!(hint.contains("/model — Pick or switch"));
        assert!(hint.contains("/mode — Set goose mode"));
        assert!(hint.lines().count() >= 2);
    }

    #[test]
    fn slash_command_hint_limits_partial_match_list_to_five() {
        let hint = slash_command_hint("/").expect("slash command hint");

        assert_eq!(hint.lines().count(), 5);
        assert!(hint.lines().all(|line| line.contains(" — ")));
    }

    #[test]
    fn input_hint_status_prioritizes_flash() {
        let mut cache = CompletionCache::new();
        cache.status_line = Some("anthropic/claude · auto · ctx 12%".to_string());
        cache.hint_status = HintStatus::MaybeExit;
        cache.flash = Some("preset → fast · anthropic/claude".to_string());

        assert_eq!(
            input_hint_status(&cache),
            "preset → fast · anthropic/claude"
        );
    }

    #[test]
    fn input_hint_status_shows_interrupt_and_exit_prompts() {
        let mut cache = CompletionCache::new();
        cache.hint_status = HintStatus::Interrupted;
        assert_eq!(
            input_hint_status(&cache),
            "Interrupted, what should goose work on instead?"
        );

        cache.hint_status = HintStatus::MaybeExit;
        assert_eq!(
            input_hint_status(&cache),
            "Press Ctrl+C again to exit, or type new instructions to continue"
        );
    }

    #[test]
    fn input_hint_status_reuses_completion_cache_status_line() {
        let mut cache = CompletionCache::new();
        cache.status_line = Some("anthropic/claude · auto · ctx 12%".to_string());

        let status = input_hint_status(&cache);

        assert!(status.starts_with("anthropic/claude · auto · ctx 12% · Enter to send"));
        assert!(status.contains("Ctrl+J newline"));
    }

    #[test]
    fn input_hint_status_uses_cached_newline_state() {
        let mut cache = CompletionCache::new();
        cache.newline_hint_state = super::super::terminal_setup::NewlineHintState {
            terminal: super::super::terminal_setup::DetectedTerminal::VsCode,
            shift_enter_configured: true,
        };

        assert_eq!(
            input_hint_status(&cache),
            "Enter to send · Shift+Enter newline · Ctrl+J newline"
        );

        cache.newline_hint_state.shift_enter_configured = false;
        let status = input_hint_status(&cache);

        assert!(status.contains("/terminal-setup"));
        assert!(status.contains("Option+Enter"));
    }

    #[test]
    fn test_complete_slash_commands() {
        let cache = create_test_cache();
        let completer = GooseCompleter::new(cache);

        // Test complete match
        let (pos, candidates) = completer.complete_slash_commands("/exit").unwrap();
        assert_eq!(pos, 0);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display, "/exit");
        assert_eq!(candidates[0].replacement, "/exit ");

        // Test partial match
        let (pos, candidates) = completer.complete_slash_commands("/e").unwrap();
        assert_eq!(pos, 0);
        // There might be multiple commands starting with "e" like "/exit" and "/extension"
        assert!(!candidates.is_empty());

        // Test multiple matches
        let (pos, candidates) = completer.complete_slash_commands("/").unwrap();
        assert_eq!(pos, 0);
        assert!(candidates.len() > 1);

        // Test no match
        let (_pos, candidates) = completer.complete_slash_commands("/nonexistent").unwrap();
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_complete_model_names() {
        let cache = create_test_cache();
        let completer = GooseCompleter::new(cache);

        let (pos, candidates) = completer.complete_model_names("/model ").unwrap();
        assert_eq!(pos, "/model ".len());
        assert!(candidates.iter().any(|c| c.replacement == "openai/"));
        assert!(candidates.iter().any(|c| c.replacement == "codex-acp/"));
        assert!(candidates.iter().any(|c| c.replacement == "gpt-5"));

        let (pos, candidates) = completer.complete_model_names("/model gpt").unwrap();
        assert_eq!(pos, "/model ".len());
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.replacement.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-5", "gpt-5.5"]
        );

        let (pos, candidates) = completer
            .complete_model_names("/model codex-acp/gpt-5.")
            .unwrap();
        assert_eq!(pos, "/model ".len());
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.replacement.as_str())
                .collect::<Vec<_>>(),
            vec!["codex-acp/gpt-5.5", "codex-acp/gpt-5.6"]
        );
    }

    #[test]
    fn test_complete_prompt_names() {
        let cache = create_test_cache();
        let completer = GooseCompleter::new(cache);

        // Test with just "/prompt "
        let (pos, candidates) = completer.complete_prompt_names("/prompt ").unwrap();
        assert_eq!(pos, 8);
        assert_eq!(candidates.len(), 3); // All prompts

        // Test with partial prompt name
        let (pos, candidates) = completer.complete_prompt_names("/prompt test").unwrap();
        assert_eq!(pos, 8);
        assert_eq!(candidates.len(), 2); // test_prompt1 and test_prompt2

        // Test with specific prompt name
        let (pos, candidates) = completer
            .complete_prompt_names("/prompt test_prompt1")
            .unwrap();
        assert_eq!(pos, 8);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display, "test_prompt1");

        // Test with no match
        let (pos, candidates) = completer
            .complete_prompt_names("/prompt nonexistent")
            .unwrap();
        assert_eq!(pos, 8);
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_complete_prompt_flags() {
        let cache = create_test_cache();
        let completer = GooseCompleter::new(cache);

        // Test with partial flag
        let (_pos, candidates) = completer
            .complete_prompt_flags("/prompt test_prompt1 --")
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display, "--info");

        // Test with exact flag
        let (_pos, candidates) = completer
            .complete_prompt_flags("/prompt test_prompt1 --info")
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display, "--info");

        // Test with no match
        let (_pos, candidates) = completer
            .complete_prompt_flags("/prompt test_prompt1 --nonexistent")
            .unwrap();
        assert_eq!(candidates.len(), 0);

        // Test with no flag
        let (_pos, candidates) = completer
            .complete_prompt_flags("/prompt test_prompt1")
            .unwrap();
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_complete_argument_keys() {
        let cache = create_test_cache();
        let completer = GooseCompleter::new(cache);

        // Test with just a prompt name (no space after)
        // This case doesn't return any candidates in the current implementation
        let (_pos, candidates) = completer
            .complete_argument_keys("/prompt test_prompt1")
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display, "required_arg=");

        // Test with partial argument
        let (_pos, candidates) = completer
            .complete_argument_keys("/prompt test_prompt1 req")
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display, "required_arg=");

        // Test with one argument already provided
        let (_pos, candidates) = completer
            .complete_argument_keys("/prompt test_prompt1 required_arg=value")
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display, "optional_arg=");

        // Test with all arguments provided
        let (_pos, candidates) = completer
            .complete_argument_keys("/prompt test_prompt1 required_arg=value optional_arg=value")
            .unwrap();
        assert_eq!(candidates.len(), 0);

        // Test with prompt that has no arguments
        let (_pos, candidates) = completer
            .complete_argument_keys("/prompt test_prompt2")
            .unwrap();
        assert_eq!(candidates.len(), 0);

        // Test with nonexistent prompt
        let (_pos, candidates) = completer
            .complete_argument_keys("/prompt nonexistent")
            .unwrap();
        assert_eq!(candidates.len(), 0);
    }
}
