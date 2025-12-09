use harness::{Post, Query, User, make_request, parse_response};

mod harness;

#[tokio::test]
async fn custom_scalars() -> anyhow::Result<()> {
    harness::initialize(Some("custom_scalars.yaml"))?;

    let mut responses: Vec<Query> = Vec::with_capacity(100);
    for _ in 0..100 {
        // This produces a query that has all data types represented. To see it, run the test with RUST_LOG=debug.
        let response = make_request(7, None).await?;
        assert_eq!(200, response.status());
        responses.push(parse_response(response).await?);
    }

    // This field is a top-level alias in the query that requests a single user by ID (and is hence nullable)
    let user_alias = "nwHYPt6HYPXJ1";

    // user.name is present in the query, but aliased under this name
    let user_name_alias = "mH3ACoBr2";

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

    for user in &users {
        assert!(
            user.distance
                .is_some_and(|distance| (-5.0..=5.0).contains(&distance))
        );
        assert!(
            user.aliased
                .get(user_name_alias)
                .and_then(|name| name.as_str())
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
