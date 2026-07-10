use super::api_client::TlsConfig;
use super::base::{ConfigKey, ModelInfo, Provider, ProviderDef, ProviderMetadata, ProviderType};
use crate::config::{DeclarativeProviderConfig, ExtensionConfig};
use anyhow::Result;
use futures::future::BoxFuture;
use goose_providers::model::ModelConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub type ProviderConstructor = Arc<
    dyn Fn(
            Vec<ExtensionConfig>,
            Option<PathBuf>,
            Option<TlsConfig>,
        ) -> BoxFuture<'static, Result<Arc<dyn Provider>>>
        + Send
        + Sync,
>;

pub type ProviderCleanup = Arc<dyn Fn() -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// Actionable guidance for a provider name that gooseherd removed. Returns
/// `None` for names that were never a gooseherd provider (genuine typos).
pub fn removed_provider_hint(name: &str) -> Option<&'static str> {
    Some(match name {
        "claude-code" => "use 'claude-acp' instead (run `goose herd` to install the adapter)",
        "codex" | "chatgpt_codex" => {
            "use 'codex-acp' instead (run `goose herd` to install the adapter)"
        }
        "github_copilot" => "use 'copilot-acp' instead (run `goose herd` to install the adapter)",
        "gemini-cli" | "gemini_oauth" => {
            "use an ACP agent (set GOOSE_ACP_AGENTS to run `gemini --acp`)"
        }
        "cursor-agent" => "use an ACP agent via GOOSE_ACP_AGENTS",
        "litellm" | "tetrate" | "nano-gpt" | "avian" | "azure_openai" | "databricks"
        | "databricks_v2" | "snowflake" | "huggingface" | "gcp_vertex_ai" | "google" | "xai"
        | "xai_oauth" | "kimi_code" | "bedrock" | "sagemaker" => {
            "add a custom provider JSON in ~/.config/goose/custom_providers/ (openai-compatible)"
        }
        _ => return None,
    })
}

/// Build a removed-provider error for a name that used to resolve, or `None`
/// when the name was never a known provider.
pub fn removed_provider_error(name: &str) -> Option<anyhow::Error> {
    removed_provider_hint(name)
        .map(|hint| anyhow::anyhow!("provider '{name}' was removed in gooseherd — {hint}"))
}

#[derive(Clone)]
pub struct ProviderEntry {
    metadata: ProviderMetadata,
    pub(crate) constructor: ProviderConstructor,
    pub(crate) cleanup: Option<ProviderCleanup>,
    provider_type: ProviderType,
    tls_config: Option<TlsConfig>,
}

impl ProviderEntry {
    pub fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    pub fn provider_type(&self) -> ProviderType {
        self.provider_type
    }

    /// Apply provider-specific normalization to a model config: materialize
    /// global defaults and backfill `context_limit` from the provider's known
    /// models when the canonical registry didn't already resolve one. Used by
    /// the agent/session layer to resolve effective limits (e.g. for custom
    /// providers that declare explicit context limits in their config).
    pub fn normalize_model_config(&self, mut model: ModelConfig) -> Result<ModelConfig> {
        model = crate::model_config::materialize_model_config(&self.metadata.name, model)?;

        if model.context_limit.is_none() {
            if let Some(info) = self
                .metadata
                .known_models
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(&model.model_name) && m.context_limit > 0)
            {
                model.context_limit = Some(info.context_limit);
            }
        }

        Ok(model)
    }

    pub async fn create_with_default_model(
        &self,
        extensions: Vec<ExtensionConfig>,
    ) -> Result<Arc<dyn Provider>> {
        self.create(extensions).await
    }

    pub async fn create(&self, extensions: Vec<ExtensionConfig>) -> Result<Arc<dyn Provider>> {
        (self.constructor)(extensions, None, self.tls_config.clone()).await
    }

    pub async fn create_with_working_dir(
        &self,
        extensions: Vec<ExtensionConfig>,
        working_dir: PathBuf,
    ) -> Result<Arc<dyn Provider>> {
        (self.constructor)(extensions, Some(working_dir), self.tls_config.clone()).await
    }
}

#[derive(Default)]
pub struct ProviderRegistry {
    pub(crate) entries: HashMap<String, ProviderEntry>,
    tls_config: Option<TlsConfig>,
}

