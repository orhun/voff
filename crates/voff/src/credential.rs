//! Simple workspace-local provider credential resolution.

use std::sync::{Arc, RwLock};
use thiserror::Error;
use voff_storage::{RepositoryError, SqliteRepository};

/// In-memory credential state shared by provider adapters and the terminal UI.
#[derive(Clone, Default)]
pub struct CredentialSlot(Arc<RwLock<Option<String>>>);

impl CredentialSlot {
    #[must_use]
    pub fn new(secret: Option<String>) -> Self {
        Self(Arc::new(RwLock::new(secret)))
    }

    #[must_use]
    pub fn get(&self) -> Option<String> {
        self.0.read().ok().and_then(|secret| secret.clone())
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        self.get().is_some_and(|secret| !secret.trim().is_empty())
    }

    pub fn set(&self, secret: String) {
        if let Ok(mut current) = self.0.write() {
            *current = Some(secret);
        }
    }
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential is empty")]
    Empty,
    #[error("local credential database is unavailable: {0}")]
    Store(#[from] RepositoryError),
}

/// Resolve an environment override or the workspace-local SQLite value.
pub async fn resolve(
    reference: &str,
    repository: &SqliteRepository,
) -> Result<String, CredentialError> {
    if let Ok(secret) = std::env::var(reference)
        && !secret.trim().is_empty()
    {
        return Ok(secret);
    }
    repository
        .credential(reference)
        .await?
        .filter(|secret| !secret.trim().is_empty())
        .ok_or(CredentialError::Empty)
}

/// Save a credential in the workspace-local SQLite database.
pub async fn store(
    reference: &str,
    secret: &str,
    repository: &SqliteRepository,
) -> Result<(), CredentialError> {
    if secret.trim().is_empty() {
        return Err(CredentialError::Empty);
    }
    repository.save_credential(reference, secret).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_and_resolves_a_local_credential() {
        let repository = SqliteRepository::open_in_memory().await.unwrap();
        assert!(matches!(
            store("VOFF_TEST_TOKEN", "  ", &repository).await,
            Err(CredentialError::Empty)
        ));

        store("VOFF_TEST_TOKEN", "secret", &repository)
            .await
            .unwrap();
        assert_eq!(
            resolve("VOFF_TEST_TOKEN", &repository).await.unwrap(),
            "secret"
        );
    }
}
