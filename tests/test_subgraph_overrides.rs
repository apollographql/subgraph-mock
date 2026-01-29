use harness::{make_request, parse_response};

mod harness;

#[tokio::test]
async fn subgraph_overrides() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("subgraph_override.yaml"), None)?;

    let standard_response = make_request(18, state.clone(), None).await?;
    let subgraph_response = make_request(18, state, Some("special_subgraph".to_owned())).await?;

    assert_eq!(
        standard_response
            .headers()
            .get("test-header")
            .and_then(|header| header.to_str().ok()),
        Some("test-header-normal-value")
    );

    assert_eq!(
        subgraph_response
            .headers()
            .get("test-header")
            .and_then(|header| header.to_str().ok()),
        Some("test-header-overridden-value")
    );

    let standard_body = parse_response(standard_response).await?;
    let subgraph_body = parse_response(subgraph_response).await?;

    assert!(
        standard_body
            .posts
            .is_some_and(|posts| (0..=10).contains(&posts.len()))
    );

    assert!(
        subgraph_body
            .posts
            .is_some_and(|posts| (11..=20).contains(&posts.len()))
    );

    Ok(())
}
