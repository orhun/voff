//! Optional process environment initialization before any worker threads start.

use anyhow::Result;

/// Load only the invocation directory's `.env`, preserving process variables.
/// Must be called once, at synchronous executable startup before threads start.
pub(crate) fn load() -> Result<()> {
    match dotenvy::from_path(".env") {
        Ok(()) => Ok(()),
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // Parser errors contain source text, so never attach the original error.
        Err(_) => anyhow::bail!(
            "could not load .env in the current directory; check file access and syntax"
        ),
    }
}
