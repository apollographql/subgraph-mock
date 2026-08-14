use anyhow::{Context, Result};
use std::path::Path;
use subgraph_mock::state::Config;

/// Parses every YAML fixture under `tests/data/config/` plus `example-config.yaml` through the
/// real config-parsing path.
#[test]
fn all_config_fixtures_parse() -> Result<()> {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut checked = 0;

    let fixture_dir = Path::new(root).join("tests/data/config");
    for entry in std::fs::read_dir(&fixture_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
            continue;
        }
        parse(&path)?;
        checked += 1;
    }

    parse(&Path::new(root).join("example-config.yaml"))?;
    checked += 1;

    assert!(checked > 0, "expected to find at least one config fixture");
    Ok(())
}

fn parse(path: &Path) -> Result<()> {
    let contents = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_slice(&contents)?;
    Config::parse_yaml(value).with_context(|| format!("parsing {}", path.display()))?;
    Ok(())
}
