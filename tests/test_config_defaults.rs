use anyhow::ensure;
use futures::stream::{FuturesUnordered, StreamExt};
use harness::{Post, Query, User, assert_is_sine, make_request, parse_response};
use std::time::Duration;

mod harness;

/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
#[tokio::test(start_paused = true)]
async fn default_latency_and_port() -> anyhow::Result<()> {
    let (port, state) = harness::initialize(None, None)?;
    let rng_seed = 0;
    let subgraph_name = None;
    assert_eq!(port, 8080);

    // The default latency generator is a sine wave with a base value of 5 ms, an amplitude of 2,
    // and a period of 10 seconds.
    assert_is_sine(
        5,
        2,
        Duration::from_secs(10),
        rng_seed,
        state,
        subgraph_name,
    )
    .await
}

#[tokio::test]
async fn default_headers() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(None, None)?;
    let response = make_request(42, state, None).await?;
    let headers = response.headers();

    assert_eq!(200, response.status());
    assert_eq!(1, headers.len());

    assert!(headers.contains_key("content-type"));
    Ok(())
}

#[tokio::test]
async fn default_response_generation_caches() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(None, None)?;
    let mut responses: Vec<Query> = Vec::with_capacity(10);
    for _ in 0..10 {
        let response = make_request(4449, state.clone(), None).await?;
        assert_eq!(200, response.status());
        responses.push(parse_response(response).await?);
    }

    // All responses should be the same because they are cached by default
    for (index, response) in responses.iter().enumerate() {
        if index > 0 {
            assert_eq!(response, &responses[index - 1]);
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn default_response_generation() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("default_no_cache.yaml"), None)?;
    let mut responses: Vec<Query> = Vec::with_capacity(1000);
    let mut requests: FuturesUnordered<_> = (0..1000)
        .map(|_| {
            // This produces a query that has all data types represented. To see it, run the test with RUST_LOG=debug.
            async {
                let response = make_request(7, state.clone(), None).await?;
                ensure!(200 == response.status());
                parse_response(response).await
            }
        })
        .collect();

    while let Some(response) = requests.next().await {
        responses.push(response?);
    }

    // This field is a top-level alias in the query that requests a single user by ID (and is hence nullable)
    let user_alias = "nwHYPt6HYPXJ1";

    // user.name and user.is_active are both present in the query, but aliased under these names
    let user_name_alias = "mH3ACoBr2";
    let user_is_active_alias = "wVHIP0WIkF3xJVkyxw3";

    // the default array length is 0-10
    for response in &responses {
        assert!(
            response
                .posts
                .as_ref()
                .is_some_and(|posts| (0..=10).contains(&posts.len()))
        );
    }

    let (users, posts): (Vec<User>, Vec<Post>) = {
        let (users, posts): (Vec<_>, Vec<_>) = responses
            .into_iter()
            .map(|mut response| {
                (
                    // remove aliased values so that we get ownership of the Value underlying them for deserialization
                    response
                        .aliased
                        .remove(user_alias)
                        .and_then(|user| serde_json_bytes::from_value(user).ok()),
                    response.post,
                )
            })
            .collect();

        (
            users.into_iter().flatten().collect(),
            posts.into_iter().flatten().collect(),
        )
    };

    let user_count = users.len();

    // the default null ratio is 50% null
    assert_eq!("0.5", format!("{:.1}", user_count as f32 / 1000.0));

    for user in &users {
        // the default float range is -1.0 to 1.0
        assert!(
            user.distance
                .is_some_and(|distance| (-1.0..=1.0).contains(&distance))
        );
        // the default string length is 1-10
        assert!(
            user.aliased
                .get(user_name_alias)
                .and_then(|name| name.as_str())
                .is_some_and(|name| (1..=10).contains(&name.chars().count()))
        );
        // the default ID range is 0-100
        assert!(user.id.is_some_and(|id| (0..=100).contains(&id)));
    }

    let true_count = users
        .iter()
        .filter(|user| {
            user.aliased
                .get(user_is_active_alias)
                .and_then(|is_active| is_active.as_bool())
                .expect("is_active should be a bool")
        })
        .count();

    // booleans are configured to always be 50% true.
    assert_eq!(
        "0.5",
        format!("{:.1}", true_count as f32 / user_count as f32)
    );

    for post in posts {
        // the default Int range is 0-100
        assert!(post.views.is_some_and(|views| (0..=100).contains(&views)));
    }

    Ok(())
}
