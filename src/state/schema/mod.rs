use anyhow::anyhow;
use apollo_compiler::{Schema, ast::Document, validation::Valid};
use std::{
    fs,
    hash::{Hash, Hasher},
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock;
use tracing::info;

mod federation;

#[derive(Debug)]
pub struct FederatedSchema {
    valid: Valid<Schema>,
    source: String,
}

impl Deref for FederatedSchema {
    type Target = Valid<Schema>;

    fn deref(&self) -> &Self::Target {
        &self.valid
    }
}

impl FederatedSchema {
    /// Parse the file at `path` as a GraphQL schema.
    pub fn parse(path: &PathBuf) -> anyhow::Result<Self> {
        info!(path=%path.display(), "loading and parsing supergraph schema");
        let source = fs::read_to_string(path)?;

        Self::parse_string(source, path)
    }

    /// Parse `source` as a GraphQL schema. `path` will be used in diagnostic errors to identify this schema.
    pub fn parse_string(source: impl ToString, path: impl AsRef<Path>) -> anyhow::Result<Self> {
        // Parse the raw AST as federation-compatible schemas won't start out as valid GraphQL
        let mut ast = Document::parse(source.to_string(), path).map_err(|err| anyhow!(err))?;
        let federation_type = federation::patch_ast(&mut ast);

        let mut schema = ast.to_schema().map_err(|err| anyhow!(err))?;
        federation::patch_schema(&mut schema, federation_type)?;
        Ok(Self {
            valid: schema.validate().map_err(|err| anyhow!(err))?,
            source: source.to_string(),
        })
    }

    /// Output the Federation-compatible sdl response for this schema
    pub fn sdl(&self) -> &str {
        &self.source
    }
}

impl Hash for FederatedSchema {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source.hash(state);
    }
}

pub fn update_schema(path: &PathBuf, lock: Arc<RwLock<FederatedSchema>>) -> anyhow::Result<()> {
    let schema = FederatedSchema::parse(path)?;
    *lock.blocking_write() = schema;
    info!(path=%path.display(), "new supergraph schema loaded");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn supergraph_schema_validates() -> anyhow::Result<()> {
        let schema = include_str!("test-data/supergraph.graphql");
        let validated = FederatedSchema::parse_string(schema, "test-data/supergraph.graphql")?;

        assert_eq!(
            include_str!("test-data/supergraph-validated.graphql"),
            validated.to_string()
        );
        Ok(())
    }

    #[test]
    fn federated_subgraph_schema_validates() -> anyhow::Result<()> {
        let schema = include_str!("test-data/federated-subgraph.graphql");
        let validated =
            FederatedSchema::parse_string(schema, "test-data/federated-subgraph.graphql")?;

        assert_eq!(
            include_str!("test-data/federated-subgraph-validated.graphql"),
            validated.to_string()
        );
        Ok(())
    }

    #[test]
    fn non_federated_subgraph_schema_validates() -> anyhow::Result<()> {
        let schema = include_str!("test-data/non-federated-subgraph.graphql");
        let validated =
            FederatedSchema::parse_string(schema, "test-data/non-federated-subgraph.graphql")?;

        assert_eq!(
            include_str!("test-data/non-federated-subgraph-validated.graphql"),
            validated.to_string()
        );
        Ok(())
    }
}
