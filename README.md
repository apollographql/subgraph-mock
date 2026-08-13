# Subgraph Mock

A minimal, configurable subgraph mock. See `example-config.yaml` for documentation of the available
configuration options.

### Example usage

```bash
$ subgraph-mock --config example-config.yaml --schema my-schema.graphql
```

### Limitations

This is a minimal mock server designed for use in testing/development scenarios where a real GraphQL
server is needed to respond to queries. It is not a fully spec-compliant GraphQL server.

It does not support:

- subscriptions
- mutations
- mixed queries with both introspection and concrete fields

### Configuration files

Any string value anywhere in the config file can reference an environment variable with
`${env.VAR_NAME}`, optionally with a default for when it's unset: `${env.VAR_NAME:-fallback}`. A
referenced variable with no default that's unset at parse time is an error. See
`example-config.yaml` for a live example.

### Features

This mock server is mainly designed to act as multiple subgraphs behind a federated supergraph. It
will respond to correct queries with randomly generated data as specified by the configuration
provided. Invalid queries will be rejected with their validation errors included in the response.

Introspection-only queries will be responded to with correct data, not random data. Mixed queries
with both introspection and concrete data will be populated entirely with random data.

#### Federation

This mock server has partial Federation v2 support. It can understand and parse subgraph schemas
that use the built-in Federation v2 directives. It does not currently do any actual resolution of
the `@link` directive, so any imports or renames as specified in that directive will not work.

#### Federated Tracing (FTV1)

This mock server can emit per-field federated tracing (FTV1) data, the mechanism real subgraphs use
to report field-level timing to GraphOS through the router. When a request carries the
`apollo-federation-include-trace: ftv1` header, the response includes a base64-encoded protobuf
trace in `extensions.ftv1`, which the router decodes and stitches into its query plan.

The router only sends this header on a sampled fraction of subgraph requests (3% by default, per
`field_level_instrumentation_sampler`), so `response_generation.ftv1` lets you force emission on or
off regardless of the header: `true` always emits a trace, `false` never does, and omitting it (the
default) follows the header. This can be set per-subgraph via `subgraph_overrides`. See
`example-config.yaml` for details.

Traces are approximate rather than exact reproductions of a real subgraph's timing:

- List fields are flattened: their sub-selections appear directly as children rather than through
  the per-element `index` nodes real traces use, so per-element timing isn't represented.
- Timing is synthetic — spans nest and stay ordered, but `duration_ns` bears no relation to the
  request's real duration or to any configured latency injection.
- Interface and union (abstract-typed) fragments are pooled across a list's elements rather than
  resolved per element: the trace's field set is pruned to match what the response actually
  generated (a field never present in any element won't appear), but a field present on some
  elements and not others still shows up once, with no way to say which element(s) had it.

None of these prevent the router from decoding, redacting, stitching, or reporting the trace — they
only affect timing and field-usage fidelity for abstract-typed queries.

#### OpenTelemetry

This mock server can export OpenTelemetry traces and metrics. Both are disabled by default — see
`example-config.yaml`'s `telemetry:` section for how to point them at a collector (or `console:` for
local debugging) and turn on HTTP-layer spans/metrics. The metrics below exist as instruments
regardless, but nothing is actually collected anywhere until `telemetry.otel` is configured with an
enabled meter provider.

**HTTP-layer metrics**

Emitted for every request by `apollo-http-server-telemetry`, following the
[OpenTelemetry HTTP semantic conventions](https://opentelemetry.io/docs/specs/semconv/http/http-metrics/).
See that crate's own docs for the full attribute set attached to each.

| Metric                           | Type          | Unit | Notes                                                                                            |
| -------------------------------- | ------------- | ---- | ------------------------------------------------------------------------------------------------ |
| `http.server.request.duration`   | Histogram     | `s`  | Always recorded.                                                                                 |
| `http.server.active_requests`    | UpDownCounter | -    | Always recorded.                                                                                 |
| `http.server.request.body.size`  | Histogram     | `By` | Opt-in via `telemetry.http.metrics.request_body_size` (on by default in `example-config.yaml`).  |
| `http.server.response.body.size` | Histogram     | `By` | Opt-in via `telemetry.http.metrics.response_body_size` (on by default in `example-config.yaml`). |

**subgraph-mock metrics**

Emitted directly by this server:

| Metric                                       | Type      | Unit | Attributes                                                                                     | Description                                                                                                                                                                                                      |
| -------------------------------------------- | --------- | ---- | ---------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `subgraph_mock.response_generation.duration` | Histogram | `s`  | `subgraph.name` (if known)                                                                     | Time spent parsing, validating, and building a response body. Excludes cache hits, injected latency, and HTTP-layer overhead.                                                                                    |
| `subgraph_mock.response_cache.lookups`       | Counter   | -    | `cache.result` (`hit`/`miss`), `subgraph.name` (if known)                                      | Count of response-cache lookups for `cache_responses`-enabled subgraphs (on by default). A miss paid the cost recorded by `subgraph_mock.response_generation.duration`; a hit didn't.                            |
| `subgraph_mock.cache.size`                   | Gauge     | -    | `cache` (`parse_and_validate`, `cached_trace_shape`, or `into_response_bytes_and_status_code`) | Current entry count of subgraph-mock's internal caches, sampled every 30 seconds. None of them ever evict, so steady growth over a run's lifetime signals unbounded memory use rather than expected cache reuse. |

The OTel SDK's default histogram bucket boundaries are calibrated for millisecond-scale values, not
the seconds these two duration histograms record in — see the `views` example in
`example-config.yaml` for why that matters and the boundaries to use instead.

#### Subgraph Overrides

If your test scenario calls for behavioral differences between subgraphs, the mock server will
respond using those subgraphs' specific configurations to requests made at `/<subgraph name>`
instead of at `/`. See `example-config.yaml` for details on how to specify these overrides.

If the server is started with a federated supergraph schema, it will not infer subgraph-specific
schemas for any requests to the subgraph-overridden endpoints. The subgraph endpoints only inherit
behavioral differences, and still operate under the full provided schema for all validation and
introspection purposes.

#### Non-federated Usage

This mock server can also be used as a standalone GraphQL mock server without any federation
behavior. Just provide a standard schema file and configuration without subgraph overrides and it
will respond to valid queries for that schema.
