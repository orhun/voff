//! Embedded cloud catalog and background discovery of local server models.

use llm_kernel::provider::ProviderIndex;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelOption {
    pub id: String,
}

#[derive(Clone, Debug)]
pub struct ModelCatalog {
    llama_cpp: Vec<ModelOption>,
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
            llama_cpp: Vec::new(),
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

/// A model request owned by the UI that needs it.
pub(crate) struct ModelDiscovery {
    pub(crate) receiver: tokio::sync::mpsc::Receiver<Vec<voff_provider::ModelDescriptor>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ModelDiscovery {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ModelCatalog {
    /// Query an explicitly selected llama.cpp endpoint without blocking the UI.
    pub(crate) fn discover_llama_cpp(base_url: &str) -> ModelDiscovery {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let provider = voff_provider::LlamaCppProvider::new(base_url);
        let task = tokio::spawn(async move {
            use voff_provider::ModelProvider;
            let Ok(provider) = provider else {
                return;
            };
            if let Ok(Ok(models)) =
                tokio::time::timeout(std::time::Duration::from_secs(2), provider.models()).await
            {
                let _ = tx.send(models).await;
            }
        });
        ModelDiscovery { receiver: rx, task }
    }

    pub fn set_local_models(&mut self, models: Vec<voff_provider::ModelDescriptor>) {
        self.llama_cpp = models
            .into_iter()
            .filter(|model| {
                !model.id.trim().is_empty()
                    && model.id.len() <= 1024
                    && !model.id.chars().any(char::is_control)
            })
            .take(4096)
            .map(|model| ModelOption { id: model.id })
            .collect();
        self.llama_cpp.sort_by(|left, right| left.id.cmp(&right.id));
        self.llama_cpp.dedup_by(|left, right| left.id == right.id);
    }

    pub fn has_picker(&self, provider: &str) -> bool {
        let count = self.models(provider).len();
        count > 1 || (provider == "llamacpp" && count == 1)
    }

    #[must_use]
    pub fn models(&self, provider: &str) -> &[ModelOption] {
        match provider {
            "llamacpp" => &self.llama_cpp,
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

    #[tokio::test]
    async fn dropping_discovery_cancels_its_request_task() {
        let (tx, receiver) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(async move {
            let _sender = tx;
            std::future::pending::<()>().await;
        });
        let abort = task.abort_handle();
        let discovery = ModelDiscovery { receiver, task };
        drop(discovery);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(abort.is_finished());
    }

    #[test]
    fn bundled_catalog_has_provider_models() {
        let catalog = ModelCatalog::default();
        assert!(!catalog.models("openai").is_empty());
        assert!(!catalog.models("ollama").is_empty());
        assert_eq!(catalog.first_model("codex_cli"), "default");
    }

    #[tokio::test]
    async fn discovers_router_models_without_loading_them_and_tolerates_failures() {
        use std::io::{BufRead, Write};
        for (status, body, expected) in [
            (
                "200 OK",
                r#"{"data":[{"id":"repo/B:Q4_K_M","status":{"value":"unloaded"}},{"id":"repo/A:Q4_K_M","status":{"value":"loaded"}},{"id":"repo/A:Q4_K_M"}]}"#,
                Some(2),
            ),
            ("200 OK", r#"{"data":[]}"#, Some(0)),
            ("503 Service Unavailable", "{}", None),
            ("200 OK", "not-json", None),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base_url = format!("http://{}/proxy/llama/v1/", listener.local_addr().unwrap());
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "GET /proxy/llama/v1/models HTTP/1.1\r\n");
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                socket.write_all(response.as_bytes()).unwrap();
            });
            let mut discovery = ModelCatalog::discover_llama_cpp(&base_url);
            let mut catalog = ModelCatalog::default();
            if let Some(models) = discovery.receiver.recv().await {
                catalog.set_local_models(models);
                assert_eq!(Some(catalog.models("llamacpp").len()), expected);
                if expected == Some(2) {
                    assert_eq!(catalog.first_model("llamacpp"), "repo/A:Q4_K_M");
                    assert_eq!(
                        catalog.cycle_model("llamacpp", "repo/A:Q4_K_M", 1),
                        "repo/B:Q4_K_M"
                    );
                }
            } else {
                assert_eq!(expected, None);
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn local_catalog_filters_unsafe_ids_and_has_no_invented_default() {
        let mut catalog = ModelCatalog::default();
        assert!(catalog.first_model("llamacpp").is_empty());
        catalog.set_local_models(
            ["", "  ", "bad\u{1b}[31m", "valid"]
                .map(|id| voff_provider::ModelDescriptor {
                    id: id.into(),
                    display_name: id.into(),
                    context_limit: None,
                })
                .into(),
        );
        assert_eq!(catalog.first_model("llamacpp"), "valid");
        assert!(catalog.has_picker("llamacpp"));
        assert!(!catalog.has_picker("codex_cli"));
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
