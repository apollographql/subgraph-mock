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

Real subgraphs return a per-field timing trace when the router requests one. This mock does the
same: when a request carries the `apollo-federation-include-trace: ftv1` header, the response
includes a base64-encoded protobuf `Trace` in its `extensions.ftv1` field. The router decodes these
to report field-level metrics to GraphOS, so this lets trace-dependent router features be exercised
end-to-end against the mock.

The node tree mirrors the query's selection set, with each node carrying its `response_name`,
`type`, and `parent_type` plus synthetic-but-plausible nested timing. One simplification: list
fields emit their sub-selection children directly rather than per-element `index` nodes, so the tree
stays aligned to the query but lacks per-element timing.

Emission can be forced on or off regardless of the header via `response_generation.ftv1` (which is
also subgraph-overridable): omit it to follow the header, set `true` to always emit, or `false` to
never emit. See `example-config.yaml` for details.

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
