//! Single source of truth for interactive slash commands.
//!
//! Tab completion, the inline typing hint, `/help`, and nearest-match typo
//! suggestions all derive from [`COMMANDS`]. Dispatch itself stays a hand-written
//! match in [`super::input`]; a unit test there asserts every registry entry has
//! a dispatch arm so the two never drift.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    Session,
    Orchestration,
    Context,
    Config,
}

impl CommandGroup {
    const ORDER: [CommandGroup; 4] = [
        CommandGroup::Session,
        CommandGroup::Orchestration,
        CommandGroup::Context,
        CommandGroup::Config,
    ];

    fn title(self) -> &'static str {
        match self {
            CommandGroup::Session => "Session",
            CommandGroup::Orchestration => "Orchestration",
            CommandGroup::Context => "Context",
            CommandGroup::Config => "Config",
        }
    }
}

pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// Argument sketch shown in `/help`, e.g. `"<task>"` or `"[provider/]model"`.
    pub args: &'static str,
    pub desc: &'static str,
    pub group: CommandGroup,
    /// The bare command does not dispatch on its own (it needs an argument);
    /// used by the coverage test to pick a valid sample invocation.
    pub needs_args: bool,
}

impl SlashCommand {
    /// Left column of a `/help` row: the command, its aliases, and its args.
    fn invocation(&self) -> String {
        let mut out = self.name.to_string();
        for alias in self.aliases {
            out.push(' ');
            out.push_str(alias);
        }
        if !self.args.is_empty() {
            out.push(' ');
            out.push_str(self.args);
        }
        out
    }
}

