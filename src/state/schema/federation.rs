use anyhow::anyhow;
use apollo_compiler::{
    Name, Node, Schema,
    ast::{
        Definition, Directive, DirectiveList, Document, EnumValueDefinition, FieldDefinition,
        InputValueDefinition, OperationType, SchemaDefinition, Type,
    },
    collections::IndexSet,
    name,
    schema::{
        Component, ComponentName, ComponentOrigin, DirectiveDefinition, DirectiveLocation,
        EnumType, ExtendedType, ObjectType, ScalarType, UnionType,
    },
    ty,
};
use tracing::warn;

/// We need to be able to intercept and handle queries for entities and service:
/// {
///   _entities(representations: [_Any!]!): [_Entity]!
///   _service: _Service!
/// }
///
/// The router also auto-supports the @defer and @stream directive so schemas may be using them without
/// importing / defining them directly. In that case we need to inject them into the schema in
/// order for the validation of our queries to succeed.
///
/// See https://www.apollographql.com/docs/graphos/routing/operations/defer
///
/// The directive definitions are copied from here:
///   https://github.com/apollographql/router/blob/23e580e22a4401cc2e7a952b241a1ec955b29c99/apollo-federation/src/api_schema.rs#L156https://github.com/apollographql/router/blob/23e580e22a4401cc2e7a952b241a1ec955b29c99/apollo-federation/src/api_schema.rs#L156
pub fn patch_schema(schema: &mut Schema, federation_type: FederationType) -> anyhow::Result<()> {
    // Resolve federated types for the _Entity union.
    let members: IndexSet<ComponentName> = schema
        .types
        .iter()
        .filter(|(_, ty)| is_federated_type(schema, ty))
        .map(|(name, _)| ComponentName {
            origin: ComponentOrigin::Definition,
            name: name.clone(),
        })
        .collect();

    let has_federated_members = !members.is_empty();
    if has_federated_members {
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
    }
    // Inject all other federation types that aren't dynamic
    insert_federation_types(schema, &federation_type);

    let query_type_name = if let FederationType::Subgraph = federation_type {
        // Create the Query type if it doesn't exist, which Federation-compatible schemas won't always do
        if schema.schema_definition.query.is_none() {
            schema.schema_definition.make_mut().query = Some(name!("Query").into());
            schema.types.insert(
                name!("Query"),
                Node::new(ObjectType {
                    description: None,
                    name: name!("Query"),
                    implements_interfaces: Default::default(),
                    directives: Default::default(),
                    fields: Default::default(),
                })
                .into(),
            );
        }
        let query_type_name: &Name = schema.schema_definition.query.as_ref().unwrap();
        if !schema.types.contains_key(query_type_name) {
            schema.types.insert(
                query_type_name.clone(),
                Node::new(ObjectType {
                    description: None,
                    name: query_type_name.clone(),
                    implements_interfaces: Default::default(),
                    directives: Default::default(),
                    fields: Default::default(),
                })
                .into(),
            );
        }
        query_type_name
    } else {
        schema
            .schema_definition
            .query
            .as_ref()
            .ok_or_else(|| anyhow!("Schema does not define a query type"))?
    };

    // Inject _entities query if appropriate and the _service query
    let query_root = match schema.types.get_mut(query_type_name).unwrap() {
        ExtendedType::Object(obj) => obj,
        _ => return Err(anyhow!("query root is not an object")),
    };

    if has_federated_members {
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
    }

    query_root.make_mut().fields.insert(
        name!("_service"),
        Component::new(FieldDefinition {
            description: None,
            name: name!("_service"),
            arguments: vec![],
            ty: Type::NonNullNamed(name!("_Service")),
            directives: Default::default(),
        }),
    );

    if let FederationType::Supergraph = federation_type {
        // Matching the behaviour in the Router:
        //   https://github.com/apollographql/router/blob/23e580e22a4401cc2e7a952b241a1ec955b29c99/apollo-federation/src/api_schema.rs#L139-L149
        // These are not yet formally defined in the Federation/GraphQL spec, so we will match the software-injected behavior
        // rather than pulling in the literal GraphQL definitions.
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
    }

    Ok(())
}

