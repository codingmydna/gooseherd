use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use crate::acp::{
    extension_configs_to_mcp_servers, AcpProvider, AcpProviderConfig, ACP_CURRENT_MODEL,
};
use crate::config::search_path::SearchPaths;
use crate::config::{Config, ConfigError, ExtensionConfig, GooseMode};
use crate::providers::base::{current_working_dir, Provider, ProviderMetadata, ProviderType};
use crate::providers::inventory::{InventoryIdentityInput, InventoryRegistration};
use crate::providers::provider_registry::{ProviderConstructor, ProviderRegistry};

pub const GOOSE_ACP_AGENTS_KEY: &str = "GOOSE_ACP_AGENTS";
const GENERIC_ACP_DOC_URL: &str = "https://agentclientprotocol.com";
const ACP_DEFAULT_MODEL_ALIAS: &str = "default";
static EMPTY_ENV: Lazy<BTreeMap<String, String>> = Lazy::new(BTreeMap::new);
static ENV_REF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AcpAgentSpec {
    Command(String),
    Detailed {
        command: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        env_remove: Vec<String>,
    },
}

impl AcpAgentSpec {
    pub fn command(&self) -> &str {
        match self {
            Self::Command(command) | Self::Detailed { command, .. } => command,
        }
    }

    pub fn env(&self) -> &BTreeMap<String, String> {
        match self {
            Self::Command(_) => &EMPTY_ENV,
            Self::Detailed { env, .. } => env,
        }
    }

    pub fn env_remove(&self) -> &[String] {
        match self {
            Self::Command(_) => &[],
            Self::Detailed { env_remove, .. } => env_remove,
        }
    }
}

pub fn parse_acp_command(command: &str) -> Result<(String, Vec<String>)> {
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("GOOSE_ACP_AGENTS command cannot be empty"))?
        .to_string();
    let args = parts.map(str::to_string).collect();
    Ok((program, args))
}

pub fn generic_acp_provider_name(key: &str) -> String {
    format!("{}-acp", key.trim())
}

pub fn generic_acp_display_name(key: &str) -> String {
    let label = key.trim().replace(['-', '_'], " ");
    if label.is_empty() {
        return "ACP Agent".to_string();
    }

    let titled = label
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    format!("{titled} (ACP)")
}

pub fn is_default_model_alias(model: &str) -> bool {
    matches!(model, ACP_CURRENT_MODEL | ACP_DEFAULT_MODEL_ALIAS)
}

pub fn read_acp_agents(config: &Config) -> Result<BTreeMap<String, AcpAgentSpec>> {
    match config.get_param::<BTreeMap<String, AcpAgentSpec>>(GOOSE_ACP_AGENTS_KEY) {
        Ok(agents) => Ok(agents),
        Err(ConfigError::NotFound(_)) => Ok(BTreeMap::new()),
        Err(err) => Err(err.into()),
    }
}

pub fn env_var_refs(value: &str) -> Vec<String> {
    ENV_REF_RE
        .captures_iter(value)
        .filter_map(|captures| captures.get(1).map(|match_| match_.as_str().to_string()))
        .collect()
}

pub fn generic_acp_metadata(key: &str) -> ProviderMetadata {
    let name = generic_acp_provider_name(key);
    let display_name = generic_acp_display_name(key);
    let mut metadata = ProviderMetadata::new(
        &name,
        &display_name,
        &format!("Use goose with the configured {key} ACP agent."),
        ACP_CURRENT_MODEL,
        vec![],
        GENERIC_ACP_DOC_URL,
        vec![],
    );
    metadata.setup_steps = vec![
        format!("Install the {key} ACP agent CLI and ensure it is on PATH."),
        format!(
            "Add it to {GOOSE_ACP_AGENTS_KEY} in your goose config file, for example: {key}: \"{key} --acp\""
        ),
        "Restart goose for changes to take effect.".to_string(),
    ];
    metadata.model_selection_hint = Some(format!("Use the {key} ACP agent to configure models."));
    metadata
}

pub fn session_config_options_for_model(model: &str) -> Vec<(String, String)> {
    if is_default_model_alias(model) {
        vec![]
    } else {
        vec![("model".to_string(), model.to_string())]
    }
}

