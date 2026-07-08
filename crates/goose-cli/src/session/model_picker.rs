use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ModelPickerTarget {
    Session,
    Planner,
    Implementer,
    Reviewer,
}

impl ModelPickerTarget {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Planner => "planner",
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
        }
    }

    pub(super) fn role_name(self) -> Option<&'static str> {
        match self {
            Self::Session => None,
            Self::Planner => Some("planner"),
            Self::Implementer => Some("implementer"),
            Self::Reviewer => Some("reviewer"),
        }
    }

    pub(super) fn all() -> [Self; 4] {
        [
            Self::Session,
            Self::Planner,
            Self::Implementer,
            Self::Reviewer,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModelPickerOutcome {
    Selected {
        target: ModelPickerTarget,
        provider: String,
        model: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModelPickerPhase {
    Target,
    Provider {
        target: ModelPickerTarget,
    },
    Model {
        target: ModelPickerTarget,
        provider: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelPickerState {
    phase: ModelPickerPhase,
}

impl ModelPickerState {
    pub(super) fn new() -> Self {
        Self {
            phase: ModelPickerPhase::Target,
        }
    }

    pub(super) fn select_target(self, target: ModelPickerTarget) -> Self {
        Self {
            phase: ModelPickerPhase::Provider { target },
        }
    }

    pub(super) fn select_provider(self, provider: impl Into<String>) -> Self {
        match self.phase {
            ModelPickerPhase::Provider { target } => Self {
                phase: ModelPickerPhase::Model {
                    target,
                    provider: provider.into(),
                },
            },
            _ => self,
        }
    }

    pub(super) fn select_model(self, model: impl Into<String>) -> ModelPickerOutcome {
        match self.phase {
            ModelPickerPhase::Model { target, provider } => ModelPickerOutcome::Selected {
                target,
                provider,
                model: model.into(),
            },
            _ => ModelPickerOutcome::Cancelled,
        }
    }

    pub(super) fn cancel(self) -> ModelPickerOutcome {
        ModelPickerOutcome::Cancelled
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MenuOption<T> {
    pub(super) value: T,
    pub(super) label: String,
    pub(super) hint: String,
    pub(super) selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TargetStatus {
    pub(super) target: ModelPickerTarget,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelPickerProvider {
    pub(super) name: String,
    pub(super) display_name: String,
    pub(super) default_model: String,
    pub(super) models: Vec<String>,
}

impl From<goose::providers::base::ProviderMetadata> for ModelPickerProvider {
    fn from(metadata: goose::providers::base::ProviderMetadata) -> Self {
        Self {
            name: metadata.name,
            display_name: metadata.display_name,
            default_model: metadata.default_model,
            models: metadata
                .known_models
                .into_iter()
                .map(|model| model.name)
                .collect(),
        }
    }
}

pub(super) const DIRECT_MODEL_INPUT: &str = "__goose_direct_model_input__";

pub(super) fn build_target_options(
    statuses: &[TargetStatus],
) -> Vec<MenuOption<ModelPickerTarget>> {
    ModelPickerTarget::all()
        .into_iter()
        .filter_map(|target| {
            statuses
                .iter()
                .find(|status| status.target == target)
                .map(|status| {
                    let mut hint = format!("{}/{}", status.provider, status.model);
                    if let Some(effort) = &status.effort {
                        hint.push_str(&format!(" · effort={effort}"));
                    }
                    MenuOption {
                        value: target,
                        label: target.label().to_string(),
                        hint,
                        selected: false,
                    }
                })
        })
        .collect()
}

pub(super) fn build_provider_options(
    providers: &[ModelPickerProvider],
    current_provider: Option<&str>,
) -> Vec<MenuOption<String>> {
    providers
        .iter()
        .map(|provider| {
            let selected = current_provider == Some(provider.name.as_str());
            let label = selected_label(
                selected,
                &format!("{} ({})", provider.display_name, provider.name),
            );
            MenuOption {
                value: provider.name.clone(),
                label,
                hint: format!("default: {}", provider.default_model),
                selected,
            }
        })
        .collect()
}

pub(super) fn build_model_options(
    provider: &ModelPickerProvider,
    current_model: Option<&str>,
) -> Vec<MenuOption<String>> {
    let mut seen = HashSet::new();
    let mut options = Vec::new();

    if !provider.default_model.is_empty() && seen.insert(provider.default_model.clone()) {
        let selected = current_model == Some(provider.default_model.as_str());
        options.push(MenuOption {
            value: provider.default_model.clone(),
            label: selected_label(selected, &format!("{} (default)", provider.default_model)),
            hint: "provider default alias".to_string(),
            selected,
        });
    }

    for model in &provider.models {
        if !model.is_empty() && seen.insert(model.clone()) {
            let selected = current_model == Some(model.as_str());
            options.push(MenuOption {
                value: model.clone(),
                label: selected_label(selected, model),
                hint: "known model".to_string(),
                selected,
            });
        }
    }

    if let Some(model) = current_model {
        if !model.is_empty() && seen.insert(model.to_string()) {
            options.push(MenuOption {
                value: model.to_string(),
                label: selected_label(true, model),
                hint: "current custom model".to_string(),
                selected: true,
            });
        }
    }

    options.push(MenuOption {
        value: DIRECT_MODEL_INPUT.to_string(),
        label: "Direct input".to_string(),
        hint: "type a model id".to_string(),
        selected: false,
    });

    options
}

pub(super) fn build_effort_options(current_effort: Option<&str>) -> Vec<MenuOption<String>> {
    [
        ("low", "fastest; least reasoning"),
        ("medium", "balanced speed and reasoning"),
        ("high", "slower; smarter reasoning"),
        ("xhigh", "slowest; deepest reasoning"),
    ]
    .into_iter()
    .map(|(level, hint)| {
        let selected = current_effort.and_then(normalize_effort_label) == Some(level);
        MenuOption {
            value: level.to_string(),
            label: selected_label(selected, level),
            hint: hint.to_string(),
            selected,
        }
    })
    .collect()
}

pub(super) fn parse_target(value: &str) -> Option<ModelPickerTarget> {
    match value.trim().to_ascii_lowercase().as_str() {
        "session" => Some(ModelPickerTarget::Session),
        "planner" => Some(ModelPickerTarget::Planner),
        "implementer" | "impl" => Some(ModelPickerTarget::Implementer),
        "reviewer" | "review" => Some(ModelPickerTarget::Reviewer),
        _ => None,
    }
}

pub(super) fn normalize_effort_label(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" | "med" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "max" => Some("xhigh"),
        _ => None,
    }
}

fn selected_label(selected: bool, label: &str) -> String {
    if selected {
        format!("✓ {label}")
    } else {
        label.to_string()
    }
}
