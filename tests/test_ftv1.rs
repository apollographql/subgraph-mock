use harness::{initialize, send_request, send_request_with_headers};
use http_body_util::BodyExt;
use serde_json_bytes::{Value, serde_json};
use std::collections::HashSet;
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

/// Golden encoding of a fixed `Trace`, verified against the router's *real* `reports.proto` rather
/// than just round-tripped against our own encoder/decoder — a round trip can't catch a wrong field
/// tag, since our encoder and decoder would silently agree with each other while the real router
/// miss-decodes.
///
/// To regenerate: build a throwaway crate outside this repo that depends on `protox` + `prost-build`
/// against the router's real `apollo-router/src/plugins/telemetry/proto/reports.proto`. Construct
/// the same fixed `subgraph_mock::ftv1::Trace` value below, encode it with our hand-rolled struct and
/// decode it with the struct prost-build generated from the real proto (and vice versa), asserting
/// every field lands in the same place both ways. Then take the base64 that
/// `subgraph_mock::ftv1::encode` produces for that trace and paste it in below. Discard the scratch
/// crate afterward.
const GOLDEN_TRACE_B64: &str = "GgsIgOLPqgYQgMCEPSILCIDiz6oGEMCp0zpYwJaxAnKBAUjAlrECYnoKBHVzZXIaBVVzZXIhSICS9AFiGQoEbmFtZRoHU3RyaW5nIUjAhD1qBFVzZXJiRAoHYWRkcmVzcxoIQWRkcmVzcyFAwIQ9SMCNtwFiIAoEY2l0eRoHU3RyaW5nIUDAhD1IgIl6agdBZGRyZXNzagRVc2VyagVRdWVyeQ==";

