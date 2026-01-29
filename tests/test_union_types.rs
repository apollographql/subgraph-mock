use anyhow::ensure;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use crate::harness::{parse_response, send_request, Query};

mod harness;

#[tokio::test(flavor = "multi_thread")]
async fn union_test() -> anyhow::Result<()> {
    let schema = "schema_with_union".to_string();
    let (_, state) = harness::initialize(Some("no_null.yaml"), Some(&schema))?;
    let query = "\
    {
      user(id: 1) {
        content {
          __typename
          ... on Post {
            title
            content
            author { name }
            views
          }
          ... on Article {
            title
            content
            author { email }
            citations
          }
        }
      }
    }
    ";

    let mut responses: Vec<Query> = Vec::with_capacity(1000);
    let mut requests: FuturesUnordered<_> = (0..1)
        .map(|_| {
            // This produces a query that has all data types represented. To see it, run the test with RUST_LOG=debug.
            async {
                let response = send_request(query.to_string(), Some(schema.clone()), state.clone(), None).await?;
                ensure!(200 == response.status());
                parse_response(response).await
            }
        })
        .collect();

    while let Some(response) = requests.next().await {
        responses.push(response?);
    }


    // for each user, check that the content typename is either Article or Post, _not_ Content. Content
    // is a union and shouldn't be what's returned.
    for response in responses {
        let user = response.user.expect("missing user from response");
        let content = user.aliased.get("content").unwrap().as_array().unwrap();
        for element in content {
            let typename = element.as_object().unwrap().get("__typename").unwrap().as_str().unwrap();
            assert_ne!(typename, "Content", "element = {:?}", element);
        }
    }

    Ok(())
}
