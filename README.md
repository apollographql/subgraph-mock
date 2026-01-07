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

#### Subgraph Overrides

If your test scenario calls for behavioral differences between subgraphs, the mock server will
respond using those subgraphs' specific configurations to requests made at `/<subgraph name>`
instead of at `/`. See `example-config.yaml` for details on how to specify these overrides.

#### Non-federated Usage

This mock server can also be used as a standalone GraphQL mock server without any federation
behavior. Just provide a standard schema file and configuration without subgraph overrides and it
will respond to valid queries for that schema.