#[derive(Debug)]
pub enum FederationType {
    Subgraph,
    Supergraph,
    None,
}

/// Federated schemas do not start out as valid GraphQL schemas and must be patched before they will parse as one.
///
/// This means patching in a schema definition if it doesn't exist, and ensuring all relevant directives are in scope.
///
/// Returns the federation type of this schema as inferred by the presence of key directives and types.
/// Subgraph schemas are identified by the presence of the `@link` directive on their schema extension.
/// Supergraph schemas are identified by the presence of the `join__Graph` enum.
pub fn patch_ast(ast: &mut Document) -> FederationType {
    let schema_extension = ast.definitions.iter().find_map(|def| match def {
        Definition::SchemaExtension(node) => Some(node),
        _ => None,
    });

    // The `join__Graph` enum is a required part of the supergraph spec
    let fed_type = if ast
        .definitions
        .iter()
        .any(|definion| definion.name().is_some_and(|name| name == "join__Graph"))
    {
        FederationType::Supergraph
    } else if let Some(extension) = schema_extension
        // If `join__Graph` is not present, but the schema is still extended with `@link`, then this is a subgraph schema
        && extension.directives.iter().any(|dir| dir.name == "link")
    {
        // Federated subgraph schemas can omit the root schema definition entirely, and it is expected to be implicitly added
        if !ast
            .definitions
            .iter()
            .any(|def| matches!(def, Definition::SchemaDefinition(_)))
        {
            ast.definitions
                .push(Definition::SchemaDefinition(Node::new(SchemaDefinition {
                    description: None,
                    directives: Default::default(),
                    root_operations: vec![Node::new((OperationType::Query, name!("Query")))],
                })));
        }
        // The federation spec requires that all these directives be implicitly added to the schema for a subgraph server
        ast.definitions.append(&mut federation_directives());

        FederationType::Subgraph
    } else {
        FederationType::None
    };

    if let FederationType::Subgraph | FederationType::Supergraph = fed_type {
        // The `@link` directive must be followed to import values that may be referenced in the file
        // This behavior is currently not implemented.
        for def in &ast.definitions {
            if let Definition::SchemaDefinition(schema_def) = def {
                process_link_directives(&schema_def.directives);
            }
            if let Definition::SchemaExtension(schema_ext) = def {
                process_link_directives(&schema_ext.directives);
            }
        }
    }

    fed_type
}

/// Determines if a type is federated based on its schema definition
fn is_federated_type(schema: &Schema, ty: &ExtendedType) -> bool {
    ty.directives().iter().any(|directive| {
        is_federated_directive(schema, directive)
            // Do not include the query type if it is defined
            && schema
                .schema_definition
                .query
                .as_ref()
                .is_none_or(|query| &query.name != ty.name())
    })
}

/// Determines if a directive represents a federated type
///
/// If we are loading a supergraph schema, types that are federated will use `@join__type`.
/// If we are loading a subgraph schema, types that are federated will use [`@key`](key_definition).
fn is_federated_directive(schema: &Schema, directive: &Component<Directive>) -> bool {
    match directive.name.as_str() {
        "key" | "join__type" => {
            // federated unless explicitly marked resolvable: false
            directive
                .argument_by_name("resolvable", schema)
                .ok()
                .and_then(|arg| arg.to_bool())
                .expect("the @key and @join__type directives specify 'resolvable' as a boolean argument")
        }
        _ => false,
    }
}

/// `@link` directives describe external imports of other GraphQL schema files. We do not currently handle
/// that external resolution as our present use cases only are "importing" the federation spec itself.
fn process_link_directives(directives: &DirectiveList) {
    for directive in directives {
        if directive.name == "link" {
            warn!("@link directive detected, but link directive resolution is not implemented.")
        }
    }
}

