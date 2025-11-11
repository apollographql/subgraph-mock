use std::sync::OnceLock;

use crate::harness::make_request;

mod harness;

static PORT: OnceLock<u16> = OnceLock::new();

#[ctor::ctor]
fn init() {
    PORT.set(harness::initialize(None).unwrap()).unwrap();
}

#[test]
fn test_default_port() -> anyhow::Result<()> {
    assert_eq!(PORT.get(), Some(8080).as_ref());
    Ok(())
}

#[tokio::test]
async fn test_default_headers() -> anyhow::Result<()> {
    let response = make_request(42, None).await?;
    let headers = response.headers();

    assert_eq!(200, response.status());
    assert_eq!(1, headers.len());

    assert!(headers.contains_key("content-type"));
    Ok(())
}
