use anyhow::anyhow;
use apollo_compiler::{
    Node, Schema,
    ast::{FieldDefinition, InputValueDefinition, Type},
    collections::IndexSet,
    name,
    schema::{
        Component, ComponentName, ComponentOrigin, DirectiveDefinition, DirectiveLocation,
        ExtendedType, ScalarType, UnionType,
    },
    ty,
    validation::Valid,
};
use std::{
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::RwLock;
use tracing::info;

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

        let mut schema = Schema::parse(source, path).map_err(|err| anyhow!(err))?;
        patch_supergraph(&mut schema)?;
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
    Ok(())
}

/// We need to be able to intercept and handle queries for entities:
/// { _entities(representations: [_Any!]!): [_Entity]!
///
/// The router also auto-supports the @defer and @stream directive so schemas may be using them without
/// importing / defining them directly. In that case we need to inject them into the schema in
/// order for the validation of our queries to succeed.
///
/// See https://www.apollographql.com/docs/graphos/routing/operations/defer
///
/// The directive definitions are copied from here:
///   https://github.com/apollographql/router/blob/23e580e22a4401cc2e7a952b241a1ec955b29c99/apollo-federation/src/api_schema.rs#L156https://github.com/apollographql/router/blob/23e580e22a4401cc2e7a952b241a1ec955b29c99/apollo-federation/src/api_schema.rs#L156
fn patch_supergraph(schema: &mut Schema) -> anyhow::Result<()> {
    // Grab _everything_ for our _Entity union. This is a lot more than the true _Entity union for
    // any of the actual subgraphs but it at least means that we can correctly parse the queries
    // coming from the client.
    let members: IndexSet<ComponentName> = schema
        .types
        .iter()
        .filter(|(_, ty)| ty.is_object())
        .map(|(name, _)| ComponentName {
            origin: ComponentOrigin::Definition,
            name: name.clone(),
        })
        .collect();

    // Inject our _Entity union
    schema.types.insert(
        name!("_Entity"),
        ExtendedType::Union(Node::new(UnionType {
            description: None,
            name: name!("_Entity"),
            directives: Default::default(),
            members,
        })),
    );

    // Inject our stub _Any scalar
    schema.types.insert(
        name!("_Any"),
        ExtendedType::Scalar(Node::new(ScalarType {
            description: None,
            name: name!("_Any"),
            directives: Default::default(),
        })),
    );

    // Inject the _entities query itself
    let query_type_name = &schema.schema_definition.query.as_ref().unwrap().name;
    let query_root = match schema.types.get_mut(query_type_name).unwrap() {
        ExtendedType::Object(obj) => obj,
        _ => return Err(anyhow!("query root is not an object")),
    };

    query_root.make_mut().fields.insert(
        name!("_entities"),
        Component::new(FieldDefinition {
            description: None,
            name: name!("_entities"),
            arguments: vec![Node::new(InputValueDefinition {
                description: None,
                name: name!("representations"),
                ty: Node::new(Type::NonNullList(Box::new(Type::NonNullNamed(name!(
                    "_Any"
                ))))),
                default_value: None,
                directives: Default::default(),
            })],
            ty: Type::NonNullList(Box::new(Type::Named(name!("_Entity")))),
            directives: Default::default(),
        }),
    );

    // Matching the behaviour in the Router:
    //   https://github.com/apollographql/router/blob/23e580e22a4401cc2e7a952b241a1ec955b29c99/apollo-federation/src/api_schema.rs#L139-L149
    if !schema.directive_definitions.contains_key(&name!("defer")) {
        schema
            .directive_definitions
            .insert(name!("defer"), defer_definition());
    }
    if !schema.directive_definitions.contains_key(&name!("stream")) {
        schema
            .directive_definitions
            .insert(name!("stream"), stream_definition());
    }
    Ok(())
}

fn defer_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: None,
        name: name!("defer"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("label"),
                ty: ty!(String).into(),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("if"),
                ty: ty!(Boolean!).into(),
                default_value: Some(true.into()),
                directives: Default::default(),
            }),
        ],
        repeatable: false,
        locations: vec![
            DirectiveLocation::FragmentSpread,
            DirectiveLocation::InlineFragment,
        ],
    })
}

fn stream_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: None,
        name: name!("stream"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("label"),
                ty: ty!(String).into(),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("if"),
                ty: ty!(Boolean!).into(),
                default_value: Some(true.into()),
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("initialCount"),
                ty: ty!(Int).into(),
                default_value: Some(0.into()),
                directives: Default::default(),
            }),
        ],
        repeatable: false,
        locations: vec![DirectiveLocation::Field],
    })
}
