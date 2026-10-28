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

/// Asserts that `yaml` fails validation with a message containing `expected`.
fn assert_config_error(yaml: &str, expected: &str) {
    match parse_err(yaml) {
        Error::Config(err) => {
            let report = format!("{:?}", miette::Report::new(err));
            assert!(
                report.contains(expected),
                "expected {expected:?} in:\n{report}"
            );
        }
        other => panic!("expected a config error, got {other:?}"),
    }
}

#[test]
fn failure_schedule_requires_duration() {
    // A required field missing inside an optional section fails the schema's `anyOf` rather than
    // naming the field, so check the report points at the schedule.
    assert_config_error("failure_schedule:\n  start: 10s\n", "{\"start\":\"10s\"}");
}

#[test]
fn failure_schedule_duration_must_be_positive() {
    assert_config_error(
        "failure_schedule:\n  duration: 0s\n",
        "duration must be greater than zero",
    );
}

#[test]
fn failure_schedule_period_must_exceed_duration() {
    assert_config_error(
        "failure_schedule:\n  duration: 30s\n  period: 30s\n",
        "period must be longer than duration",
    );
}

#[test]
fn failure_schedule_http_status_must_be_an_error_status() {
    assert_config_error(
        "failure_schedule:\n  duration: 30s\n  failure:\n    http_status: 200\n",
        "http_status must be between 400 and 599",
    );
}

#[test]
fn failure_schedule_in_an_override_is_validated() {
    assert_config_error(
        "subgraph_overrides:\n  products:\n    failure_schedule:\n      duration: 0s\n",
        "duration must be greater than zero",
    );
}
