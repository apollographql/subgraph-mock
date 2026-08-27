use serde_yaml::Value;
use subgraph_mock::state::Config;

fn parse_otel(yaml: &str) -> String {
    let value: Value = serde_yaml::from_str(yaml).expect("test fixture should be valid YAML");
    let (_, _, _, telemetry_config) = Config::parse_yaml(value).expect("expected config to parse");
    format!("{:?}", telemetry_config.otel)
}

fn parse_http(yaml: &str) -> String {
    let value: Value = serde_yaml::from_str(yaml).expect("test fixture should be valid YAML");
    let (_, _, _, telemetry_config) = Config::parse_yaml(value).expect("expected config to parse");
    format!("{:?}", telemetry_config.http)
}

#[test]
fn otel_is_disabled_when_entirely_absent() {
    // Upstream's own default still builds real (if exporterless) providers -- subgraph-mock's
    // own default is a true no-op instead, matching every other config section's "untouched
    // config, no behavior change" bar. See `disabled_open_telemetry`.
    let debug = parse_otel("{}\n");
    assert!(
        debug.contains("disabled: Some(true)"),
        "expected otel to default to disabled when telemetry.otel isn't mentioned at all, got: {debug}"
    );
}

#[test]
fn mentioning_otel_at_all_opts_out_of_disabled_by_default() {
    // Even an empty mapping counts as "the user opted in" -- disabled must not be forced on.
    let debug = parse_otel("telemetry:\n  otel: {}\n");
    assert!(
        !debug.contains("disabled: Some(true)"),
        "expected mentioning otel at all to skip the disabled-by-default resolution, got: {debug}"
    );
}

#[test]
fn mentioning_otel_at_all_does_not_smuggle_in_a_propagator_default() {
    // Unlike the old YAML-merge approach, resolve() doesn't reach inside a `Some(cfg)` to
    // default individual sub-fields -- `OpenTelemetryConfig`'s fields are private, there's
    // nothing to reach into. Once otel is mentioned at all, propagator falls back to
    // apollo_opentelemetry's own (empty) default unless the user sets it themselves.
    let debug = parse_otel("telemetry:\n  otel:\n    disabled: false\n");
    assert!(
        debug.contains("composite: []"),
        "expected the upstream (empty) propagator default with no smuggled-in tracecontext, got: {debug}"
    );
}

#[test]
fn explicit_disabled_false_is_respected() {
    let debug = parse_otel("telemetry:\n  otel:\n    disabled: false\n");
    assert!(
        debug.contains("disabled: Some(false)"),
        "expected the user's explicit disabled: false to be preserved, got: {debug}"
    );
}

#[test]
fn explicit_propagator_choice_is_respected() {
    let debug = parse_otel("telemetry:\n  otel:\n    propagator:\n      composite: [baggage]\n");
    assert!(
        debug.contains("Baggage"),
        "expected the user's explicit propagator choice to come through untouched, got: {debug}"
    );
}

#[test]
fn http_defaults_to_body_size_enabled_when_entirely_absent() {
    let debug = parse_http("{}\n");
    for needle in [
        "spans: SpanConfig { request_body_size: true",
        "response_body_size: true",
        "metrics: MetricsConfig { request_body_size: true",
    ] {
        assert!(
            debug.contains(needle),
            "expected body-size recording on by default, missing {needle:?} in: {debug}"
        );
    }
}

#[test]
fn mentioning_http_at_all_opts_out_of_the_body_size_default() {
    // Same principle as otel: presence, not per-field merging, decides. Once `http` is
    // mentioned at all, unset sub-fields fall back to apollo_http_server_telemetry's own
    // (body-size off) defaults, not subgraph-mock's.
    let debug = parse_http("telemetry:\n  http: {}\n");
    assert!(
        debug.contains("spans: SpanConfig { request_body_size: false, response_body_size: false"),
        "expected upstream's body-size-off default once http is mentioned at all, got: {debug}"
    );
}

#[test]
fn explicit_body_size_choice_is_respected() {
    let debug = parse_http(
        "telemetry:\n  http:\n    spans:\n      request_body_size: false\n      response_body_size: false\n",
    );
    assert!(
        debug.contains("spans: SpanConfig { request_body_size: false, response_body_size: false"),
        "expected the user's explicit false to come through untouched, got: {debug}"
    );
}

#[test]
fn otel_and_http_resolve_independently_under_one_telemetry_key() {
    let value: Value = serde_yaml::from_str(
        "telemetry:\n  otel:\n    disabled: false\n  http:\n    spans:\n      request_body_size: false\n",
    )
    .unwrap();
    let (_, _, _, telemetry_config) = Config::parse_yaml(value).expect("expected config to parse");

    let otel_debug = format!("{:?}", telemetry_config.otel);
    assert!(otel_debug.contains("disabled: Some(false)"));

    let http_debug = format!("{:?}", telemetry_config.http);
    assert!(http_debug.contains("request_body_size: false"));
    // response_body_size wasn't mentioned -- falls back to apollo_http_server_telemetry's own
    // default (false) too, since `http` being present at all opts out of subgraph-mock's
    // section-wide default.
    assert!(http_debug.contains("response_body_size: false"));
}
