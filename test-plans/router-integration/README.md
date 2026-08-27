# router-integration

Validates that subgraph-mock's FTV1 traces reach the router and are decoded correctly under a modest
volume of concurrent traffic, using mock-studio as a local stand-in for GraphOS ingest.

Before generating any load, the scenario also polls the router's and subgraph-mock's own health
endpoints directly (`scripts/verify-ftv1.js`'s `setup()`) and fails the run if either doesn't come
up healthy in time - see "Health checking the router and subgraph-mock" below.

The scenario runs a short k6 load (`scripts/verify-ftv1.js`, driven by this test plan's own
hand-authored `data/canned-ops.json` - see below) against the router, then queries mock-studio's
`GET /request-stats` and fails if:

- the `POST /v1/traces` stats entry is missing any field, or any field is non-numeric or negative
  (mock-studio's own decode/serialization is broken, independent of what was sent), or
- the router didn't send any OTLP trace spans to `/v1/traces` (`n_root_spans == 0`), or
- a decoded report has fewer total nodes than reports (`n_ftv1_nodes < n_ftv1_reports`) - every
  report has at least a root node, so this means mock-studio's own FTV1 walk under-counted, or
- any FTV1 anomaly counter (`n_ftv1_trace_parsing_failed`, `n_ftv1_bad_type_nodes`,
  `n_ftv1_timing_inversions`, `n_ftv1_index_nodes`, `n_ftv1_errors`) is nonzero, or
- no FTV1 trace was decoded at all (`n_ftv1_reports == 0`).

`field_level_instrumentation_sampler` is set to `always_on` in the router config, so every request
in the run is expected to produce a decodable FTV1 report, not just a sampled subset.

**Canned ops are hand-authored, not pulled via `graphos_canned_ops`.** The supergraph here is
subgraph-mock's own synthetic `users`/`posts` schema, served entirely by subgraph-mock itself - no
real GraphOS graph shares it. Pulling real historical operations from a live `graph_ref` would send
queries that fail router-side validation before ever reaching the subgraph, producing no FTV1 traces
at all. `data/canned-ops.json` instead contains one query written against this repo's own schema -
the same multi-fetch `users { ... posts { ... } }` query the single-request version of this test
used, kept because `bio` (owned by `users`) `@requires` `posts.content` (owned by `posts`), forcing
a multi-fetch query plan rather than a bare fetch.

**Verification runs inside k6/JS, not curl+jq.** `scripts/verify-ftv1.sh` is a thin wrapper that
runs `scripts/verify-ftv1.js` (load generation + the `/request-stats` assertions above, in one k6
process) and saves a copy of mock-studio's final stats as a run artifact. The assertions live in JS
rather than a shell script because the `grafana/k6` image runs as a non-root user with no `apk`
access and has no `curl`/`jq` preinstalled (only busybox `wget`) - confirmed by hand, not assumed;
see the comments at the top of both scripts for the full reasoning.

**`otlp_tracing_sampler` is deliberately `always_on`.** OTLP tracing and the legacy Report's
raw-trace embedding are mutually exclusive - whichever is "on" is where the router sends the
per-request trace (confirmed by inspecting the router's own submitted Report with OTLP off vs on:
with it on, `/studio`'s `traces_per_query` held only aggregated `stats_with_context`, with an empty
`traces` array, every time). This test plan validates FTV1 via mock-studio's `/v1/traces` (OTLP)
decode, so `experimental_otlp_endpoint` points there with the full path - the router doesn't append
`/v1/traces` on its own.

## Health checking the router and subgraph-mock

`scripts/verify-ftv1.js`'s `setup()` polls both the router's and subgraph-mock's health endpoints
directly before generating any load, and hard-fails the whole run if either doesn't come up healthy
in time - this test plan fully controls the environment, so an unready router or subgraph-mock at
that point always means a real regression, not external flakiness. There's no docker-compose-level
healthcheck sidecar for either anymore (there used to be, for local `rtf run` only) - it did nothing
under the RTF Orchestrator, so a k6-based check that works the same way in both places replaced it.

- **subgraph-mock** is polled at its own single combined `/health` endpoint (subgraph-mock's own
  `apollo-healthcheck`-backed endpoint - see `example-config.yaml`'s `health` section), via
  `SUBGRAPH_HEALTH_URL`.
- **The router** is polled at `health_check_url` (this test plan's own `variables:` block, matching
  the `orchestrator-k6-graphql` scenario's own variable of the same name - default `""`, which skips
  the check entirely, since not every pulled scenario version defines it and not every target
  exposes one).

Both checks poll with a timeout rather than checking once (60s timeout, 2s interval), since a
single-shot check can't tell "still starting up" apart from "actually down". This mirrors
`health_check_url`'s own upstream implementation
(`lib/custom-providers/k6-graphql/data/graphql-client.js`'s `waitForHealthy`, called from the
upstream scenario's own k6 entry point, `graphql-test.js`) rather than inventing different retry
semantics for the same kind of check - it has to be reimplemented here, rather than simply relied
on, because this test plan's `docker.command`/`K6_TEST_ENTRY` overrides replace that entry point
entirely with `scripts/verify-ftv1.js`, so `graphql-test.js` (and its call to `waitForHealthy`)
never runs. The _value_ of `health_check_url` still reaches `scripts/verify-ftv1.js` regardless
(baked into `$K6_CONFIG_FILE` by the `k6-graphql` custom_provider's own generation step, which our
overrides don't touch) - only the code that reads and acts on it had to move.

## OTel metrics reaching Prometheus (orchestrator only)

Separately from FTV1 (above), subgraph-mock also emits general OTel HTTP metrics
(`http.server.request.duration`, etc. - see subgraph-mock's own `example-config.yaml`), and this
test plan's environment (`data/docker-compose.yaml`'s `subgraph` service, `rtf.io/otel: true`) is
set up to forward them to an OTel Collector and on to Prometheus, checked via `environment.yaml`'s
`output_collection.prometheus` block.

This only produces data when the test plan runs via the RTF Orchestrator (`rtf remote run`), not
plain `rtf run` - the label and the `RTF_OTEL_COLLECTOR_GRPC` env var it injects only exist under
the Orchestrator. Under `rtf run`, `data/subgraph-config.yaml`'s exporter endpoint falls back to a
local address nothing is listening on: harmless (confirmed by hand - an unreachable collector
doesn't crash subgraph-mock), but no metrics actually go anywhere. `output_collection` itself is
observational only - it reports whether the metrics arrived, it doesn't fail the run if they don't.

## Prerequisites

- `APOLLO_KEY` with access to the `starstuff` graph (Apollo's shared internal test graph). This is
  used only to generate an offline license - the router validates it locally and never contacts
  GraphOS at runtime. Without a real graph_ref + license, the router's telemetry.apollo pipeline
  (both `/studio` usage-reporting and OTLP tracing) never activates at all, regardless of
  `APOLLO_USAGE_REPORTING_INGRESS_URL` pointing at mock-studio - this was confirmed by observing
  zero reporting activity even at full router debug logging with a valid subgraph round-trip.
- `GITHUB_TOKEN` to pull the `orchestrator-k6-graphql` scenario config from the `rtf-morgue` repo.
- `gcloud auth` access to pull the private mock-studio image.

## Running

Build a local subgraph-mock image tagged to match this test plan's `SUBGRAPH_IMAGE` default first:

```bash
docker build -t local-subgraph-mock .
APOLLO_KEY="<insert_token>" APOLLO_SUDO=true GITHUB_TOKEN="<insert_token>" rtf run test-plans/router-integration/test-plan.yaml --var 'subgraph_image=local-subgraph-mock'
```

Use `rtf remote run` instead of `rtf run` when the OTel metrics/Prometheus check above matters -
otherwise it's silently a no-op, per the section above.
