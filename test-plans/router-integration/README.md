# router-integration

Validates that subgraph-mock's FTV1 traces (`SPEC_ftv1.md`) reach the router and are decoded
correctly, using mock-studio as a local stand-in for GraphOS ingest - a wiring/correctness check,
not a load test.

The scenario sends a single GraphQL request to the router, then queries mock-studio's
`GET /request-stats` and fails if:

- the `POST /v1/traces` stats entry is missing any field, or any field is non-numeric or negative
  (mock-studio's own decode/serialization is broken, independent of what was sent), or
- the router didn't send any OTLP trace spans to `/v1/traces` (`n_root_spans == 0`), or
- a decoded report has fewer total nodes than reports (`n_ftv1_nodes < n_ftv1_reports`) - every
  report has at least a root node, so this means mock-studio's own FTV1 walk under-counted, or
- any FTV1 anomaly counter (`n_ftv1_trace_parsing_failed`, `n_ftv1_bad_type_nodes`,
  `n_ftv1_timing_inversions`, `n_ftv1_index_nodes`, `n_ftv1_errors`) is nonzero, or
- no FTV1 trace was decoded at all (`n_ftv1_reports == 0`).

`field_level_instrumentation_sampler` is set to `always_on` in the router config, so a single
request is enough to make FTV1 arrival deterministic rather than sampled.

**`otlp_tracing_sampler` is deliberately `always_on`.** OTLP tracing and the legacy Report's
raw-trace embedding are mutually exclusive - whichever is "on" is where the router sends the
per-request trace (confirmed by inspecting the router's own submitted Report with OTLP off vs on:
with it on, `/studio`'s `traces_per_query` held only aggregated `stats_with_context`, with an empty
`traces` array, every time). This test plan validates FTV1 via mock-studio's `/v1/traces` (OTLP)
decode, so `experimental_otlp_endpoint` points there with the full path - the router doesn't append
`/v1/traces` on its own.

## Prerequisites

- `APOLLO_KEY` with access to the `starstuff` graph (Apollo's shared internal test graph). This is
  used only to generate an offline license - the router validates it locally and never contacts
  GraphOS at runtime. Without a real graph_ref + license, the router's telemetry.apollo pipeline
  (both `/studio` usage-reporting and OTLP tracing) never activates at all, regardless of
  `APOLLO_USAGE_REPORTING_INGRESS_URL` pointing at mock-studio - this was confirmed by observing
  zero reporting activity even at full router debug logging with a valid subgraph round-trip.
- `GITHUB_TOKEN` and `gcloud auth` access to pull the private mock-studio image.

## Running

Build a local subgraph-mock image tagged to match this test plan's `SUBGRAPH_IMAGE` default first:

```bash
docker build -t local-subgraph-mock .
APOLLO_KEY="<insert_token>" APOLLO_SUDO=true rtf run test-plans/router-integration/test-plan.yaml --var 'subgraph_image=local-subgraph-mock'
```
