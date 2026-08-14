use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode, body::Bytes};
use serde_json_bytes::{Value, serde_json};
use std::sync::Arc;
use subgraph_mock::{handle::handle_request, state::State};

mod harness;

async fn post(state: Arc<State>, body: &str) -> anyhow::Result<(StatusCode, Value)> {
    let req = Request::builder()
        .method("POST")
        .uri("/")
        .body(Full::<Bytes>::from(body.to_owned()))?;
    let (parts, body) = handle_request(req, state).await?.into_parts();
    let bytes = body.collect().await?.to_bytes();
    Ok((parts.status, serde_json::from_slice(&bytes)?))
}

/// Every variant renders through the same envelope; this pulls out the bits every test checks so
/// each test only has to state what's variant-specific.
fn first_error(json: &Value) -> (&str, &Value) {
    assert_eq!(json.get("data"), Some(&Value::Null), "got {json:?}");
    let errors = json
        .get("errors")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected an errors array, got {json:?}"));
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error object, got {errors:?}"
    );

    let extensions = errors[0]
        .get("extensions")
        .unwrap_or_else(|| panic!("expected extensions, got {:?}", errors[0]));
    let code = extensions
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected extensions.code, got {extensions:?}"));

    (code, extensions)
}

#[tokio::test]
async fn malformed_json_is_rejected() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(None, None)?;

    let (status, json) = post(state, "{ not valid json").await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (code, _) = first_error(&json);
    assert_eq!(code, "graphql::invalid_json");

    Ok(())
}

#[tokio::test]
async fn invalid_query_carries_every_diagnostic() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(None, None)?;

    // Two unknown fields -- confirmed empirically (examples/scratch_request_errors.rs, since
    // deleted) that apollo_compiler reports *four* diagnostics for this shape, not two: each
    // unknown field also fails the "must have a subselection set" check on `Post`.
    let body = serde_json::json!({
        "query": "{ posts { unknownField1 } post(id: \"1\") { unknownField2 } }",
    })
    .to_string();

    let (status, json) = post(state, &body).await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (code, extensions) = first_error(&json);
    assert_eq!(code, "graphql::invalid_query");

    let diagnostics = extensions
        .get("errors")
        .and_then(Value::as_array)
        .expect("extensions.errors should be an array of every diagnostic");
    assert_eq!(diagnostics.len(), 4);
    for diagnostic in diagnostics {
        assert!(diagnostic.get("message").is_some());
        assert!(diagnostic.get("locations").is_some());
    }

    Ok(())
}

#[tokio::test]
async fn missing_variable_on_introspection_is_a_request_error() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(None, None)?;

    // Regular (non-introspection) queries never coerce variables at all -- confirmed live -- so
    // this has to be an introspection query to actually reach `coerce_variable_values`.
    let body = serde_json::json!({
        "query": "query($name: String!) { __type(name: $name) { name } }",
    })
    .to_string();

    let (status, json) = post(state, &body).await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (code, extensions) = first_error(&json);
    assert_eq!(code, "graphql::request_error");
    assert_eq!(
        extensions.get("message").and_then(Value::as_str),
        Some("missing value for non-null variable 'name'")
    );
    assert!(extensions.get("locations").is_some());

    Ok(())
}

#[tokio::test]
async fn mutations_are_not_implemented() -> anyhow::Result<()> {
    // `schema.graphql` (the default fixture) has no mutation type at all, so `mutation { ... }`
    // against it fails validation before ever reaching this check -- confirmed live. This fixture
    // exists solely so `NotImplemented` is reachable.
    let (_, state) = harness::initialize(None, Some("schema_with_mutation"))?;

    let body = serde_json::json!({
        "query": "mutation { createPost(title: \"hi\") { id } }",
    })
    .to_string();

    let (status, json) = post(state, &body).await?;

    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let (code, extensions) = first_error(&json);
    assert_eq!(code, "graphql::not_implemented");
    assert_eq!(
        extensions.get("operation_type").and_then(Value::as_str),
        Some("mutation")
    );

    Ok(())
}
