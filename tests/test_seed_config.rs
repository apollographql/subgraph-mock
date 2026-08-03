use rand::RngExt;
use std::path::PathBuf;
use subgraph_mock::Args;

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
        let (_, state) = args.init()?;
        Ok(state.rng.next().random_range(0..u32::MAX))
    };

    assert_eq!(init()?, init()?);
    Ok(())
}

/// Without a configured seed, the RNG falls back to OS-sourced entropy, which
/// should not reproduce the same value run to run.
#[test]
fn unconfigured_seed_is_not_reproducible() -> anyhow::Result<()> {
    let init = || -> anyhow::Result<u32> {
        let args = Args {
            config: None,
            schema: pkg_path("tests/data/schema.graphql"),
        };
        let (_, state) = args.init()?;
        Ok(state.rng.next().random_range(0..u32::MAX))
    };

    assert_ne!(init()?, init()?);
    Ok(())
}
