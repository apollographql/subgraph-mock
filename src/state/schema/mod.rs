use apollo_compiler::{Schema, ast::Document, validation::Valid};
use miette::Diagnostic;
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

#[derive(Debug, apollo_errors::Error, Diagnostic)]
pub enum SchemaError {
    #[error("failed to read schema file")]
    #[diagnostic(code(schema::io))]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("failed to parse schema: {message}")]
    #[diagnostic(code(schema::parse))]
    Parse {
        #[extension]
        message: String,
    },

    #[error("failed to build schema: {message}")]
    #[diagnostic(code(schema::build))]
    Build {
        #[extension]
        message: String,
    },

    #[error("schema failed validation: {message}")]
    #[diagnostic(code(schema::validate))]
    Validate {
        #[extension]
        message: String,
    },

    #[error("schema does not define a query type")]
    #[diagnostic(code(schema::missing_query_type))]
    MissingQueryType,

    #[error("query root is not an object type")]
    #[diagnostic(code(schema::query_root_not_object))]
    QueryRootNotObject,

    #[error("failed to watch schema file for changes")]
    #[diagnostic(code(schema::watch))]
    Watch {
        #[from]
        source: notify::Error,
    },
}

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
    pub fn parse(path: &PathBuf) -> Result<Self, SchemaError> {
        info!(path=%path.display(), "loading and parsing supergraph schema");
        let source = fs::read_to_string(path)?;

        Self::parse_string(source, path)
    }

    /// Parse `source` as a GraphQL schema. `path` will be used in diagnostic errors to identify this schema.
    pub fn parse_string(
        source: impl ToString,
        path: impl AsRef<Path>,
    ) -> Result<Self, SchemaError> {
        // Parse the raw AST as federation-compatible schemas won't start out as valid GraphQL
        let mut ast =
            Document::parse(source.to_string(), path).map_err(|err| SchemaError::Parse {
                message: err.to_string(),
            })?;
        let federation_type = federation::patch_ast(&mut ast);

        let mut schema = ast.to_schema().map_err(|err| SchemaError::Build {
            message: err.to_string(),
        })?;
        federation::patch_schema(&mut schema, federation_type)?;
        Ok(Self {
            valid: schema.validate().map_err(|err| SchemaError::Validate {
                message: err.to_string(),
            })?,
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

pub fn update_schema(
    path: &PathBuf,
    lock: Arc<RwLock<FederatedSchema>>,
) -> Result<(), SchemaError> {
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

    #[test]
    fn malformed_syntax_fails_to_parse() {
        let err = FederatedSchema::parse_string("type Query {", "test.graphql")
            .expect_err("expected a parse error");
        assert!(matches!(err, SchemaError::Parse { .. }));
    }

    #[test]
    fn missing_query_type_is_rejected() {
        // No `Query` type, no explicit `schema { ... }` definition, and no federation markers
        // (no `@link`, no `join__Graph`) -- classified `FederationType::None`, which requires an
        // existing query type rather than synthesizing one the way `FederationType::Subgraph`
        // does.
        let err = FederatedSchema::parse_string("type Foo { bar: String }", "test.graphql")
            .expect_err("expected a missing-query-type error");
        assert!(matches!(err, SchemaError::MissingQueryType));
    }

    #[test]
    fn non_object_query_root_is_rejected() {
        let err = FederatedSchema::parse_string(
            "scalar MyScalar\nschema { query: MyScalar }\ntype Query { foo: String }",
            "test.graphql",
        )
        .expect_err("expected a query-root-not-object error");
        assert!(matches!(err, SchemaError::QueryRootNotObject));
    }

    #[test]
    fn duplicate_type_definition_fails_to_build() {
        let err = FederatedSchema::parse_string(
            "type Query { foo: String }\ntype Query { bar: String }",
            "test.graphql",
        )
        .expect_err("expected a build error");
        assert!(matches!(err, SchemaError::Build { .. }));
    }

    #[test]
    fn unresolvable_type_reference_fails_validation() {
        let err = FederatedSchema::parse_string("type Query { foo: DoesNotExist }", "test.graphql")
            .expect_err("expected a validation error");
        assert!(matches!(err, SchemaError::Validate { .. }));
    }

    #[test]
    fn nonexistent_schema_file_is_an_io_error() {
        let err = FederatedSchema::parse(&PathBuf::from("/definitely/does/not/exist.graphql"))
            .expect_err("expected an io error");
        assert!(matches!(err, SchemaError::Io { .. }));
    }
}
