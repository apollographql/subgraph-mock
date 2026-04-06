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
            // This produces a query requesting a single nullable user by ID (with id, name,
            // is_active, and distance) plus a list of posts (with views). To see it, run the
            // test with RUST_LOG=debug.
            async {
                let response = make_request(54167, state.clone(), None).await?;
                ensure!(200 == response.status());
                parse_response(response).await
            }
        })
        .collect();

    while let Some(response) = requests.next().await {
        responses.push(response?);
    }

    // the default array length is 0-10
    for response in &responses {
        assert!(
            response
                .posts
                .as_ref()
                .is_some_and(|posts| (0..=10).contains(&posts.len()))
        );
    }

    let (users, posts): (Vec<Option<User>>, Vec<Option<Vec<Post>>>) = responses
        .into_iter()
        .map(|response| (response.user, response.posts))
        .unzip();

    let users: Vec<User> = users.into_iter().flatten().collect();
    let posts: Vec<Post> = posts.into_iter().flatten().flatten().collect();

    let user_count = users.len();

    // the default null_ratio is 1/2, outcome seeded for determinism by the harness
    assert_eq!(478, user_count);

    for user in &users {
        // the default float range is -1.0 to 1.0
        assert!(
            user.distance
                .is_some_and(|distance| (-1.0..=1.0).contains(&distance))
        );
        // the default string length is 1-10
        assert!(
            user.name
                .as_deref()
                .is_some_and(|name| (1..=10).contains(&name.chars().count()))
        );
        // the default ID range is 0-100
        assert!(user.id.is_some_and(|id| (0..=100).contains(&id)));
    }

    let true_count = users
        .iter()
        .filter(|user| user.is_active.expect("is_active should be present"))
        .count();

    // The default boolean generator is 50% true; the seeded run's exact count.
    assert_eq!(240, true_count);

    for post in posts {
        // the default Int range is 0-100
        assert!(post.views.is_some_and(|views| (0..=100).contains(&views)));
    }

    Ok(())
}
