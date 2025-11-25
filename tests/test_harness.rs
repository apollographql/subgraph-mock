use apollo_compiler::{Schema, validation::Valid};
use serde_json::json;

use harness::{ValidationError, validate_response};

mod harness;

// Sometimes your test harness is elaborate enough to need its own tests.
// Yo dawg, I heard you like tests...

fn get_schema() -> Valid<Schema> {
    let supergraph = include_str!("data/schema.graphql");

    Schema::parse_and_validate(supergraph, "schema.graphql").unwrap()
}

#[test]
fn validate_simple_query() -> Result<(), Vec<ValidationError>> {
    let schema = get_schema();
    let query = r#"
            query {
                user(id: "1") {
                    id
                    name
                    email
                }
            }
        "#;

    let response = json!({
        "user": {
            "id": "1",
            "name": "Isaac M. Good",
            "email": "scrapersgostraighttospam@!valid",
        }
    });

    validate_response(&schema, query, &response)
}

#[test]
fn validate_missing_field() {
    let schema = get_schema();
    let query = r#"
            query {
                user(id: "1") {
                    id
                    name
                    email
                }
            }
        "#;

    let response = json!({
        "user": {
            "id": "1",
            "name": "Isaac M. Good",
        }
    });

    let result = validate_response(&schema, query, &response);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].field, "email");
}

#[test]
fn validate_nested_fields() -> Result<(), Vec<ValidationError>> {
    let schema = get_schema();
    let query = r#"
            query {
                user(id: "1") {
                    id
                    address {
                        city
                        state
                    }
                }
            }
        "#;

    let response = json!({
        "user": {
            "id": "1",
            "address": {
                "city": "Not Telling the Internet",
                "state": "CA"
            }
        }
    });

    validate_response(&schema, query, &response)
}

#[test]
fn validate_array_fields() -> Result<(), Vec<ValidationError>> {
    let schema = get_schema();
    let query = r#"
            query {
                posts {
                    id
                    title
                    author {
                        name
                    }
                }
            }
        "#;

    let response = json!({
        "posts": [
            {
                "id": "1",
                "title": "Post 1",
                "author": {
                    "name": "Isaac M. Good"
                }
            },
            {
                "id": "2",
                "title": "Post 2",
                "author": {
                    "name": "Isaac B. Bad"
                }
            }
        ]
    });

    validate_response(&schema, query, &response)
}

#[test]
fn validate_with_field_alias() -> Result<(), Vec<ValidationError>> {
    let schema = get_schema();
    let query = r#"
            query {
                user(id: "1") {
                    userId: id
                    fullName: name
                }
            }
        "#;

    let response = json!({
        "user": {
            "userId": "1",
            "fullName": "Isaac M. Good",
        }
    });

    validate_response(&schema, query, &response)
}

#[test]
fn validate_inline_fragment() -> Result<(), Vec<ValidationError>> {
    let schema = get_schema();
    let query = r#"
            query {
                user(id: "1") {
                    id
                    ... on User {
                        name
                        email
                    }
                }
            }
        "#;

    let response = json!({
        "user": {
            "id": "1",
            "name": "Isaac M. Good",
            "email": "scrapersgostraighttospam@!valid",
        }
    });

    validate_response(&schema, query, &response)
}

#[test]
fn validate_fragment_spread() -> Result<(), Vec<ValidationError>> {
    let schema = get_schema();
    let query = r#"
            fragment UserDetails on User {
                name
                email
                address {
                    city
                    state
                }
            }

            query {
                user(id: "1") {
                    id
                    ...UserDetails
                }
            }
        "#;

    let response = json!({
        "user": {
            "id": "1",
            "name": "Isaac M. Good",
            "email": "scrapersgostraighttospam@!valid",
            "address": {
                "city": "Not Telling the Internet",
                "state": "CA"
            }
        }
    });

    validate_response(&schema, query, &response)
}

#[test]
fn validate_fragment_spread_missing_field() {
    let schema = get_schema();
    let query = r#"
            fragment UserDetails on User {
                name
                email
                address {
                    city
                    state
                }
            }

            query {
                user(id: "1") {
                    id
                    ...UserDetails
                }
            }
        "#;

    let response = json!({
        "user": {
            "id": "1",
            "name": "Isaac M. Good",
            "email": "scrapersgostraighttospam@!valid",
            // missing address
        }
    });

    let result = validate_response(&schema, query, &response);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].field, "address");
}
