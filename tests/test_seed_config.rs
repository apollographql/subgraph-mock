use rand::RngExt;
use std::path::PathBuf;
use subgraph_mock::{Args, state::RngSource};

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

/// Without a configured seed, the server should fall back to OS-sourced
/// entropy rather than some other implicit fixed seed.
#[test]
fn unconfigured_seed_falls_back_to_os_rng() -> anyhow::Result<()> {
    let args = Args {
        config: None,
        schema: pkg_path("tests/data/schema.graphql"),
    };
    let (_, state) = args.init()?;

    assert!(matches!(state.rng, RngSource::Os));
    Ok(())
}
