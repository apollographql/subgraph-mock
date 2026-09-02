use super::FederationType;
use apollo_compiler::{
    Node, Schema,
    ast::{Definition, EnumValueDefinition, FieldDefinition, InputValueDefinition, Type},
    name,
    schema::{
        Component, DirectiveDefinition, DirectiveLocation, EnumType, ExtendedType, ObjectType,
        ScalarType,
    },
    ty,
};

/// Directives that must be implicitly injected into federated schemas:
/// https://www.apollographql.com/docs/graphos/schema-design/federated-schemas/reference/subgraph-spec#subgraph-schema-additions
pub fn federation_directives() -> Vec<Definition> {
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
pub fn insert_federation_types(schema: &mut Schema, federation_type: &FederationType) {
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

pub fn link_definition() -> Definition {
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

pub fn key_definition() -> Definition {
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

pub fn requires_definition() -> Definition {
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

pub fn provides_definition() -> Definition {
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

pub fn external_definition() -> Definition {
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

pub fn tag_definition() -> Definition {
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

pub fn shareable_definition() -> Definition {
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

pub fn inaccessible_definition() -> Definition {
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

pub fn override_definition() -> Definition {
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

pub fn compose_directive_definition() -> Definition {
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

pub fn interface_object_definition() -> Definition {
    Definition::DirectiveDefinition(Node::new(DirectiveDefinition {
        description: None,
        name: name!("interfaceObject"),
        arguments: vec![],
        repeatable: false,
        locations: vec![DirectiveLocation::Object],
    }))
}

pub fn authenticated_definition() -> Definition {
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

pub fn requires_scopes_definition() -> Definition {
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

pub fn policy_definition() -> Definition {
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

pub fn context_definition() -> Definition {
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

pub fn from_context_definition() -> Definition {
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

pub fn defer_definition() -> Node<DirectiveDefinition> {
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

pub fn stream_definition() -> Node<DirectiveDefinition> {
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
