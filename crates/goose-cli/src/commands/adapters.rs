use std::collections::BTreeMap;

use anyhow::{anyhow, Context, Result};
use goose::providers::generic_acp::{parse_acp_command, AcpAgentSpec};
use include_dir::{include_dir, Dir};
use serde::Deserialize;

static ADAPTERS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../adapters");

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AdapterStatus {
    Verified,
    Community,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct AdapterEntry {
    pub name: String,
    pub description: String,
    pub command: AcpAgentSpec,
    pub install: String,
    pub auth: String,
    #[serde(default)]
    pub models: Vec<String>,
    pub status: AdapterStatus,
    pub homepage: String,
}

pub fn catalog() -> Result<BTreeMap<String, AdapterEntry>> {
    let mut entries = BTreeMap::new();

    for file in ADAPTERS_DIR.files().filter(|file| {
        file.path()
            .extension()
            .is_some_and(|extension| extension == "yaml")
    }) {
        let path = file.path();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("invalid adapter catalog filename: {}", path.display()))?;
        let contents = file
            .contents_utf8()
            .ok_or_else(|| anyhow!("adapter catalog file is not UTF-8: {}", path.display()))?;
        let entry: AdapterEntry = serde_yaml::from_str(contents)
            .with_context(|| format!("failed to parse adapter catalog file {}", path.display()))?;

        if entry.name != stem {
            return Err(anyhow!(
                "adapter name `{}` does not match filename `{}`",
                entry.name,
                path.display()
            ));
        }
        parse_acp_command(entry.command.command()).with_context(|| {
            format!("invalid command in adapter catalog file {}", path.display())
        })?;

        if entries.insert(entry.name.clone(), entry).is_some() {
            return Err(anyhow!("duplicate adapter name `{stem}`"));
        }
    }

    Ok(entries)
}

pub fn example_model(entry: &AdapterEntry) -> &str {
    entry
        .models
        .first()
        .map(String::as_str)
        .unwrap_or("default")
}

pub fn unknown_name_error(name: &str, entries: &BTreeMap<String, AdapterEntry>) -> anyhow::Error {
    let available = entries
        .values()
        .map(|entry| format!("  {} — {}", entry.name, entry.description))
        .collect::<Vec<_>>()
        .join("\n");
    anyhow!("unknown agent `{name}`\n\nAvailable agents:\n{available}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_seed_adapters() {
        let entries = catalog().unwrap();
        assert_eq!(
            entries.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["gemini", "glm", "kimi", "opencode", "vibe"]
        );

        assert_eq!(
            entries["gemini"].command,
            AcpAgentSpec::Command("gemini --acp".to_string())
        );
        assert_eq!(entries["glm"].status, AdapterStatus::Community);
        assert_eq!(
            entries["glm"].command.env().get("ANTHROPIC_BASE_URL"),
            Some(&"https://api.z.ai/api/anthropic".to_string())
        );
        assert_eq!(
            entries["glm"].command.env().get("ANTHROPIC_AUTH_TOKEN"),
            Some(&"${ZAI_API_KEY}".to_string())
        );
    }

    #[test]
    fn every_adapter_has_a_valid_schema() {
        for (name, entry) in catalog().unwrap() {
            assert_eq!(name, entry.name);
            assert!(!entry.description.is_empty());
            assert!(!entry.install.is_empty());
            assert!(!entry.auth.is_empty());
            assert!(!entry.models.is_empty());
            assert!(entry.homepage.starts_with("http"));
            parse_acp_command(entry.command.command()).unwrap();
        }
    }

    #[test]
    fn unknown_name_error_lists_available_agents() {
        let entries = catalog().unwrap();
        let message = unknown_name_error("missing", &entries).to_string();

        assert!(message.contains("unknown agent `missing`"));
        for name in entries.keys() {
            assert!(message.contains(name));
        }
    }
}