pub fn register_generic_acp_providers(registry: &mut ProviderRegistry) -> Result<usize> {
    let agents = read_acp_agents(Config::global())?;
    Ok(register_generic_acp_providers_from_agents(
        registry, &agents,
    ))
}

pub fn register_generic_acp_providers_from_config(
    registry: &mut ProviderRegistry,
    config: &Config,
) -> Result<usize> {
    let agents = read_acp_agents(config)?;
    Ok(register_generic_acp_providers_from_agents(
        registry, &agents,
    ))
}

pub fn register_generic_acp_providers_from_agents(
    registry: &mut ProviderRegistry,
    agents: &BTreeMap<String, AcpAgentSpec>,
) -> usize {
    let mut registered = 0;

    for (key, spec) in agents {
        let provider_name = generic_acp_provider_name(key);
        if registry.entries.contains_key(&provider_name) {
            tracing::warn!(
                provider = provider_name,
                key,
                "Skipping configured ACP agent because a provider with this name is already registered"
            );
            continue;
        }

        let (program, args) = match parse_acp_command(spec.command()) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(
                    key,
                    error = %error,
                    "Skipping configured ACP agent with invalid command"
                );
                continue;
            }
        };

        let metadata = generic_acp_metadata(key);
        let inventory = generic_acp_inventory(provider_name.clone(), program.clone());
        let constructor = generic_acp_constructor(
            provider_name,
            program,
            args,
            spec.env().clone(),
            spec.env_remove().to_vec(),
        );
        registry.register_acp_agent(metadata, ProviderType::Custom, Some(inventory), constructor);
        registered += 1;
    }

    registered
}

fn generic_acp_inventory(provider_name: String, program: String) -> InventoryRegistration {
    let identity_provider_name = provider_name.clone();
    let identity_program = program.clone();
    InventoryRegistration::new(false, move || {
        let resolved_command = resolve_acp_program(&identity_program)?;
        Ok(
            InventoryIdentityInput::new(&identity_provider_name, &identity_provider_name)
                .with_public("command", resolved_command.display().to_string()),
        )
    })
    .with_configured(move || resolve_acp_program(&program).is_ok())
}

fn generic_acp_constructor(
    provider_name: String,
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    env_remove: Vec<String>,
) -> ProviderConstructor {
    Arc::new(move |extensions, working_dir, _tls_config| {
        let provider_name = provider_name.clone();
        let program = program.clone();
        let args = args.clone();
        let env = env.clone();
        let env_remove = env_remove.clone();
        Box::pin(async move {
            let provider = connect_generic_acp_provider(
                provider_name,
                program,
                args,
                env,
                env_remove,
                extensions,
                working_dir.unwrap_or_else(current_working_dir),
            )
            .await?;
            Ok(Arc::new(provider) as Arc<dyn Provider>)
        })
    })
}

async fn connect_generic_acp_provider(
    provider_name: String,
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    env_remove: Vec<String>,
    extensions: Vec<ExtensionConfig>,
    working_dir: PathBuf,
) -> Result<AcpProvider> {
    let resolved_command = resolve_acp_program(&program)
        .map_err(|_| missing_acp_binary_error(&provider_name, &program))?;
    let config = Config::global();
    let goose_mode = config.get_goose_mode().unwrap_or(GooseMode::Auto);
    let plan_explore = config
        .get_param::<bool>("GOOSE_ACP_PLAN_EXPLORE")
        .unwrap_or(false);
    let model = config
        .get_goose_model()
        .unwrap_or_else(|_| ACP_CURRENT_MODEL.to_string());
    let resolved_env = resolve_agent_env(&provider_name, &env, config)?;

    let provider_config = AcpProviderConfig {
        command: resolved_command,
        args,
        env: resolved_env,
        env_remove,
        work_dir: working_dir,
        mcp_servers: extension_configs_to_mcp_servers(&extensions),
        session_mode_id: None,
        session_config_options: session_config_options_for_model(&model),
        model_config_option_id: Some("model".to_string()),
        mode_mapping: HashMap::new(),
        notification_callback: None,
        plan_explore,
    };

    AcpProvider::connect(provider_name, goose_mode, provider_config).await
}

