use harness::make_request;
use http_body_util::BodyExt;
use rand::RngExt;
use std::path::PathBuf;
use subgraph_mock::{Args, state::RngSource};

mod harness;

fn pkg_path(relative: &str) -> PathBuf {
    PathBuf::from(format!("{}/{relative}", env!("CARGO_MANIFEST_DIR")))
}

/// Configuring a seed should make the server's RNG stream reproducible across
/// independent process/config initializations, bypassing the test harness's
/// own seed override so the config -> RngSource wiring is actually exercised.
#[test]
fn configured_seed_is_reproducible() -> anyhow::Result<()> {
    let init = || -> anyhow::Result<u32> {
        let args = Args {
            config: Some(pkg_path("tests/data/config/seed.yaml")),
            schema: pkg_path("tests/data/schema.graphql"),
        };
        let (_, state, _) = args.init()?;
        Ok(state.rng.next().random_range(0..u32::MAX))
    };

    assert_eq!(init()?, init()?);
    Ok(())
}

/// Without a configured seed, the server should fall back to OS-sourced
/// entropy rather than some other implicit fixed seed.
#[test]
fn unconfigured_seed_falls_back_to_os_rng() -> anyhow::Result<()> {
    let args = Args {
        config: None,
        schema: pkg_path("tests/data/schema.graphql"),
    };
    let (_, state, _) = args.init()?;

    assert!(matches!(state.rng, RngSource::Os));
    Ok(())
}

/// Response generation caching must not sidestep the configured seed: a cached and an
/// uncached server given the same seed and the same query should generate byte-identical
/// responses, since both draw from the same RNG stream. Regression test for a bug where the
/// cached path sourced its own RNG from OS entropy instead of the seeded `RngSource`.
#[tokio::test]
async fn cache_responses_honors_configured_seed() -> anyhow::Result<()> {
    let (_, cached_state) = harness::initialize(None, None)?;
    let (_, uncached_state) = harness::initialize(Some("default_no_cache.yaml"), None)?;

    let query_seed = 4242;
    let cached_response = make_request(query_seed, cached_state, None).await?;
    let uncached_response = make_request(query_seed, uncached_state, None).await?;

    assert_eq!(cached_response.status(), uncached_response.status());

    let cached_bytes = cached_response.into_body().collect().await?.to_bytes();
    let uncached_bytes = uncached_response.into_body().collect().await?.to_bytes();

    assert_eq!(
        cached_bytes, uncached_bytes,
        "cached and uncached responses should be generated from the same rng stream given the same seed"
    );

    Ok(())
}
