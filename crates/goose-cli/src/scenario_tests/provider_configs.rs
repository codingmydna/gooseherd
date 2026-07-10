//! Providers for the scenario tests. Keep in sync with
//! goose/crates/goose/src/providers/factory.rs

use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub name: &'static str,
    pub model_name: &'static str,
    pub required_env_vars: &'static [&'static str],
    pub env_modifications: Option<HashMap<&'static str, Option<String>>>,
    pub skip_reason: Option<&'static str>,
}

impl ProviderConfig {
    fn simple_skip(
        name: &'static str,
        model_name: &'static str,
        skip_reason: Option<&'static str>,
    ) -> Self {
        let key = format!("{}_API_KEY", name.to_uppercase());
        let required_env_vars =
            Box::leak(vec![Box::leak(key.into_boxed_str()) as &str].into_boxed_slice());

        Self {
            name,
            model_name,
            required_env_vars,
            env_modifications: None,
            skip_reason,
        }
    }

    pub fn simple(name: &'static str, model_name: &'static str) -> Self {
        Self::simple_skip(name, model_name, None)
    }

    pub fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }
}

static PROVIDER_CONFIGS: LazyLock<Vec<ProviderConfig>> = LazyLock::new(|| {
    vec![
        ProviderConfig::simple("openai", "gpt-4o"),
        ProviderConfig::simple("anthropic", "claude-sonnet-4-20250514"),
        ProviderConfig::simple("groq", "llama-3.3-70b-versatile"),
        ProviderConfig::simple_skip(
            "openrouter",
            "anthropic/claude-sonnet-4",
            Some("Key is no longer valid"),
        ),
        ProviderConfig::simple_skip("ollama", "qwen3", Some("Ollama not supported")),
        ProviderConfig::simple_skip("venice", "llama-3.3-70b", Some("No keys available")),
    ]
});

pub fn get_provider_configs() -> Vec<&'static ProviderConfig> {
    PROVIDER_CONFIGS
        .iter()
        .filter(|config| !config.is_skipped())
        .collect()
}
