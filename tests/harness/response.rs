use apollo_compiler::executable::{Selection, SelectionSet};
use apollo_compiler::validation::Valid;
use apollo_compiler::{ExecutableDocument, Schema};
use cached::proc_macro::cached;
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json_bytes::{Value, serde_json};
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Display, Formatter};
use std::hash::{DefaultHasher, Hash, Hasher};
use subgraph_mock::handle::ByteResponse;

#[derive(Debug, Deserialize, PartialEq)]
#[allow(dead_code)]
pub struct Address {
    pub street_address1: Option<String>,
    pub street_address2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub post_code: Option<String>,
    pub country: Option<String>,
    #[serde(flatten)]
    pub aliased: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[allow(dead_code)]
pub struct Post {
    pub id: Option<u64>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub author: Option<User>,
    pub featured_image: Option<String>,
    pub views: Option<i32>,
    #[serde(flatten)]
    pub aliased: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[allow(dead_code)]
pub struct User {
    pub id: Option<u64>,
    pub posts: Option<Vec<Post>>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub bio: Option<String>,
    pub address: Option<Address>,
    pub is_active: Option<bool>,
    pub distance: Option<f64>,
    #[serde(flatten)]
    pub aliased: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[allow(dead_code)]
pub struct Query {
    pub posts: Option<Vec<Post>>,
    pub post: Option<Post>,
    pub user: Option<User>,
    pub users: Option<Vec<User>>,
    #[serde(flatten)]
    pub aliased: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQLError {
    pub message: String,
    pub path: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Response {
    pub data: Option<Query>,
    #[serde(default)]
    pub errors: Vec<GraphQLError>,
}

#[derive(Debug, Clone)]
pub struct ValidationError {
    pub path: String,
    pub field: String,
    pub message: String,
}

impl Display for ValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Field '{}' at path '{}': {}",
            self.field, self.path, self.message
        )
    }
}

/// Parses a raw [ByteResponse] from the mock subgraph server into a modeled [Query] for making test assertions
pub async fn parse_response(response: ByteResponse) -> anyhow::Result<Query> {
    let body = response.into_body().collect().await?;
    let value: Response = serde_json::from_slice(&body.to_bytes())?;
    Ok(value
        .data
        .expect("data should be present on successful responses"))
}

/// Parses a raw [ByteResponse] from the mock subgraph server into a modeled [Response] that includes both
/// the GraphQL `data` and `errors` for making test assertions.
pub async fn parse_response_with_errors(response: ByteResponse) -> anyhow::Result<Response> {
    let body = response.into_body().collect().await?;
    serde_json::from_slice(&body.to_bytes()).map_err(|err| err.into())
}

#[cached(key = "u64", convert = "{_query_hash}")]
fn parse_and_validate(
    schema: &Valid<Schema>,
    query: &str,
    _query_hash: u64,
) -> Result<Valid<ExecutableDocument>, Vec<ValidationError>> {
    ExecutableDocument::parse_and_validate(schema, query, "generated-query.graphql").map_err(|e| {
        vec![ValidationError {
            path: String::new(),
            field: String::new(),
            message: format!("Failed to parse query: {}", e),
        }]
    })
}

/// Validates that a response contains all fields requested in an arbitrary GraphQL query against a given schema
pub fn validate_response(
    schema: &Valid<Schema>,
    query: &str,
    mut response: Value,
) -> Result<(), Vec<ValidationError>> {
    let response_object = response.as_object_mut().ok_or(vec![ValidationError {
        path: String::new(),
        field: String::new(),
        message: "Response should be a JSON object".to_owned(),
    }])?;

    let data = response_object
        .remove("data")
        .expect("response should have data");

    let has_errors = response_object.contains_key("errors");
    let errored_fields: Option<HashSet<String>> = response_object
        .remove("errors")
        .and_then(|errors| serde_json_bytes::from_value::<Vec<GraphQLError>>(errors).ok())
        .map(|error_vec| {
            error_vec
                .into_iter()
                .filter_map(|error| error.path.and_then(|mut path| path.pop()))
                .collect()
        });

    // Data is allowed to be null iff errors are present
    if data.is_null() && has_errors {
        return Ok(());
    }

    let mut hasher = DefaultHasher::new();
    query.hash(&mut hasher);
    let query_hash = hasher.finish();

    let document = parse_and_validate(schema, query, query_hash)?;

    let mut errors = Vec::new();

    let operation = document.operations.get(None).ok();

    if let Some(op) = operation {
        validate_selection_set(
            "",
            &op.selection_set,
            &data,
            &errored_fields,
            &document,
            &mut errors,
        );
    } else {
        errors.push(ValidationError {
            path: String::new(),
            field: String::new(),
            message: "No operation found in query".to_owned(),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// NB: cannot handle union types
fn validate_selection_set(
    path: &str,
    selection_set: &SelectionSet,
    response: &Value,
    errored_fields: &Option<HashSet<String>>,
    document: &ExecutableDocument,
    errors: &mut Vec<ValidationError>,
) {
    match response {
        Value::Object(obj) => {
            for selection in &selection_set.selections {
                match selection {
                    Selection::Field(field) => {
                        let field_name = field.response_key();

                        if !obj.contains_key(field_name.as_str())
                            && !errored_fields
                                .as_ref()
                                .is_some_and(|fields| fields.contains(field_name.as_str()))
                        {
                            errors.push(ValidationError {
                                path: path.to_string(),
                                field: field_name.to_string(),
                                message: "Field is missing in response".to_string(),
                            });
                        } else if obj.contains_key(field_name.as_str()) {
                            let field_value = &obj[field_name.as_str()];
                            let new_path = if path.is_empty() {
                                field_name.to_string()
                            } else {
                                format!("{}.{}", path, field_name)
                            };

                            // If the field has a selection set, validate nested fields
                            if !field.selection_set.selections.is_empty() {
                                if field_value.is_null()
                                    && field.ty().is_non_null()
                                    && !errored_fields
                                        .as_ref()
                                        .is_some_and(|fields| fields.contains(field.name.as_str()))
                                {
                                    errors.push(ValidationError {
                                        path: new_path,
                                        field: field_name.to_string(),
                                        message: "Field is null but has requested subfields"
                                            .to_string(),
                                    });
                                } else {
                                    match field_value {
                                        Value::Array(arr) => {
                                            for (idx, item) in arr.iter().enumerate() {
                                                let array_path = format!("{}[{}]", new_path, idx);
                                                validate_selection_set(
                                                    &array_path,
                                                    &field.selection_set,
                                                    item,
                                                    errored_fields,
                                                    document,
                                                    errors,
                                                );
                                            }
                                        }
                                        Value::Object(_) => {
                                            validate_selection_set(
                                                &new_path,
                                                &field.selection_set,
                                                field_value,
                                                errored_fields,
                                                document,
                                                errors,
                                            );
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Selection::FragmentSpread(fragment_spread) => {
                        let fragment_name = &fragment_spread.fragment_name;
                        if let Some(fragment) = document.fragments.get(fragment_name.as_str()) {
                            validate_selection_set(
                                path,
                                &fragment.selection_set,
                                response,
                                errored_fields,
                                document,
                                errors,
                            );
                        } else if !errored_fields
                            .as_ref()
                            .is_some_and(|fields| fields.contains(fragment_name.as_str()))
                        {
                            errors.push(ValidationError {
                                path: path.to_string(),
                                field: fragment_name.to_string(),
                                message: format!("Fragment '{}' not found", fragment_name),
                            });
                        }
                    }
                    Selection::InlineFragment(inline_fragment) => {
                        validate_selection_set(
                            path,
                            &inline_fragment.selection_set,
                            response,
                            errored_fields,
                            document,
                            errors,
                        );
                    }
                }
            }
        }
        // We can get away with ignoring this branch for potential field errors because we only
        // drop top-level fields when generating them and thus this branch can only be hit when
        // we have recursed at least one level into the response.
        Value::Null => {
            for selection in &selection_set.selections {
                if let Selection::Field(field) = selection {
                    errors.push(ValidationError {
                        path: path.to_string(),
                        field: field.response_key().to_string(),
                        message: "Parent object is null".to_string(),
                    });
                }
            }
        }
        _ => {}
    }
}