impl ProviderRegistry {
    pub fn new(tls_config: Option<TlsConfig>) -> Self {
        Self {
            entries: HashMap::new(),
            tls_config,
        }
    }

    pub fn register<F>(&mut self, preferred: bool)
    where
        F: ProviderDef + 'static,
    {
        let metadata = F::metadata();
        let name = metadata.name.clone();

        self.entries.insert(
            name,
            ProviderEntry {
                metadata,
                constructor: Arc::new(|extensions, working_dir, tls_config| {
                    Box::pin(async move {
                        let provider = match working_dir {
                            Some(working_dir) => {
                                F::from_env_with_working_dir(extensions, working_dir, tls_config)
                                    .await?
                            }
                            None => F::from_env(extensions, tls_config).await?,
                        };
                        Ok(Arc::new(provider) as Arc<dyn Provider>)
                    })
                }),
                cleanup: None,
                provider_type: if preferred {
                    ProviderType::Preferred
                } else {
                    ProviderType::Builtin
                },
                tls_config: self.tls_config.clone(),
            },
        );
    }

    pub fn register_acp_agent(
        &mut self,
        metadata: ProviderMetadata,
        provider_type: ProviderType,
        constructor: ProviderConstructor,
    ) {
        let name = metadata.name.clone();
        self.entries.insert(
            name,
            ProviderEntry {
                metadata,
                constructor,
                cleanup: None,
                provider_type,
                tls_config: self.tls_config.clone(),
            },
        );
    }

    pub fn register_with_name<P, F>(
        &mut self,
        config: &DeclarativeProviderConfig,
        provider_type: ProviderType,
        constructor: F,
    ) where
        P: ProviderDef + 'static,
        F: Fn(Option<TlsConfig>) -> Result<P::Provider> + Send + Sync + 'static,
    {
        let base_metadata = P::metadata();
        let description = config
            .description
            .clone()
            .unwrap_or_else(|| format!("Custom {} provider", config.display_name));
        let default_model = config
            .models
            .first()
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let known_models: Vec<ModelInfo> = config
            .models
            .iter()
            .map(|m| ModelInfo {
                name: m.name.clone(),
                resolved_model: None,
                context_limit: m.context_limit,
                input_token_cost: m.input_token_cost,
                output_token_cost: m.output_token_cost,
                currency: m.currency.clone(),
                supports_cache_control: Some(m.supports_cache_control.unwrap_or(false)),
                reasoning: m.reasoning,
            })
            .collect();

        let mut config_keys = if provider_type == ProviderType::Declarative {
            if !config.api_key_env.is_empty() {
                vec![ConfigKey::new(
                    &config.api_key_env,
                    config.requires_auth,
                    true,
                    None,
                    true,
                )]
            } else {
                Vec::new()
            }
        } else {
            let mut config_keys = base_metadata.config_keys.clone();

            if let Some(api_key_index) = config_keys.iter().position(|key| key.secret) {
                if !config.requires_auth {
                    config_keys.remove(api_key_index);
                } else if !config.api_key_env.is_empty() {
                    config_keys[api_key_index] =
                        ConfigKey::new(&config.api_key_env, false, true, None, true);
                }
            }

            config_keys
        };

        if let Some(ref env_vars) = config.env_vars {
            for ev in env_vars {
                // Default primary to `required` so required fields show prominently in the UI
                let primary = ev.primary.unwrap_or(ev.required);
                config_keys.push(ConfigKey::new(
                    &ev.name,
                    ev.required,
                    ev.secret,
                    ev.default.as_deref(),
                    primary,
                ));
            }
        }

        let custom_metadata = ProviderMetadata {
            name: config.name.clone(),
            display_name: config.display_name.clone(),
            description,
            default_model,
            known_models,
            model_doc_link: config
                .model_doc_link
                .clone()
                .unwrap_or(base_metadata.model_doc_link),
            config_keys,
            setup_steps: config.setup_steps.clone(),
            model_selection_hint: None,
            fast_model: config.fast_model.clone(),
        };

        self.entries.insert(
            config.name.clone(),
            ProviderEntry {
                metadata: custom_metadata,
                constructor: Arc::new(move |_extensions, _working_dir, tls_config| {
                    let result = constructor(tls_config);
                    Box::pin(async move {
                        let provider = result?;
                        Ok(Arc::new(provider) as Arc<dyn Provider>)
                    })
                }),
                cleanup: None,
                provider_type,
                tls_config: self.tls_config.clone(),
            },
        );
    }