pub const COMMANDS: &[SlashCommand] = &[
    // Session
    SlashCommand {
        name: "/help",
        aliases: &["/?"],
        args: "",
        desc: "Show available commands",
        group: CommandGroup::Session,
        needs_args: false,
    },
    SlashCommand {
        name: "/exit",
        aliases: &["/quit"],
        args: "",
        desc: "Exit the session",
        group: CommandGroup::Session,
        needs_args: false,
    },
    SlashCommand {
        name: "/clear",
        aliases: &[],
        args: "",
        desc: "Clear the conversation history",
        group: CommandGroup::Session,
        needs_args: false,
    },
    SlashCommand {
        name: "/status",
        aliases: &[],
        args: "",
        desc: "Session status: model, roles, subagents, usage",
        group: CommandGroup::Session,
        needs_args: false,
    },
    SlashCommand {
        name: "/usage",
        aliases: &[],
        args: "",
        desc: "Token usage and cost for this session",
        group: CommandGroup::Session,
        needs_args: false,
    },
    SlashCommand {
        name: "/stats",
        aliases: &[],
        args: "",
        desc: "Orch/goal run statistics and model verification",
        group: CommandGroup::Session,
        needs_args: false,
    },
    // Orchestration
    SlashCommand {
        name: "/orch",
        aliases: &[],
        args: "<task>",
        desc: "Plan → implement → review loop across models",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    SlashCommand {
        name: "/goal",
        aliases: &[],
        args: "<goal> [--max N] [--check cmd]",
        desc: "Retry until a check or evaluator confirms the goal",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    SlashCommand {
        name: "/loop",
        aliases: &[],
        args: "[every] <prompt>",
        desc: "Repeat a prompt on an interval until stopped or done",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    SlashCommand {
        name: "/arena",
        aliases: &[],
        args: "[lineup=…] <task>",
        desc: "Same task, N models, isolated worktrees, blind judge",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    SlashCommand {
        name: "/worktree",
        aliases: &[],
        args: "<name>",
        desc: "Create a named git worktree",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    SlashCommand {
        name: "/roles",
        aliases: &[],
        args: "[role=model …]",
        desc: "Show or change orchestration roles/effort",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    SlashCommand {
        name: "/preset",
        aliases: &[],
        args: "[save|apply|delete <name>]",
        desc: "Save/apply role presets; Shift+Tab cycles",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    SlashCommand {
        name: "/btw",
        aliases: &[],
        args: "<question>",
        desc: "Side question in the background, history untouched",
        group: CommandGroup::Orchestration,
        needs_args: false,
    },
    // Context
    SlashCommand {
        name: "/compact",
        aliases: &[],
        args: "",
        desc: "Compact the conversation to free context",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/init",
        aliases: &[],
        args: "",
        desc: "Analyze the repo and write AGENTS.md",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/remember",
        aliases: &[],
        args: "<note>",
        desc: "Append a note to .goosehints project memory",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/skills",
        aliases: &[],
        args: "[<name>…]",
        desc: "List or load skills",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/prompts",
        aliases: &[],
        args: "[--extension <name>]",
        desc: "List available prompts",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/prompt",
        aliases: &[],
        args: "<name> [--info] [key=value…]",
        desc: "Get prompt info or run a prompt",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/recipe",
        aliases: &[],
        args: "[file.yaml]",
        desc: "Generate a recipe from this session",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/r",
        aliases: &[],
        args: "",
        desc: "Toggle full tool output",
        group: CommandGroup::Context,
        needs_args: false,
    },
    SlashCommand {
        name: "/edit",
        aliases: &[],
        args: "[text]",
        desc: "Compose the message in your editor",
        group: CommandGroup::Context,
        needs_args: false,
    },
    // Config
    SlashCommand {
        name: "/model",
        aliases: &[],
        args: "[provider/]model",
        desc: "Pick or switch provider/model (e.g. /model codex-acp/gpt-5.5)",
        group: CommandGroup::Config,
        needs_args: false,
    },
    SlashCommand {
        name: "/effort",
        aliases: &[],
        args: "[target] <level>",
        desc: "Pick or set reasoning effort (low/medium/high/xhigh)",
        group: CommandGroup::Config,
        needs_args: false,
    },
    SlashCommand {
        name: "/mode",
        aliases: &[],
        args: "<name>",
        desc: "Set goose mode (auto/approve/smart_approve/chat)",
        group: CommandGroup::Config,
        needs_args: true,
    },
    SlashCommand {
        name: "/extension",
        aliases: &[],
        args: "<command>",
        desc: "Add a stdio extension",
        group: CommandGroup::Config,
        needs_args: true,
    },
    SlashCommand {
        name: "/builtin",
        aliases: &[],
        args: "<names>",
        desc: "Add builtin extensions by name",
        group: CommandGroup::Config,
        needs_args: true,
    },
    SlashCommand {
        name: "/t",
        aliases: &[],
        args: "[name]",
        desc: "Toggle or set theme (light/dark/ansi)",
        group: CommandGroup::Config,
        needs_args: false,
    },
    SlashCommand {
        name: "/terminal-setup",
        aliases: &[],
        args: "",
        desc: "Configure Shift+Enter newline for this terminal",
        group: CommandGroup::Config,
        needs_args: false,
    },
];

/// `(token, description)` pairs for every command and alias, used by Tab
/// completion and the inline typing hint.
pub fn completion_pairs() -> Vec<(&'static str, &'static str)> {
    let mut pairs = Vec::with_capacity(COMMANDS.len());
    for command in COMMANDS {
        pairs.push((command.name, command.desc));
        for alias in command.aliases {
            pairs.push((*alias, command.desc));
        }
    }
    pairs
}

/// Whether a bare invocation of `token` still needs an argument to do anything.
pub fn requires_args(token: &str) -> bool {
    COMMANDS
        .iter()
        .find(|command| command.name == token || command.aliases.contains(&token))
        .map(|command| command.needs_args)
        .unwrap_or(false)
}

/// Every command name and alias, for nearest-match search and coverage tests.
pub fn all_tokens() -> Vec<&'static str> {
    let mut tokens = Vec::new();
    for command in COMMANDS {
        tokens.push(command.name);
        tokens.extend_from_slice(command.aliases);
    }
    tokens
}

/// Suggest the closest known command to an unrecognized slash token, or `None`
/// when nothing is near enough to be a likely typo.
pub fn nearest_command(token: &str) -> Option<&'static str> {
    const MAX_DISTANCE: usize = 2;
    let tokens = all_tokens();
    if tokens.contains(&token) {
        return None;
    }
    tokens
        .into_iter()
        .filter_map(|candidate| {
            let distance = levenshtein(token, candidate);
            (distance > 0 && distance <= MAX_DISTANCE).then_some((distance, candidate))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0usize; b_chars.len() + 1];
    for (i, a_char) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &b_char) in b_chars.iter().enumerate() {
            let cost = usize::from(a_char != b_char);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

/// Grouped, column-aligned command listing for `/help` (keybindings are
/// appended by the caller).
pub fn help_text() -> String {
    let mut out = String::new();
    for group in CommandGroup::ORDER {
        out.push_str(group.title());
        out.push('\n');
        let commands: Vec<&SlashCommand> = COMMANDS
            .iter()
            .filter(|command| command.group == group)
            .collect();
        // Align the description column within each group so short commands do
        // not trail a full screen of padding after the longest one.
        let width = commands
            .iter()
            .map(|command| command.invocation().chars().count())
            .max()
            .unwrap_or(0);
        for command in commands {
            out.push_str(&format!(
                "  {:<width$}  {}\n",
                command.invocation(),
                command.desc,
                width = width
            ));
        }
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tokens_are_unique() {
        let tokens = all_tokens();
        let unique: HashSet<_> = tokens.iter().collect();
        assert_eq!(tokens.len(), unique.len(), "duplicate command token");
    }

    #[test]
    fn nearest_command_catches_typos_but_not_nonsense() {
        assert_eq!(nearest_command("/statuss"), Some("/status"));
        assert_eq!(nearest_command("/hlep"), Some("/help"));
        assert_eq!(nearest_command("/quti"), Some("/quit"));
        assert_eq!(nearest_command("/xyzzy"), None);
        // An exact match is not a typo.
        assert_eq!(nearest_command("/status"), None);
    }

    #[test]
    fn help_text_lists_every_command_exactly_once() {
        let help = help_text();
        for command in COMMANDS {
            let occurrences = help.match_indices(command.desc).count();
            assert_eq!(
                occurrences, 1,
                "{} should appear exactly once in /help",
                command.name
            );
        }
        // Every group header is present.
        for group in CommandGroup::ORDER {
            assert!(help.contains(group.title()));
        }
    }
}
