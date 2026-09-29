//! Provider-aware model choices and cost estimates for the application shell.

use ai_model_directory_router::{
    CostRequest, FlatModel, ModelModality, ModelStatus, RouterError, RouterStore,
    calculate_cost_for_model,
};
use std::collections::HashMap;
use voff_provider::ModelDescriptor;

const OPENAI: &str = "openai";
const OLLAMA: &str = "ollama";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5-mini";
const DEFAULT_OLLAMA_MODEL: &str = "qwen3:4b";
const DEFAULT_CLI_MODEL: &str = "default";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelOption {
    pub id: String,
    pub display_name: String,
    pub context_limit: Option<u64>,
    pub input_price: Option<String>,
    pub output_price: Option<String>,
}

impl ModelOption {
    #[must_use]
    pub fn price_label(&self) -> String {
        match (&self.input_price, &self.output_price) {
            (Some(input), Some(output)) => {
                format!("${input} input · ${output} output / 1M tokens")
            }
            _ => "price unavailable".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModelCatalog {
    openai: Vec<ModelOption>,
    ollama: Vec<ModelOption>,
    other: HashMap<String, Vec<ModelOption>>,
    priced_openai: HashMap<String, FlatModel>,
    ollama_discovered: bool,
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self {
            openai: vec![ModelOption {
                id: DEFAULT_OPENAI_MODEL.into(),
                display_name: DEFAULT_OPENAI_MODEL.into(),
                context_limit: None,
                input_price: None,
                output_price: None,
            }],
            ollama: vec![ModelOption {
                id: DEFAULT_OLLAMA_MODEL.into(),
                display_name: DEFAULT_OLLAMA_MODEL.into(),
                context_limit: None,
                input_price: None,
                output_price: None,
            }],
            other: HashMap::from([
                ("codex_cli".into(), fallback(DEFAULT_CLI_MODEL)),
                ("claude_cli".into(), fallback(DEFAULT_CLI_MODEL)),
            ]),
            priced_openai: HashMap::new(),
            ollama_discovered: false,
        }
    }
}

impl ModelCatalog {
    pub fn bundled() -> Result<Self, RouterError> {
        let store = RouterStore::bundled()?;
        let mut models = store
            .find_models_by_provider(OPENAI)
            .into_iter()
            .filter(compatible_with_voff)
            .cloned()
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            (right.id == DEFAULT_OPENAI_MODEL)
                .cmp(&(left.id == DEFAULT_OPENAI_MODEL))
                .then_with(|| right.release_date.cmp(&left.release_date))
                .then_with(|| left.id.cmp(&right.id))
        });
        let openai = models.iter().map(model_option).collect();
        let priced_openai = models
            .into_iter()
            .map(|model| (model.id.clone(), model))
            .collect();
        Ok(Self {
            openai,
            priced_openai,
            ..Self::default()
        })
    }

    pub fn set_ollama_models(&mut self, models: Vec<ModelDescriptor>) {
        if models.is_empty() {
            return;
        }
        self.ollama = models
            .into_iter()
            .map(|model| ModelOption {
                id: model.id,
                display_name: model.display_name,
                context_limit: model.context_limit,
                input_price: None,
                output_price: None,
            })
            .collect();
        self.ollama.sort_by(|left, right| left.id.cmp(&right.id));
        self.ollama_discovered = true;
    }

    #[must_use]
    pub fn models(&self, provider: &str) -> &[ModelOption] {
        match provider {
            OPENAI => &self.openai,
            OLLAMA => &self.ollama,
            _ => self.other.get(provider).map_or(&[], Vec::as_slice),
        }
    }

    #[must_use]
    pub fn option(&self, provider: &str, model: &str) -> Option<&ModelOption> {
        self.models(provider)
            .iter()
            .find(|option| option.id == model)
    }

    #[must_use]
    pub fn cycle_model(&self, provider: &str, current: &str, delta: isize) -> String {
        let models = self.models(provider);
        if models.is_empty() {
            return current.to_owned();
        }
        let current = models
            .iter()
            .position(|model| model.id == current)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(models.len() as isize) as usize;
        models[next].id.clone()
    }

