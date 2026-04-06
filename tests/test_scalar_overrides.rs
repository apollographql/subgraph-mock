use harness::{Post, Query, User, make_request, parse_response};

mod harness;

#[tokio::test]
async fn custom_scalars() -> anyhow::Result<()> {
    let (port, state) = harness::initialize(Some("custom_scalars_and_port.yaml"), None)?;
    assert_eq!(8042, port);

    let mut responses: Vec<Query> = Vec::with_capacity(100);
    for _ in 0..100 {
        // This produces a query requesting a single nullable user by ID (with id, name,
        // is_active, and distance) plus a list of posts (with views). To see it, run the test
        // with RUST_LOG=debug.
        let response = make_request(54167, state.clone(), None).await?;
        assert_eq!(200, response.status());
        responses.push(parse_response(response).await?);
    }

    let (users, posts): (Vec<Option<User>>, Vec<Option<Vec<Post>>>) = responses
        .into_iter()
        .map(|response| (response.user, response.posts))
        .unzip();

    let users: Vec<User> = users.into_iter().flatten().collect();
    let posts: Vec<Post> = posts.into_iter().flatten().flatten().collect();

    for user in &users {
        assert!(
            user.distance
                .is_some_and(|distance| (-5.0..=5.0).contains(&distance))
        );
        assert!(
            user.name
                .as_deref()
                .is_some_and(|name| (10..=20).contains(&name.chars().count()))
        );
        assert!(user.id.is_some_and(|id| (100..=200).contains(&id)));
    }

    // We want to verify that both positive and negative float values work, so this is the one field
    // that has a range in the check above that would still pass even if only the default values of
    // -1.0 to 1.0 were produced. These extra checks assert that we actually moved out of those bounds.
    assert!(
        users
            .iter()
            .filter_map(|user| user.distance)
            .any(|distance| distance > 1.0)
    );

    assert!(
        users
            .iter()
            .filter_map(|user| user.distance)
            .any(|distance| distance < -1.0)
    );

    for post in posts {
        assert!(post.views.is_some_and(|views| (10..=15).contains(&views)));
    }

    Ok(())
}