    pub fn set_cleanup(&mut self, name: &str, cleanup: ProviderCleanup) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.cleanup = Some(cleanup);
        }
    }

    pub fn with_providers<F>(mut self, setup: F) -> Self
    where
        F: FnOnce(&mut Self),
    {
        setup(&mut self);
        self
    }

    pub async fn create(
        &self,
        name: &str,
        extensions: Vec<ExtensionConfig>,
    ) -> Result<Arc<dyn Provider>> {
        let entry = self.entries.get(name).ok_or_else(|| {
            removed_provider_error(name)
                .unwrap_or_else(|| anyhow::anyhow!("Unknown provider: {}", name))
        })?;

        entry.create(extensions).await
    }

    pub fn all_metadata_with_types(&self) -> Vec<(ProviderMetadata, ProviderType)> {
        self.entries
            .values()
            .map(|e| (e.metadata.clone(), e.provider_type))
            .collect()
    }

    pub fn remove_custom_providers(&mut self) {
        self.entries.retain(|name, _| !name.starts_with("custom_"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::declarative_providers::ProviderEngine;
    use crate::providers::openai_def::OpenAiProviderDef;

    fn test_config() -> DeclarativeProviderConfig {
        DeclarativeProviderConfig {
            name: "custom_hf".to_string(),
            engine: ProviderEngine::OpenAI,
            display_name: "Custom HF".to_string(),
            description: None,
            api_key_env: String::new(),
            base_url: "https://router.huggingface.co/v1".to_string(),
            models: vec![ModelInfo::new("test-model", 128_000)],
            headers: None,
            timeout_seconds: None,
            supports_streaming: Some(true),
            requires_auth: true,
            catalog_provider_id: Some("huggingface".to_string()),
            base_path: None,
            env_vars: None,
            dynamic_models: None,
            skip_canonical_filtering: false,
            model_doc_link: None,
            setup_steps: vec![],
            fast_model: None,
            preserves_thinking: false,
        }
    }

    #[test]
    fn register_with_name_uses_config_name() {
        let mut registry = ProviderRegistry::new(None);
        registry.register_with_name::<OpenAiProviderDef, _>(
            &test_config(),
            ProviderType::Declarative,
            |_| unreachable!("constructor is not used by this test"),
        );

        let entry = registry.entries.get("custom_hf").unwrap();
        assert_eq!(entry.metadata().name, "custom_hf");
        assert_eq!(entry.provider_type(), ProviderType::Declarative);
    }

    #[test]
    fn register_acp_agent_uses_runtime_metadata() {
        let mut registry = ProviderRegistry::new(None);
        let metadata = ProviderMetadata::new(
            "gemini-acp",
            "Gemini (ACP)",
            "Configured ACP agent.",
            "current",
            vec![],
            "",
            vec![],
        );
        let constructor: ProviderConstructor = Arc::new(|_, _, _| {
            Box::pin(async { unreachable!("constructor is not used by this test") })
        });

        registry.register_acp_agent(metadata, ProviderType::Custom, constructor);

        let entry = registry.entries.get("gemini-acp").unwrap();
        assert_eq!(entry.metadata().display_name, "Gemini (ACP)");
        assert_eq!(entry.provider_type(), ProviderType::Custom);
    }

    #[test]
    fn removed_provider_names_map_to_actionable_hints() {
        assert!(removed_provider_hint("claude-code")
            .unwrap()
            .contains("claude-acp"));
        assert!(removed_provider_hint("chatgpt_codex")
            .unwrap()
            .contains("codex-acp"));
        assert!(removed_provider_hint("github_copilot")
            .unwrap()
            .contains("copilot-acp"));
        assert!(removed_provider_hint("databricks")
            .unwrap()
            .contains("custom_providers"));
        assert!(removed_provider_hint("not-a-real-provider").is_none());
    }
}
