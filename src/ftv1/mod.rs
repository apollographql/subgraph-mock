//! Federated Tracing v1 (FTV1) support.
//!
//! Real federation subgraphs return a per-field timing trace whenever the router sends the
//! `apollo-federation-include-trace: ftv1` request header. The trace is a base64-encoded protobuf
//! [`Trace`] message placed in the GraphQL response's `extensions.ftv1` field, which the router
//! decodes and stitches into its query plan to report field-level metrics to GraphOS.
//!
//! This module hand-rolls the wire-compatible subset of Apollo's `reports.proto` `Trace` message
//! that the router needs, so the crate can emit realistic traces without a protobuf toolchain
//! (`prost` handles encoding via `#[derive(prost::Message)]`; there is no `.proto` file or
//! `build.rs`). Only [`Trace`] and [`Node`] are Apollo-proprietary; timestamps reuse the standard
//! [`prost_types::Timestamp`] well-known type.
//!
//! ## List-node simplification
//!
//! Apollo's real traces represent list fields as a parent node whose children are per-element
//! `index` nodes, each carrying the element's sub-selection. To keep the tree aligned to the query
//! we skip the `index` layer: a list field emits its sub-selection children directly. The tree
//! still decodes and stitches correctly; it simply lacks per-element timing granularity.

use crate::selection;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable::{Field, Operation, SelectionSet};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use prost::Message as _;
use std::time::{Duration, SystemTime};

/// Synthetic per-node processing cost used to advance the trace clock so timing offsets nest and
/// stay ordered.
const PER_NODE_COST_NS: u64 = 1_000;

/// Wire-compatible subset of Apollo's `reports.proto` `Trace` message.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Trace {
    /// Wall-clock time at which the operation started executing.
    #[prost(message, optional, tag = "4")]
    pub start_time: Option<prost_types::Timestamp>,
    /// Wall-clock time at which the operation finished executing.
    #[prost(message, optional, tag = "3")]
    pub end_time: Option<prost_types::Timestamp>,
    /// Total operation duration in nanoseconds.
    #[prost(uint64, tag = "11")]
    pub duration_ns: u64,
    /// Root of the field-timing tree. Its children are the top-level fields of the operation.
    #[prost(message, optional, tag = "14")]
    pub root: Option<Node>,
}

/// A single field in the trace tree.
///
/// `response_name` is modelled as a plain `string` (tag 1), which is wire-identical to the first
/// arm of the real message's `oneof id`. We never populate the alternative `index` arm, so list
/// nodes carry their sub-selection children directly (see the module docs).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Node {
    /// The response key (alias if present, otherwise the field name).
    #[prost(string, tag = "1")]
    pub response_name: String,
    /// The field's GraphQL type, including any list/non-null wrappers (e.g. `[Post!]!`).
    #[prost(string, tag = "3")]
    pub r#type: String,
    /// The name of the type on which this field was selected.
    #[prost(string, tag = "13")]
    pub parent_type: String,
    /// Start offset in nanoseconds, relative to [`Trace::start_time`].
    #[prost(uint64, tag = "8")]
    pub start_time: u64,
    /// End offset in nanoseconds, relative to [`Trace::start_time`].
    #[prost(uint64, tag = "9")]
    pub end_time: u64,
    /// Child field nodes selected under this field.
    #[prost(message, repeated, tag = "12")]
    pub child: Vec<Node>,
}

/// Builds a [`Trace`] whose node tree mirrors the operation's selection set.
///
/// Timing is synthetic: a running nanosecond clock assigns each node a `start_time`, recurses into
/// its children (advancing the clock), then adds a fixed per-node cost before recording `end_time`,
/// so child spans nest within their parent and siblings stay ordered.
pub fn build_trace(op: &Operation, doc: &ExecutableDocument) -> Trace {
    let start = SystemTime::now();

    let mut builder = TraceBuilder { doc, clock: 0 };
    let child = builder.nodes(&op.selection_set);
    let duration_ns = builder.clock;

    let root = Node {
        response_name: String::new(),
        r#type: String::new(),
        parent_type: String::new(),
        start_time: 0,
        end_time: duration_ns,
        child,
    };

    Trace {
        start_time: Some(start.into()),
        end_time: Some((start + Duration::from_nanos(duration_ns)).into()),
        duration_ns,
        root: Some(root),
    }
}

/// Encodes a [`Trace`] to the base64 (standard, padded) protobuf representation the router expects.
pub fn encode(trace: &Trace) -> String {
    let mut buf = Vec::with_capacity(trace.encoded_len());
    trace
        .encode(&mut buf)
        .expect("encoding into a Vec never fails");
    STANDARD.encode(buf)
}

/// Decodes a base64 protobuf [`Trace`]. Exposed so integration tests can round-trip a trace without
/// depending on `prost` or `base64` directly.
pub fn decode(encoded: &str) -> anyhow::Result<Trace> {
    let bytes = STANDARD.decode(encoded)?;
    Trace::decode(bytes.as_slice()).map_err(Into::into)
}

struct TraceBuilder<'a> {
    doc: &'a ExecutableDocument,
    clock: u64,
}

impl TraceBuilder<'_> {
    fn nodes(&mut self, selection_set: &SelectionSet) -> Vec<Node> {
        let grouped = selection::collect_fields(self.doc, selection_set);

        let mut nodes = Vec::with_capacity(grouped.len());
        for (response_name, fields) in grouped {
            let meta = fields[0];
            if meta.name == "__typename" {
                continue;
            }

            let start_time = self.clock;

            let child = if meta.selection_set.selections.is_empty() {
                Vec::new()
            } else {
                self.nodes(&merged_child_selections(&fields))
            };

            self.clock += PER_NODE_COST_NS;

            nodes.push(Node {
                response_name,
                r#type: meta.ty().to_string(),
                parent_type: selection_set.ty.to_string(),
                start_time,
                end_time: self.clock,
                child,
            });
        }

        nodes
    }
}

/// Combines the sub-selections of every field sharing a response key into one selection set, so a
/// field selected from several places (e.g. across fragments) is traced as a single node whose
/// children are the union of those selections.
fn merged_child_selections(fields: &[&apollo_compiler::Node<Field>]) -> SelectionSet {
    let mut selections = Vec::new();
    for field in fields {
        selections.extend_from_slice(&field.selection_set.selections);
    }
    SelectionSet {
        ty: fields[0].selection_set.ty.clone(),
        selections,
    }
}
