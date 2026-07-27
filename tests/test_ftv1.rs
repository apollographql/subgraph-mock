use harness::{initialize, send_request, send_request_with_headers};
use http_body_util::BodyExt;
use serde_json_bytes::{Value, serde_json};
use subgraph_mock::ftv1::{self, Node};
use subgraph_mock::handle::ByteResponse;

mod harness;

const FTV1_HEADER: (&str, &str) = ("apollo-federation-include-trace", "ftv1");

const QUERY: &str = r#"query { user(id: "1") { __typename name email address { city } } }"#;

/// Exercises the traversal branches the plain `QUERY` does not: an alias, a fragment spread, an
/// inline fragment, and a field (`address`) selected from two places that must merge into one node.
const FRAGMENT_QUERY: &str = r#"
query {
  user(id: "1") {
    __typename
    displayName: name
    ...UserContact
    ... on User {
      distance
      address { state }
    }
  }
}
fragment UserContact on User {
  email
  address { city }
}
"#;

/// Collects a response body into its parsed JSON value.
async fn response_json(response: ByteResponse) -> anyhow::Result<Value> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

/// Finds a direct child node by its response name.
fn child<'a>(node: &'a Node, response_name: &str) -> &'a Node {
    node.child
        .iter()
        .find(|node| node.response_name == response_name)
        .unwrap_or_else(|| panic!("expected a `{response_name}` node among {:?}", node.child))
}

/// Asserts that every child's span nests within its parent's and that individual spans are ordered.
fn assert_timing_nested(node: &Node) {
    assert!(
        node.start_time <= node.end_time,
        "node `{}` has start_time after end_time",
        node.response_name
    );
    for child in &node.child {
        assert!(
            node.start_time <= child.start_time && child.end_time <= node.end_time,
            "child `{}` span escapes parent `{}`",
            child.response_name,
            node.response_name
        );
        assert_timing_nested(child);
    }
}

#[tokio::test]
async fn header_emits_ftv1_trace() -> anyhow::Result<()> {
    let (_, state) = initialize(None, None)?;

    let response =
        send_request_with_headers(QUERY.to_owned(), None, state, None, true, &[FTV1_HEADER])
            .await?;
    let body = response_json(response).await?;

    let encoded = body
        .get("extensions")
        .and_then(|extensions| extensions.get("ftv1"))
        .and_then(Value::as_str)
        .expect("response should carry extensions.ftv1");

    let trace = ftv1::decode(encoded)?;
    assert!(
        trace.duration_ns > 0,
        "trace should have a non-zero duration"
    );

    let root = trace.root.as_ref().expect("trace should have a root node");
    assert_timing_nested(root);

    let user = child(root, "user");
    assert_eq!(user.parent_type, "Query");
    assert_eq!(user.r#type, "User");

    // `__typename` is skipped, leaving exactly the three selected fields.
    assert_eq!(user.child.len(), 3);
    assert!(
        user.child
            .iter()
            .all(|node| node.response_name != "__typename")
    );

    let name = child(user, "name");
    assert_eq!(name.parent_type, "User");
    assert_eq!(name.r#type, "String!");
    assert!(name.child.is_empty());

    let email = child(user, "email");
    assert_eq!(email.parent_type, "User");
    assert_eq!(email.r#type, "String!");

    let address = child(user, "address");
    assert_eq!(address.parent_type, "User");
    assert_eq!(address.r#type, "Address!");

    let city = child(address, "city");
    assert_eq!(city.parent_type, "Address");
    assert_eq!(city.r#type, "String!");

    Ok(())
}

#[tokio::test]
async fn no_header_omits_extensions() -> anyhow::Result<()> {
    let (_, state) = initialize(None, None)?;

    let response = send_request(QUERY.to_owned(), None, state, None, true).await?;
    let body = response_json(response).await?;

    assert!(
        body.get("extensions").is_none(),
        "response should not include extensions without the ftv1 header"
    );

    Ok(())
}

#[tokio::test]
async fn config_forces_trace_without_header() -> anyhow::Result<()> {
    let (_, state) = initialize(Some("ftv1.yaml"), None)?;

    let response = send_request(QUERY.to_owned(), None, state, None, true).await?;
    let body = response_json(response).await?;

    assert!(
        body.get("extensions")
            .and_then(|extensions| extensions.get("ftv1"))
            .is_some(),
        "config `ftv1: true` should emit a trace even without the header"
    );

    Ok(())
}

#[tokio::test]
async fn subgraph_override_disables_trace_despite_header() -> anyhow::Result<()> {
    let (_, state) = initialize(Some("ftv1.yaml"), None)?;

    let response = send_request_with_headers(
        QUERY.to_owned(),
        None,
        state,
        Some("no_trace".to_owned()),
        true,
        &[FTV1_HEADER],
    )
    .await?;
    let body = response_json(response).await?;

    assert!(
        body.get("extensions").is_none(),
        "subgraph override `ftv1: false` should suppress the trace even with the header"
    );

    Ok(())
}

#[tokio::test]
async fn aliases_and_fragments_are_traced() -> anyhow::Result<()> {
    let (_, state) = initialize(None, None)?;

    let response = send_request_with_headers(
        FRAGMENT_QUERY.to_owned(),
        None,
        state,
        None,
        true,
        &[FTV1_HEADER],
    )
    .await?;
    let body = response_json(response).await?;

    let encoded = body
        .get("extensions")
        .and_then(|extensions| extensions.get("ftv1"))
        .and_then(Value::as_str)
        .expect("response should carry extensions.ftv1");

    let trace = ftv1::decode(encoded)?;
    let root = trace.root.as_ref().expect("trace should have a root node");
    let user = child(root, "user");

    // `__typename` is skipped; the alias, the fragment-spread fields, and the inline-fragment field
    // remain, with `address` merged into a single node despite being selected from two places.
    assert_eq!(user.child.len(), 4);

    // Aliased field: the response name is the alias, but the type is still `name`'s type.
    let display_name = child(user, "displayName");
    assert_eq!(display_name.parent_type, "User");
    assert_eq!(display_name.r#type, "String!");

    // Pulled in through the fragment spread.
    child(user, "email");

    // Pulled in through the inline fragment.
    let distance = child(user, "distance");
    assert_eq!(distance.r#type, "Float!");

    // Selected in both the fragment (`city`) and the inline fragment (`state`): the two selections
    // merge under one `address` node rather than producing duplicate nodes.
    let address = child(user, "address");
    assert_eq!(address.child.len(), 2);
    child(address, "city");
    child(address, "state");

    Ok(())
}

#[tokio::test]
async fn validation_error_omits_trace() -> anyhow::Result<()> {
    let (_, state) = initialize(None, None)?;

    // `not_a_field` does not exist on `User`, so the request fails validation and returns 400.
    let response = send_request_with_headers(
        r#"query { user(id: "1") { not_a_field } }"#.to_owned(),
        None,
        state,
        None,
        false,
        &[FTV1_HEADER],
    )
    .await?;

    assert_eq!(
        response.status().as_u16(),
        400,
        "an invalid query should return 400"
    );

    let body = response_json(response).await?;
    assert!(
        body.get("extensions").is_none(),
        "a validation-error response should not carry a trace even with the ftv1 header"
    );

    Ok(())
}