fn federation_directives() -> Vec<Definition> {
    vec![
        external_definition(),
        requires_definition(),
        provides_definition(),
        key_definition(),
        link_definition(),
        shareable_definition(),
        inaccessible_definition(),
        tag_definition(),
        override_definition(),
        compose_directive_definition(),
        interface_object_definition(),
        authenticated_definition(),
        requires_scopes_definition(),
        policy_definition(),
        context_definition(),
        from_context_definition(),
    ]
}

/// Types that must be implicitly injected into federated schemas:
/// https://www.apollographql.com/docs/graphos/schema-design/federated-schemas/reference/subgraph-spec#subgraph-schema-additions
fn insert_federation_types(schema: &mut Schema, federation_type: &FederationType) {
    // We always support the _service federated query even if the schema is unfederated
    schema.types.insert(
        name!("_Service"),
        ExtendedType::Object(Node::new(ObjectType {
            description: None,
            name: name!("_Service"),
            implements_interfaces: Default::default(),
            directives: Default::default(),
            fields: vec![(
                name!("sdl"),
                Component::new(FieldDefinition {
                    description: None,
                    name: name!("sdl"),
                    arguments: vec![],
                    ty: Type::NonNullNamed(name!("String")),
                    directives: Default::default(),
                }),
            )]
            .into_iter()
            .collect(),
        })),
    );

    // Used in _Entities for both federation types
    if let FederationType::Subgraph | FederationType::Supergraph = federation_type {
        schema.types.insert(
            name!("_Any"),
            ExtendedType::Scalar(Node::new(ScalarType {
                description: None,
                name: name!("_Any"),
                directives: Default::default(),
            })),
        );
    }

    // Exclusive to the subgraph spec
    if let FederationType::Subgraph = federation_type {
        schema.types.insert(
            name!("link__Purpose"),
            ExtendedType::Enum(Node::new(EnumType {
                description: None,
                name: name!("link__Purpose"),
                directives: Default::default(),
                values: vec![
                    (
                        name!("SECURITY"),
                        Component::new(EnumValueDefinition {
                            description: None,
                            value: name!("SECURITY"),
                            directives: Default::default(),
                        }),
                    ),
                    (
                        name!("EXECUTION"),
                        Component::new(EnumValueDefinition {
                            description: None,
                            value: name!("EXECUTION"),
                            directives: Default::default(),
                        }),
                    ),
                ]
                .into_iter()
                .collect(),
            })),
        );

        schema.types.insert(
            name!("FieldSet"),
            ExtendedType::Scalar(Node::new(ScalarType {
                description: None,
                name: name!("FieldSet"),
                directives: Default::default(),
            })),
        );
        schema.types.insert(
            name!("link__Import"),
            ExtendedType::Scalar(Node::new(ScalarType {
                description: None,
                name: name!("link__Import"),
                directives: Default::default(),
            })),
        );
        schema.types.insert(
            name!("federation__ContextFieldValue"),
            ExtendedType::Scalar(Node::new(ScalarType {
                description: None,
                name: name!("federation__ContextFieldValue"),
                directives: Default::default(),
            })),
        );
        schema.types.insert(
            name!("federation__Scope"),
            ExtendedType::Scalar(Node::new(ScalarType {
                description: None,
                name: name!("federation__Scope"),
                directives: Default::default(),
            })),
        );
        schema.types.insert(
            name!("federation__Policy"),
            ExtendedType::Scalar(Node::new(ScalarType {
                description: None,
                name: name!("federation__Policy"),
                directives: Default::default(),
            })),
        );
    }
}

fn link_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("link"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("url"),
                ty: Node::new(Type::NonNullNamed(name!("String"))),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("as"),
                ty: Node::new(Type::Named(name!("String"))),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("for"),
                ty: Node::new(Type::Named(name!("link__Purpose"))),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("import"),
                ty: Node::new(Type::List(Box::new(Type::Named(name!("link__Import"))))),
                default_value: None,
                directives: Default::default(),
            }),
        ],
        repeatable: true,
        locations: vec![DirectiveLocation::Schema],
    }))
}