#[test]
fn golden_trace_decodes_to_expected_shape() {
    let trace = ftv1::decode_trace(GOLDEN_TRACE_B64).expect("golden trace should decode");

    assert_eq!(trace.duration_ns, 5_000_000);

    let root = trace.root.expect("trace should have a root node");
    assert_eq!(root.child.len(), 1);

    let user = &root.child[0];
    assert_eq!(user.response_name, "user");
    assert_eq!(user.r#type, "User!");
    assert_eq!(user.parent_type, "Query");
    assert_eq!(user.child.len(), 2);

    let name = &user.child[0];
    assert_eq!(name.response_name, "name");
    assert_eq!(name.r#type, "String!");
    assert_eq!(name.parent_type, "User");
    assert!(name.child.is_empty());

    let address = &user.child[1];
    assert_eq!(address.response_name, "address");
    assert_eq!(address.r#type, "Address!");
    assert_eq!(address.parent_type, "User");
    assert_eq!(address.child.len(), 1);

    let city = &address.child[0];
    assert_eq!(city.response_name, "city");
    assert_eq!(city.r#type, "String!");
    assert_eq!(city.parent_type, "Address");
    assert!(city.child.is_empty());
}

/// Complements the decode-only golden test above: confirms our `encode` reproduces the
/// externally-verified golden bytes exactly for the same decoded value, rather than just being an
/// inverse of our own `decode` (which a self-round-trip alone can't distinguish from a
/// consistently-wrong pair of functions).
#[test]
fn golden_trace_round_trips_through_encode() {
    let trace = ftv1::decode_trace(GOLDEN_TRACE_B64).expect("golden trace should decode");
    assert_eq!(ftv1::encode_trace(&trace), GOLDEN_TRACE_B64);
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

    let trace = ftv1::decode_trace(encoded)?;
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

    let trace = ftv1::decode_trace(encoded)?;
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

/// Decodes an [`ftv1::Error::json`] string back into a [`Value`], the same shape the mock originally
/// injected into the response body.
fn error_json(error: &ftv1::Error) -> anyhow::Result<Value> {
    Ok(serde_json::from_str(&error.json)?)
}

/// Asserts `error.json` round-trips to an object carrying the `extensions.code` the mock injects
/// into both error shapes.
fn assert_carries_extensions_code(error: &ftv1::Error) {
    let json = error_json(error).expect("error.json should be valid JSON");
    let code = json
        .get("extensions")
        .and_then(|extensions| extensions.get("code"))
        .and_then(Value::as_str);
    assert_eq!(
        code,
        Some("INTERNAL_SERVER_ERROR"),
        "error.json should carry extensions.code: {json:?}"
    );
}

#[tokio::test]
async fn request_error_traces_on_root() -> anyhow::Result<()> {
    let (_, state) = initialize(Some("ftv1_request_error.yaml"), None)?;

    let response = send_request(QUERY.to_owned(), None, state, None, true).await?;
    let body = response_json(response).await?;

    let encoded = body
        .get("extensions")
        .and_then(|extensions| extensions.get("ftv1"))
        .and_then(Value::as_str)
        .expect("response should carry extensions.ftv1");

    let trace = ftv1::decode_trace(encoded)?;
    let root = trace.root.as_ref().expect("trace should have a root node");

    assert_eq!(
        root.error.len(),
        1,
        "the path-less request error should attach to root"
    );
    let error = &root.error[0];
    assert_eq!(error.message, "Request error simulated");
    assert_carries_extensions_code(error);

    // The request error is a whole-operation failure, so no child node should carry it.
    let user = child(root, "user");
    assert!(user.error.is_empty());

    Ok(())
}

#[tokio::test]
async fn field_error_traces_on_named_child() -> anyhow::Result<()> {
    let (_, state) = initialize(Some("ftv1_field_error.yaml"), None)?;

    // `QUERY`'s only top-level field is `user`, so the mock's field-error injection (which only ever
    // drops top-level fields) has exactly one field it can pick.
    let response = send_request(QUERY.to_owned(), None, state, None, true).await?;
    let body = response_json(response).await?;

    let encoded = body
        .get("extensions")
        .and_then(|extensions| extensions.get("ftv1"))
        .and_then(Value::as_str)
        .expect("response should carry extensions.ftv1");

    let trace = ftv1::decode_trace(encoded)?;
    let root = trace.root.as_ref().expect("trace should have a root node");
    assert!(
        root.error.is_empty(),
        "a field error should not attach to root"
    );

    let user = child(root, "user");
    assert_eq!(
        user.error.len(),
        1,
        "the field error's path should resolve to the `user` node"
    );
    let error = &user.error[0];
    assert_eq!(error.message, "Field error simulated");
    assert_carries_extensions_code(error);

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

/// Query against `schema_with_union` selecting both `Content` union members' fields. `title` and
/// `content` are common to both `Post` and `Article`; `author { name }`/`views` are Post-only and
/// `author { email }`/`citations` are Article-only.
const UNION_QUERY: &str = r#"
query {
  user(id: "1") {
    content {
      __typename
      ... on Post { title content author { name } views }
      ... on Article { title content author { email } citations }
    }
  }
}
"#;

#[tokio::test(flavor = "multi_thread")]
async fn abstract_type_fragments_prune_to_actual_response_shape() -> anyhow::Result<()> {
    let schema = "schema_with_union".to_string();
    let (_, state) = initialize(Some("ftv1_union.yaml"), Some(&schema))?;

    let mut saw_pruned_field = false;

    for _ in 0..25 {
        let response = send_request_with_headers(
            UNION_QUERY.to_owned(),
            Some(schema.clone()),
            state.clone(),
            None,
            false,
            &[FTV1_HEADER],
        )
        .await?;
        let body = response_json(response).await?;

        let content_array = body
            .get("data")
            .and_then(|data| data.get("user"))
            .and_then(|user| user.get("content"))
            .and_then(Value::as_array)
            .expect("content should be an array");
        assert!(
            !content_array.is_empty(),
            "ftv1_union.yaml's forced array length should keep this non-empty"
        );

        // The response keys that actually appeared across every generated `content` element.
        let mut actual_keys: HashSet<&str> = HashSet::new();
        for element in content_array {
            if let Some(object) = element.as_object() {
                actual_keys.extend(object.keys().map(|key| key.as_str()));
            }
        }

        let encoded = body
            .get("extensions")
            .and_then(|extensions| extensions.get("ftv1"))
            .and_then(Value::as_str)
            .expect("response should carry extensions.ftv1");
        let trace = ftv1::decode_trace(encoded)?;
        let root = trace.root.expect("trace should have a root node");
        let user = child(&root, "user");
        let content = child(user, "content");

        for traced in &content.child {
            assert!(
                actual_keys.contains(traced.response_name.as_str()),
                "trace claims field `{}`, which never appeared in any generated `content` element \
                 ({actual_keys:?})",
                traced.response_name
            );
        }

        // The query spans 5 fields across both union members (excluding `__typename`): `title`,
        // `content`, `author`, `views` (Post-only), `citations` (Article-only). If the trace ever
        // comes back with fewer than all 5, pruning actually dropped something rather than always
        // taking the permissive "no info" fallback.
        if content.child.len() < 5 {
            saw_pruned_field = true;
        }
    }

    assert!(
        saw_pruned_field,
        "expected at least one of the 25 requests to generate a `content` list missing one union \
         member's fields, proving `prune_to_response` drops the fields that don't apply"
    );

    Ok(())
}
