mod harness;

#[tokio::test]
async fn port_override() -> anyhow::Result<()> {
    let port = harness::initialize(Some("port_and_caching_override.yaml"))?;
    assert_eq!(port, 9001);

    Ok(())
}