fn key_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("key"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("fields"),
                ty: Node::new(Type::NonNullNamed(name!("FieldSet"))),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("resolvable"),
                ty: Node::new(Type::Named(name!("Boolean"))),
                default_value: Some(Node::new(apollo_compiler::ast::Value::Boolean(true))),
                directives: Default::default(),
            }),
        ],
        repeatable: true,
        locations: vec![DirectiveLocation::Object, DirectiveLocation::Interface],
    }))
}

fn requires_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("requires"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("fields"),
            ty: Node::new(Type::NonNullNamed(name!("FieldSet"))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: false,
        locations: vec![DirectiveLocation::FieldDefinition],
    }))
}

fn provides_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("provides"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("fields"),
            ty: Node::new(Type::NonNullNamed(name!("FieldSet"))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: false,
        locations: vec![DirectiveLocation::FieldDefinition],
    }))
}

fn external_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("external"),
        arguments: vec![],
        repeatable: false,
        locations: vec![
            DirectiveLocation::Object,
            DirectiveLocation::FieldDefinition,
        ],
    }))
}

fn tag_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("tag"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("name"),
            ty: Node::new(Type::NonNullNamed(name!("String"))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: true,
        locations: vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Union,
            DirectiveLocation::ArgumentDefinition,
            DirectiveLocation::Scalar,
            DirectiveLocation::Enum,
            DirectiveLocation::EnumValue,
            DirectiveLocation::InputObject,
            DirectiveLocation::InputFieldDefinition,
        ],
    }))
}

fn shareable_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("shareable"),
        arguments: vec![],
        repeatable: false,
        locations: vec![
            DirectiveLocation::Object,
            DirectiveLocation::FieldDefinition,
        ],
    }))
}

fn inaccessible_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("inaccessible"),
        arguments: vec![],
        repeatable: false,
        locations: vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Union,
            DirectiveLocation::ArgumentDefinition,
            DirectiveLocation::Scalar,
            DirectiveLocation::Enum,
            DirectiveLocation::EnumValue,
            DirectiveLocation::InputObject,
            DirectiveLocation::InputFieldDefinition,
        ],
    }))
}

fn override_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("override"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("from"),
            ty: Node::new(Type::NonNullNamed(name!("String"))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: false,
        locations: vec![DirectiveLocation::FieldDefinition],
    }))
}

fn compose_directive_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("composeDirective"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("name"),
            ty: Node::new(Type::NonNullNamed(name!("String"))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: true,
        locations: vec![DirectiveLocation::Schema],
    }))
}

fn interface_object_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("interfaceObject"),
        arguments: vec![],
        repeatable: false,
        locations: vec![DirectiveLocation::Object],
    }))
}

fn authenticated_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("authenticated"),
        arguments: vec![],
        repeatable: false,
        locations: vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Scalar,
            DirectiveLocation::Enum,
        ],
    }))
}

fn requires_scopes_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("requiresScopes"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("scopes"),
            ty: Node::new(Type::NonNullList(Box::new(Type::NonNullList(Box::new(
                Type::NonNullNamed(name!("federation__Scope")),
            ))))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: false,
        locations: vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Scalar,
            DirectiveLocation::Enum,
        ],
    }))
}

fn policy_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("policy"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("policies"),
            ty: Node::new(Type::NonNullList(Box::new(Type::NonNullList(Box::new(
                Type::NonNullNamed(name!("federation__Policy")),
            ))))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: false,
        locations: vec![
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
            DirectiveLocation::Interface,
            DirectiveLocation::Scalar,
            DirectiveLocation::Enum,
        ],
    }))
}

fn context_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("context"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("name"),
            ty: Node::new(Type::NonNullNamed(name!("String"))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: true,
        locations: vec![
            DirectiveLocation::Interface,
            DirectiveLocation::Object,
            DirectiveLocation::Union,
        ],
    }))
}

fn from_context_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("fromContext"),
        arguments: vec![Node::new(InputValueDefinition {
            description: None,
            name: name!("field"),
            ty: Node::new(Type::Named(name!("federation__ContextFieldValue"))),
            default_value: None,
            directives: Default::default(),
        })],
        repeatable: false,
        locations: vec![DirectiveLocation::ArgumentDefinition],
    }))
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