    #[must_use]
    pub fn first_model(&self, provider: &str) -> String {
        self.models(provider)
            .first()
            .map(|model| model.id.clone())
            .unwrap_or_else(|| match provider {
                OPENAI => DEFAULT_OPENAI_MODEL.into(),
                OLLAMA => DEFAULT_OLLAMA_MODEL.into(),
                "codex_cli" | "claude_cli" => DEFAULT_CLI_MODEL.into(),
                _ => String::new(),
            })
    }

    #[must_use]
    pub fn selection_label(&self, provider: &str, model: &str) -> String {
        let models = self.models(provider);
        let selected = models
            .iter()
            .position(|option| option.id == model)
            .map_or(1, |index| index + 1);
        format!("{selected}/{}", models.len().max(1))
    }

    #[must_use]
    pub fn model_is_editable(&self, provider: &str) -> bool {
        (provider == OLLAMA && !self.ollama_discovered)
            || matches!(provider, "codex_cli" | "claude_cli")
    }

    #[must_use]
    pub fn estimate_usd(
        &self,
        provider: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Option<f64> {
        if provider != OPENAI {
            return None;
        }
        let model = self.priced_openai.get(model)?;
        let cost = calculate_cost_for_model(
            model,
            &CostRequest {
                input_tokens,
                output_tokens,
                ..CostRequest::default()
            },
        )
        .ok()?;
        cost.total.to_string().parse().ok()
    }
}

fn fallback(model: &str) -> Vec<ModelOption> {
    vec![ModelOption {
        id: model.into(),
        display_name: model.into(),
        context_limit: None,
        input_price: None,
        output_price: None,
    }]
}

fn compatible_with_voff(model: &&FlatModel) -> bool {
    model.status != Some(ModelStatus::Deprecated)
        && model
            .features
            .as_ref()
            .and_then(|features| features.tool_call)
            == Some(true)
        && model
            .modalities
            .as_ref()
            .and_then(|modalities| modalities.output.as_ref())
            .is_some_and(|outputs| outputs.contains(&ModelModality::Text))
}

fn model_option(model: &FlatModel) -> ModelOption {
    let rates = model.pricing.as_ref().map(|pricing| &pricing.rates);
    ModelOption {
        id: model.id.clone(),
        display_name: model.name.clone().unwrap_or_else(|| model.id.clone()),
        context_limit: model.limit.as_ref().and_then(|limit| limit.context),
        input_price: rates
            .and_then(|rates| rates.input)
            .map(|price| price.to_string()),
        output_price: rates
            .and_then(|rates| rates.output)
            .map(|price| price.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_lists_compatible_openai_models_and_prices_them() {
        let catalog = ModelCatalog::bundled().unwrap();
        assert!(catalog.models(OPENAI).len() > 10);
        assert_eq!(catalog.models(OPENAI)[0].id, DEFAULT_OPENAI_MODEL);
        assert!(catalog.option(OPENAI, "gpt-5-mini").is_some());
        assert!(
            catalog
                .estimate_usd(OPENAI, "gpt-5-mini", 1_000, 500)
                .is_some()
        );
        assert!(catalog.option(OPENAI, "text-embedding-3-small").is_none());
    }

    #[test]
    fn local_models_replace_the_fallback_and_remain_unpriced() {
        let mut catalog = ModelCatalog::default();
        catalog.set_ollama_models(vec![ModelDescriptor {
            id: "local-code:latest".into(),
            display_name: "Local Code".into(),
            context_limit: Some(32_000),
        }]);
        assert_eq!(catalog.first_model("ollama"), "local-code:latest");
        assert_eq!(catalog.models("ollama")[0].display_name, "Local Code");
        assert!(
            catalog
                .estimate_usd("ollama", "local-code:latest", 1_000, 500)
                .is_none()
        );
    }
}
