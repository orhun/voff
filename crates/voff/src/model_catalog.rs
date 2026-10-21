//! Offline provider model picker backed by llm-kernel's embedded catalog.

use llm_kernel::provider::ProviderIndex;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelOption {
    pub id: String,
}

#[derive(Clone, Debug)]
pub struct ModelCatalog {
    ollama: Vec<ModelOption>,
    openai: Vec<ModelOption>,
    anthropic: Vec<ModelOption>,
    cli: Vec<ModelOption>,
}

impl Default for ModelCatalog {
    fn default() -> Self {
        let catalog = ProviderIndex::embedded();
        let options = |provider| {
            let mut models = catalog
                .models_for(provider)
                .iter()
                .filter(|model| {
                    model.capabilities.as_ref().is_some_and(|capabilities| {
                        capabilities.tool_call && capabilities.streaming
                    })
                })
                .collect::<Vec<_>>();
            models.sort_by(|left, right| {
                right
                    .release_date
                    .cmp(&left.release_date)
                    .then_with(|| left.id.cmp(&right.id))
            });
            models
                .into_iter()
                .map(|model| ModelOption {
                    id: model.id.clone(),
                })
                .collect::<Vec<_>>()
        };
        Self {
            ollama: options("ollama"),
            openai: options("openai"),
            // Keep a short, current Claude API list here instead of exposing
            // the embedded catalog's retired Anthropic snapshots.
            anthropic: [
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-fable-5-1",
                "claude-haiku-5-5",
            ]
            .into_iter()
            .map(|id| ModelOption { id: id.into() })
            .collect(),
            cli: vec![ModelOption {
                id: "default".into(),
            }],
        }
    }
}

impl ModelCatalog {
    #[must_use]
    pub fn models(&self, provider: &str) -> &[ModelOption] {
        match provider {
            "ollama" => &self.ollama,
            "openai" => &self.openai,
            "anthropic" => &self.anthropic,
            "codex_cli" | "claude_cli" => &self.cli,
            _ => &[],
        }
    }

    #[must_use]
    pub fn cycle_model(&self, provider: &str, current: &str, delta: isize) -> String {
        let models = self.models(provider);
        if models.is_empty() {
            return current.to_owned();
        }
        let current = models
            .iter()
            .position(|option| option.id == current)
            .unwrap_or(if delta > 0 { models.len() - 1 } else { 0 });
        let next = (current as isize + delta).rem_euclid(models.len() as isize) as usize;
        models[next].id.clone()
    }

    #[must_use]
    pub fn first_model(&self, provider: &str) -> String {
        self.models(provider)
            .first()
            .map_or_else(String::new, |option| option.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_has_provider_models() {
        let catalog = ModelCatalog::default();
        assert!(!catalog.models("openai").is_empty());
        assert!(!catalog.models("ollama").is_empty());
        assert_eq!(catalog.first_model("codex_cli"), "default");
    }

    #[test]
    fn custom_model_can_enter_picker() {
        let catalog = ModelCatalog::default();
        let models = catalog.models("openai");
        assert_eq!(catalog.cycle_model("openai", "custom", 1), models[0].id);
        assert_eq!(
            catalog.cycle_model("openai", "custom", -1),
            models.last().unwrap().id
        );
    }
}
