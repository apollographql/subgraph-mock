use anyhow::ensure;
use http_body_util::BodyExt;
use serde_json_bytes::{Value, json, serde_json};
use subgraph_mock::handle::ByteResponse;

mod harness;

const ENTITIES_QUERY: &str = "\
query ($representations: [_Any!]!) {
  _entities(representations: $representations) {
    __typename
    ... on User {
      id
      name
      reviewCount
      organization {
        id
        name
      }
    }
    ... on Product {
      sku
      price
    }
  }
}
";

async fn response_json(response: ByteResponse) -> anyhow::Result<Value> {
    ensure!(200 == response.status());
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

fn entities(response: &Value) -> &[Value] {
    response
        .get("data")
        .expect("response should have data")
        .get("_entities")
        .expect("response should have _entities")
        .as_array()
        .expect("_entities should be an array")
}

#[tokio::test(flavor = "multi_thread")]
async fn entities_align_with_representations() -> anyhow::Result<()> {
    let schema = "federated".to_string();
    let (_, state) = harness::initialize(Some("no_null.yaml"), Some(&schema))?;

    let representations = json!([
        { "__typename": "User", "id": "u-1", "organization": { "id": "org-1" } },
        { "__typename": "Product", "sku": "sku-9" },
        { "__typename": "User", "id": "u-2", "organization": { "id": "org-2" } },
    ]);
    let mut variables = serde_json_bytes::Map::new();
    variables.insert("representations", representations);

    let response = harness::send_request_with_variables(
        ENTITIES_QUERY.to_string(),
        variables,
        None,
        state,
        &None,
        false,
    )
    .await?;
    let response = response_json(response).await?;
    let entities = entities(&response);

    ensure!(
        entities.len() == 3,
        "entity list length must match representations, got {}",
        entities.len()
    );

    let user_1 = &entities[0];
    ensure!(user_1.get("__typename").unwrap().as_str() == Some("User"));
    ensure!(user_1.get("id").unwrap().as_str() == Some("u-1"));
    let organization = user_1.get("organization").unwrap();
    ensure!(
        organization.get("id").unwrap().as_str() == Some("org-1"),
        "nested key fields must echo the representation"
    );
    ensure!(
        organization.get("name").unwrap().is_string(),
        "fields not in the representation should be generated"
    );
    ensure!(user_1.get("name").unwrap().is_string());
    ensure!(user_1.get("reviewCount").unwrap().is_number());

    let product = &entities[1];
    ensure!(product.get("__typename").unwrap().as_str() == Some("Product"));
    ensure!(product.get("sku").unwrap().as_str() == Some("sku-9"));
    ensure!(product.get("price").unwrap().is_number());
    ensure!(
        product.get("id").is_none(),
        "Product must not carry User fields"
    );

    let user_2 = &entities[2];
    ensure!(user_2.get("__typename").unwrap().as_str() == Some("User"));
    ensure!(user_2.get("id").unwrap().as_str() == Some("u-2"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_typename_yields_null_entity() -> anyhow::Result<()> {
    let schema = "federated".to_string();
    let (_, state) = harness::initialize(Some("no_null.yaml"), Some(&schema))?;

    let mut variables = serde_json_bytes::Map::new();
    variables.insert(
        "representations",
        json!([
            { "__typename": "NoSuchType", "id": "x" },
            { "__typename": "Product", "sku": "sku-1" },
        ]),
    );

    let response = harness::send_request_with_variables(
        ENTITIES_QUERY.to_string(),
        variables,
        None,
        state,
        &None,
        false,
    )
    .await?;
    let response = response_json(response).await?;
    let entities = entities(&response);

    ensure!(entities.len() == 2);
    ensure!(entities[0].is_null(), "unknown __typename must yield null");
    ensure!(entities[1].get("sku").unwrap().as_str() == Some("sku-1"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn inline_representations_align_without_variables() -> anyhow::Result<()> {
    let schema = "federated".to_string();
    let (_, state) = harness::initialize(Some("no_null.yaml"), Some(&schema))?;

    let query = r#"
    query {
      _entities(representations: [{ __typename: "Product", sku: "sku-inline" }]) {
        __typename
        ... on Product { sku price }
      }
    }
    "#;

    let response = harness::send_request(query.to_string(), None, state, &None, false).await?;
    let response = response_json(response).await?;
    let entities = entities(&response);

    ensure!(entities.len() == 1);
    ensure!(entities[0].get("sku").unwrap().as_str() == Some("sku-inline"));

    Ok(())
}

/// The response cache must not serve one request's entities for another request with the
/// same query text but different representations.
#[tokio::test(flavor = "multi_thread")]
async fn cached_responses_vary_with_representations() -> anyhow::Result<()> {
    let schema = "federated".to_string();
    let (_, state) = harness::initialize(Some("entities_cache.yaml"), Some(&schema))?;

    for sku in ["sku-a", "sku-b"] {
        let mut variables = serde_json_bytes::Map::new();
        variables.insert(
            "representations",
            json!([{ "__typename": "Product", "sku": sku }]),
        );

        let response = harness::send_request_with_variables(
            ENTITIES_QUERY.to_string(),
            variables,
            None,
            state.clone(),
            &None,
            false,
        )
        .await?;
        let response = response_json(response).await?;
        let entities = entities(&response);

        ensure!(entities.len() == 1);
        ensure!(
            entities[0].get("sku").unwrap().as_str() == Some(sku),
            "cached response must match this request's representations"
        );
    }

    Ok(())
}
