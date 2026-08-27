use serde_yaml::Value;
use std::path::Path;
use subgraph_mock::{error::Error, state::Config};

fn parse_err(yaml: &str) -> Error {
    let value: Value = serde_yaml::from_str(yaml).expect("test fixture should be valid YAML");
    Config::parse_yaml(value).expect_err("expected a config error")
}

#[test]
fn top_level_not_a_mapping() {
    assert!(matches!(
        parse_err("- just\n- a\n- list\n"),
        Error::NotAMapping
    ));
}

#[test]
fn subgraph_overrides_value_not_a_mapping() {
    // `subgraph_overrides` present, but its *value* isn't a mapping -- distinct from an
    // individual override entry not being one (see below).
    assert!(matches!(
        parse_err("subgraph_overrides: \"oops\"\n"),
        Error::NotAMapping
    ));
}

#[test]
fn subgraph_override_entry_not_a_mapping() {
    match parse_err("subgraph_overrides:\n  foo: \"also oops\"\n") {
        Error::OverrideNotAMapping { subgraph } => assert_eq!(subgraph, "foo"),
        other => panic!("expected OverrideNotAMapping, got {other:?}"),
    }
}

#[test]
fn non_string_subgraph_key_is_a_yaml_error() {
    // YAML permits non-string mapping keys; `subgraph_overrides`' keys must decode as `String`.
    assert!(matches!(
        parse_err("subgraph_overrides:\n  123: {}\n"),
        Error::Yaml { .. }
    ));
}

#[test]
fn invalid_header_name_is_rejected() {
    assert!(matches!(
        parse_err("headers:\n  \"bad header\": \"value\"\n"),
        Error::InvalidHeaderName { .. }
    ));
}

#[test]
fn invalid_header_value_is_rejected() {
    assert!(matches!(
        parse_err("headers:\n  \"X-Test\": \"bad\\nvalue\"\n"),
        Error::InvalidHeaderValue { .. }
    ));
}

#[test]
fn schema_validation_failure_wraps_config_error() {
    assert!(matches!(
        parse_err("port: not-a-number\n"),
        Error::Config(_)
    ));
}

#[test]
fn nonexistent_config_file_is_an_io_error() {
    let err = Config::from_file(Path::new("/definitely/does/not/exist.yaml"))
        .expect_err("expected an io error");
    assert!(matches!(err, Error::Io { .. }));
}
