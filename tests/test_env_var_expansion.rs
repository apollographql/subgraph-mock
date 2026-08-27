use serde_yaml::Value;
use subgraph_mock::state::Config;

fn parse_otel(yaml: &str) -> String {
    let value: Value = serde_yaml::from_str(yaml).expect("test fixture should be valid YAML");
    let (_, _, _, telemetry_config) = Config::parse_yaml(value).expect("expected config to parse");
    format!("{:?}", telemetry_config.otel)
}

#[test]
fn env_var_expansion_applies_to_otel_config() {
    let var = "SUBGRAPH_MOCK_TEST_ENV_EXPANSION_PROBE";
    // SAFETY: this test binary is single-threaded for env var purposes here - no other test in
    // this file reads this specific variable name, so a data race with concurrently-running
    // tests in this binary isn't a concern.
    unsafe { std::env::set_var(var, "http://example-collector:4317") };

    let debug = parse_otel(
        "telemetry:\n  otel:\n    tracer_provider:\n      processors:\n        - batch:\n            exporter:\n              otlp_grpc:\n                endpoint: \"${env.SUBGRAPH_MOCK_TEST_ENV_EXPANSION_PROBE}\"\n",
    );

    unsafe { std::env::remove_var(var) };

    // `endpoint` is a typed `Url`, not a plain String, so its Debug output is a broken-down
    // struct (`Domain("example-collector")`, `port: Some(4317)`) rather than a flat string -
    // checking both fields confirms the *value* was substituted before URL parsing ran, which
    // is only possible if expansion actually happened.
    assert!(
        debug.contains(r#"Domain("example-collector")"#) && debug.contains("port: Some(4317)"),
        "expected ${{env.VAR}} to expand to the environment variable's value, got: {debug}"
    );
}

#[test]
fn env_var_expansion_falls_back_to_default_when_unset() {
    let var = "SUBGRAPH_MOCK_TEST_ENV_EXPANSION_PROBE_UNSET";
    unsafe { std::env::remove_var(var) };

    let debug = parse_otel(
        "telemetry:\n  otel:\n    tracer_provider:\n      processors:\n        - batch:\n            exporter:\n              otlp_grpc:\n                endpoint: \"${env.SUBGRAPH_MOCK_TEST_ENV_EXPANSION_PROBE_UNSET:-http://localhost:4317}\"\n",
    );

    assert!(
        debug.contains(r#"Domain("localhost")"#) && debug.contains("port: Some(4317)"),
        "expected ${{env.VAR:-default}} to fall back to the default when unset, got: {debug}"
    );
}

#[test]
fn env_var_expansion_applies_to_a_non_telemetry_field() {
    let var = "SUBGRAPH_MOCK_TEST_ENV_EXPANSION_HEADER_PROBE";
    unsafe { std::env::set_var(var, "probe-value") };

    let value: Value = serde_yaml::from_str(
        "headers:\n  X-Probe: \"${env.SUBGRAPH_MOCK_TEST_ENV_EXPANSION_HEADER_PROBE}\"\n",
    )
    .expect("test fixture should be valid YAML");
    let (_, _, config, _) = Config::parse_yaml(value).expect("expected config to parse");

    unsafe { std::env::remove_var(var) };

    assert_eq!(
        config.headers.get("x-probe").and_then(|v| v.to_str().ok()),
        Some("probe-value"),
        "expected ${{env.VAR}} to expand in a non-telemetry field (headers) too, not just under telemetry:"
    );
}