fn resolve_agent_env(
    provider_name: &str,
    env: &BTreeMap<String, String>,
    config: &Config,
) -> Result<Vec<(String, String)>> {
    env.iter()
        .map(|(name, value)| {
            Ok((
                name.clone(),
                resolve_agent_env_value(provider_name, name, value, config)?,
            ))
        })
        .collect()
}

fn resolve_agent_env_value(
    provider_name: &str,
    env_name: &str,
    value: &str,
    config: &Config,
) -> Result<String> {
    let mut resolved = String::with_capacity(value.len());
    let mut last = 0;

    for captures in ENV_REF_RE.captures_iter(value) {
        let Some(full_match) = captures.get(0) else {
            continue;
        };
        let Some(var_match) = captures.get(1) else {
            continue;
        };
        let prefix = value.get(last..full_match.start()).ok_or_else(|| {
            anyhow!("{provider_name}: invalid env `{env_name}` reference boundary")
        })?;
        resolved.push_str(prefix);
        let key = var_match.as_str();
        let secret = config
            .get_secret::<String>(key)
            .map_err(|error| match error {
                ConfigError::NotFound(_) => anyhow!(
                    "{provider_name}: env `{env_name}` references `{key}` which is not set; export {key} or store it with `goose configure` (secret {key})"
                ),
                other => anyhow!(
                    "{provider_name}: failed to resolve env `{env_name}` reference `{key}`: {other}"
                ),
            })?;
        resolved.push_str(&secret);
        last = full_match.end();
    }

    let suffix = value
        .get(last..)
        .ok_or_else(|| anyhow!("{provider_name}: invalid env `{env_name}` reference boundary"))?;
    resolved.push_str(suffix);
    Ok(resolved)
}

fn resolve_acp_program(program: &str) -> Result<PathBuf> {
    SearchPaths::builder().with_npm().resolve(program)
}

