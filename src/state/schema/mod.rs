use anyhow::anyhow;
use apollo_compiler::{Schema, ast::Document, validation::Valid};
use std::{
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::RwLock;
use tracing::info;

use crate::state::schema::federation::patch_schema;

mod federation;

pub struct HashedSchema {
    pub valid: Valid<Schema>,
    hash: u64,
}

impl HashedSchema {
    pub fn parse(path: &PathBuf) -> anyhow::Result<Self> {
        info!(path=%path.display(), "loading and parsing supergraph schema");
        let mut hasher = DefaultHasher::new();

        let source = fs::read_to_string(path)?;
        source.hash(&mut hasher);

        // Parse the raw AST as federation-compatible schemas won't start out as valid GraphQL
        let mut ast = Document::parse(source, path).map_err(|err| anyhow!(err))?;
        federation::patch_ast(&mut ast);

        let mut schema = ast.to_schema().map_err(|err| anyhow!(err))?;
        patch_schema(&mut schema)?;
        Ok(Self {
            valid: schema.validate().map_err(|err| anyhow!(err))?,
            hash: hasher.finish(),
        })
    }
}

impl Hash for HashedSchema {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

pub fn update_schema(path: &PathBuf, lock: Arc<RwLock<HashedSchema>>) -> anyhow::Result<()> {
    let schema = HashedSchema::parse(path)?;
    *lock.blocking_write() = schema;
    info!(path=%path.display(), "new supergraph schema loaded");
    Ok(())
}