fn missing_acp_binary_error(provider_name: &str, program: &str) -> anyhow::Error {
    anyhow!(
        "{program} not found for {provider_name} - install the ACP agent CLI, ensure `{program}` is on PATH, or update {GOOSE_ACP_AGENTS_KEY}. Then run `goose herd` to verify your setup."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::base::ProviderDescriptor;
    use crate::providers::claude_acp::ClaudeAcpProvider;
    use serial_test::serial;
    use test_case::test_case;

    fn test_config() -> Config {
        let config_file = tempfile::NamedTempFile::new().unwrap();
        let secrets_file = tempfile::NamedTempFile::new().unwrap();
        Config::new_with_file_secrets(config_file.path(), secrets_file.path()).unwrap()
    }

    #[test_case("gemini --acp", "gemini", vec!["--acp"])]
    #[test_case("  opencode   acp  --stdio  ", "opencode", vec!["acp", "--stdio"])]
    #[test_case("vibe-acp", "vibe-acp", Vec::<&str>::new())]
    fn parse_acp_command_splits_program_and_args(
        command: &str,
        expected_program: &str,
        expected_args: Vec<&str>,
    ) {
        let (program, args) = parse_acp_command(command).unwrap();

        assert_eq!(program, expected_program);
        assert_eq!(args, expected_args);
    }

    #[test]
    fn parse_acp_command_rejects_empty_commands() {
        let err = parse_acp_command("   ").unwrap_err();

        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn generic_metadata_is_key_based() {
        let metadata = generic_acp_metadata("open_code");

        assert_eq!(metadata.name, "open_code-acp");
        assert_eq!(metadata.display_name, "Open Code (ACP)");
        assert_eq!(metadata.default_model, ACP_CURRENT_MODEL);
        assert!(metadata.known_models.is_empty());
        assert!(metadata.config_keys.is_empty());
    }

    #[test_case(ACP_CURRENT_MODEL, true)]
    #[test_case("default", true)]
    #[test_case("gemini-2.5-pro", false)]
    fn default_model_aliases_are_not_session_config_options(model: &str, is_alias: bool) {
        let options = session_config_options_for_model(model);

        assert_eq!(options.is_empty(), is_alias);
    }

    #[test]
    fn read_acp_agents_reads_btreemap_from_config() {
        let config = test_config();
        let agents = BTreeMap::from([
            (
                "gemini".to_string(),
                AcpAgentSpec::Command("gemini --acp".to_string()),
            ),
            (
                "opencode".to_string(),
                AcpAgentSpec::Command("opencode acp".to_string()),
            ),
        ]);
        config.set_param(GOOSE_ACP_AGENTS_KEY, &agents).unwrap();

        assert_eq!(read_acp_agents(&config).unwrap(), agents);
    }

    #[test]
    fn read_acp_agents_accepts_string_and_detailed_specs() {
        let config = test_config();
        let agents = BTreeMap::from([
            (
                "gemini".to_string(),
                AcpAgentSpec::Command("gemini --acp".to_string()),
            ),
            (
                "glm".to_string(),
                AcpAgentSpec::Detailed {
                    command: "claude-agent-acp".to_string(),
                    env: BTreeMap::from([
                        (
                            "ANTHROPIC_BASE_URL".to_string(),
                            "https://api.z.ai/api/anthropic".to_string(),
                        ),
                        (
                            "ANTHROPIC_AUTH_TOKEN".to_string(),
                            "${ZAI_API_KEY}".to_string(),
                        ),
                    ]),
                    env_remove: vec!["CLAUDECODE".to_string()],
                },
            ),
        ]);
        config.set_param(GOOSE_ACP_AGENTS_KEY, &agents).unwrap();

        let parsed = read_acp_agents(&config).unwrap();

        assert_eq!(parsed.get("gemini").unwrap().command(), "gemini --acp");
        assert!(parsed.get("gemini").unwrap().env().is_empty());
        assert!(parsed.get("gemini").unwrap().env_remove().is_empty());
        assert_eq!(parsed.get("glm").unwrap().command(), "claude-agent-acp");
        assert_eq!(
            parsed.get("glm").unwrap().env().get("ANTHROPIC_BASE_URL"),
            Some(&"https://api.z.ai/api/anthropic".to_string())
        );
        assert_eq!(
            parsed.get("glm").unwrap().env().get("ANTHROPIC_AUTH_TOKEN"),
            Some(&"${ZAI_API_KEY}".to_string())
        );
        assert_eq!(
            parsed.get("glm").unwrap().env_remove(),
            ["CLAUDECODE".to_string()]
        );
    }

    #[test]
    fn resolve_agent_env_passes_literals_and_secret_refs() {
        let config = test_config();
        config.set_secret("ZAI_API_KEY", &"secret-token").unwrap();
        let env = BTreeMap::from([
            (
                "ANTHROPIC_BASE_URL".to_string(),
                "https://api.z.ai/api/anthropic".to_string(),
            ),
            (
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                "${ZAI_API_KEY}".to_string(),
            ),
        ]);

        let resolved = resolve_agent_env("glm-acp", &env, &config).unwrap();

        assert_eq!(
            resolved,
            vec![
                (
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    "secret-token".to_string()
                ),
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.z.ai/api/anthropic".to_string()
                ),
            ]
        );
    }

    #[test]
    #[serial]
    fn resolve_agent_env_expands_process_env_refs() {
        let config = test_config();
        let key = "GOOSE_TEST_GENERIC_ACP_ENV_TOKEN";
        std::env::set_var(key, "from-env");
        let env = BTreeMap::from([("ANTHROPIC_AUTH_TOKEN".to_string(), format!("${{{key}}}"))]);

        let resolved = resolve_agent_env("glm-acp", &env, &config).unwrap();
        std::env::remove_var(key);

        assert_eq!(
            resolved,
            vec![("ANTHROPIC_AUTH_TOKEN".to_string(), "from-env".to_string())]
        );
    }

    #[test]
    fn resolve_agent_env_reports_missing_ref_without_secret_value() {
        let config = test_config();
        let env = BTreeMap::from([(
            "ANTHROPIC_AUTH_TOKEN".to_string(),
            "${GOOSE_TEST_MISSING_ZAI_KEY}".to_string(),
        )]);

        let err = resolve_agent_env("glm-acp", &env, &config).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("glm-acp"));
        assert!(message.contains("ANTHROPIC_AUTH_TOKEN"));
        assert!(message.contains("GOOSE_TEST_MISSING_ZAI_KEY"));
        assert!(!message.contains("secret-token"));
    }

    #[test]
    fn register_generic_acp_providers_registers_configured_agent() {
        let mut registry = ProviderRegistry::new(None);
        let agents = BTreeMap::from([(
            "gemini".to_string(),
            AcpAgentSpec::Command("gemini --acp".to_string()),
        )]);

        let registered = register_generic_acp_providers_from_agents(&mut registry, &agents);

        assert_eq!(registered, 1);
        let entry = registry.entries.get("gemini-acp").unwrap();
        assert_eq!(entry.metadata().display_name, "Gemini (ACP)");
        assert_eq!(entry.metadata().default_model, ACP_CURRENT_MODEL);
        assert_eq!(entry.provider_type(), ProviderType::Custom);
    }

    #[test]
    fn register_generic_acp_providers_skips_builtin_collisions() {
        let mut registry = ProviderRegistry::new(None);
        registry.register::<ClaudeAcpProvider>(false);
        let agents = BTreeMap::from([(
            "claude".to_string(),
            AcpAgentSpec::Command("custom-claude --acp".to_string()),
        )]);
        let builtin_display = ClaudeAcpProvider::metadata().display_name;

        let registered = register_generic_acp_providers_from_agents(&mut registry, &agents);

        assert_eq!(registered, 0);
        let entry = registry.entries.get("claude-acp").unwrap();
        assert_eq!(entry.metadata().display_name, builtin_display);
        assert_eq!(entry.provider_type(), ProviderType::Builtin);
    }

    #[tokio::test]
    async fn constructor_reports_actionable_error_for_missing_program() {
        let mut registry = ProviderRegistry::new(None);
        let agents = BTreeMap::from([(
            "missing".to_string(),
            AcpAgentSpec::Command("missing-acp-binary-for-goose-test --acp".to_string()),
        )]);
        register_generic_acp_providers_from_agents(&mut registry, &agents);

        let entry = registry.entries.get("missing-acp").unwrap();
        let err = match entry
            .create_with_working_dir(vec![], PathBuf::from("."))
            .await
        {
            Ok(_) => panic!("expected missing binary error"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("missing-acp-binary-for-goose-test not found"));
        assert!(message.contains("install the ACP agent CLI"));
        assert!(message.contains(GOOSE_ACP_AGENTS_KEY));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn constructor_spawns_command_with_parsed_args_and_env() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-acp");
        let log = dir.path().join("args.log");
        std::fs::write(
            &script,
            "#!/bin/sh\n{\nprintf 'arg:%s\\n' \"$@\"\nprintf 'base:%s\\n' \"$ANTHROPIC_BASE_URL\"\nprintf 'token:%s\\n' \"$ANTHROPIC_AUTH_TOKEN\"\n} > \"$(dirname \"$0\")/args.log\"\nexit 1\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let mut registry = ProviderRegistry::new(None);
        let agents = BTreeMap::from([(
            "fake".to_string(),
            AcpAgentSpec::Detailed {
                command: format!("{} --acp extra", script.display()),
                env: BTreeMap::from([
                    (
                        "ANTHROPIC_BASE_URL".to_string(),
                        "https://api.z.ai/api/anthropic".to_string(),
                    ),
                    ("ANTHROPIC_AUTH_TOKEN".to_string(), "dummy".to_string()),
                ]),
                env_remove: vec![],
            },
        )]);
        register_generic_acp_providers_from_agents(&mut registry, &agents);

        let entry = registry.entries.get("fake-acp").unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            entry.create_with_working_dir(vec![], dir.path().to_path_buf()),
        )
        .await;

        assert!(matches!(result, Err(_) | Ok(Err(_))));
        for _ in 0..20 {
            if log.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            "arg:--acp\narg:extra\nbase:https://api.z.ai/api/anthropic\ntoken:dummy\n"
        );
    }
}
