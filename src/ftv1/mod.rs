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

use apollo_compiler::{
    ExecutableDocument,
    executable::{Field, Operation, Selection, SelectionSet},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use prost::Message as _;
use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

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

impl Trace {
    /// Builds a [`Trace`] whose node tree mirrors the operation's selection set.
    ///
    /// Timing is synthetic: a running nanosecond clock assigns each node a `start_time`, recurses
    /// into its children (advancing the clock), then adds a fixed per-node cost before recording
    /// `end_time`, so child spans nest within their parent and siblings stay ordered.
    pub fn build(op: &Operation, doc: &ExecutableDocument) -> Trace {
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

/// Encodes a [`Trace`] to the base64 (standard, padded) protobuf representation the router expects.
pub fn encode_trace(trace: &Trace) -> String {
    let mut buf = Vec::with_capacity(trace.encoded_len());
    trace
        .encode(&mut buf)
        .expect("encoding into a Vec never fails");

    STANDARD.encode(buf)
}

/// Decodes a base64 protobuf [`Trace`]. Exposed so integration tests can round-trip a trace without
/// depending on `prost` or `base64` directly.
pub fn decode_trace(encoded: &str) -> anyhow::Result<Trace> {
    let bytes = STANDARD.decode(encoded)?;

    Trace::decode(bytes.as_slice()).map_err(Into::into)
}

struct TraceBuilder<'a> {
    doc: &'a ExecutableDocument,
    clock: u64,
}

impl TraceBuilder<'_> {
    fn nodes(&mut self, selection_set: &SelectionSet) -> Vec<Node> {
        let grouped = collect_fields(self.doc, selection_set);

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

/// Flattens a selection set into `(response_name, fields)` groups in query order, following fragment
/// spreads and inline fragments (their type conditions are ignored — extra fields are harmless).
/// Fields sharing a response key are merged into one group, so a field selected from several places
/// yields a single node whose sub-selections are the union of those selections.
///
/// This is FTV1's own traversal, independent of response generation: `apollo-smith` 0.16's
/// `ResponseBuilder` resolves each abstract-typed field to a concrete type (via an RNG draw)
/// *before* collecting fields, so its traversal knows which fragment type conditions actually
/// matched for a given response element. This function makes a type-condition-blind pass instead,
/// so the two traversals can diverge under interface/union fragments — the trace may include fields
/// a given response element never had. That divergence is an accepted approximation (see
/// `SPEC_ftv1.md`'s "Known approximations"), not a bug to fix here.
fn collect_fields<'a>(
    doc: &'a ExecutableDocument,
    selection_set: &'a SelectionSet,
) -> Vec<(String, Vec<&'a apollo_compiler::Node<Field>>)> {
    let mut out = Vec::new();
    let mut index = HashMap::new();
    collect_into(doc, selection_set, &mut out, &mut index);

    out
}

fn collect_into<'a>(
    doc: &'a ExecutableDocument,
    selection_set: &'a SelectionSet,
    out: &mut Vec<(String, Vec<&'a apollo_compiler::Node<Field>>)>,
    index: &mut HashMap<String, usize>,
) {
    for selection in &selection_set.selections {
        match selection {
            Selection::Field(field) => {
                let key = field.alias.as_ref().unwrap_or(&field.name).to_string();
                match index.get(&key) {
                    Some(&i) => out[i].1.push(field),
                    None => {
                        index.insert(key.clone(), out.len());
                        out.push((key, vec![field]));
                    }
                }
            }
            Selection::FragmentSpread(fragment) => {
                if let Some(definition) = doc.fragments.get(&fragment.fragment_name) {
                    collect_into(doc, &definition.selection_set, out, index);
                }
            }
            Selection::InlineFragment(inline_fragment) => {
                collect_into(doc, &inline_fragment.selection_set, out, index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FederatedSchema;
    use apollo_compiler::validation::Valid;

    fn parse(query: &str) -> Valid<ExecutableDocument> {
        let supergraph = include_str!("../../tests/data/schema.graphql");
        let schema = FederatedSchema::parse_string(supergraph, "../../tests/data/schema.graphql")
            .expect("test schema should parse");

        ExecutableDocument::parse_and_validate(&schema, query, "query.graphql")
            .expect("test query should validate")
    }

    #[test]
    fn list_fields_flatten_without_index_nodes() {
        let doc = parse(r#"{ posts { title views } }"#);
        let op = doc.operations.iter().next().unwrap();
        let trace = Trace::build(op, &doc);

        let root = trace.root.expect("trace should have a root node");
        assert_eq!(root.child.len(), 1);

        let posts = &root.child[0];
        assert_eq!(posts.response_name, "posts");
        assert_eq!(posts.r#type, "[Post!]!");
        assert_eq!(posts.parent_type, "Query");

        // The list field's sub-selection appears directly as its children, with no per-element
        // "index" layer between `posts` and `title`/`views` — see the module docs.
        assert_eq!(posts.child.len(), 2);
        assert!(posts.child.iter().any(|node| node.response_name == "title"));
        assert!(posts.child.iter().any(|node| node.response_name == "views"));
        for child in &posts.child {
            assert_eq!(child.parent_type, "Post");
        }
    }

    #[test]
    fn collect_fields_merges_fields_sharing_a_response_key() {
        let doc = parse(
            r#"
            query {
              ...A
              ...B
            }
            fragment A on Query { posts { title } }
            fragment B on Query { posts { views } }
            "#,
        );
        let op = doc.operations.iter().next().unwrap();
        let grouped = collect_fields(&doc, &op.selection_set);

        let (_, posts_fields) = grouped
            .iter()
            .find(|(name, _)| name == "posts")
            .expect("expected a single merged `posts` group");
        assert_eq!(
            posts_fields.len(),
            2,
            "the two fragment-spread occurrences of `posts` should merge into one group"
        );

        let merged = merged_child_selections(posts_fields);
        let names: Vec<&str> = merged
            .selections
            .iter()
            .filter_map(|selection| match selection {
                Selection::Field(field) => Some(field.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"title"));
        assert!(names.contains(&"views"));
    }

    #[test]
    fn collect_fields_follows_fragment_spreads_and_inline_fragments() {
        let doc = parse(
            r#"
            query {
              user(id: "1") {
                __typename
                ...Contact
                ... on User { distance }
              }
            }
            fragment Contact on User { email }
            "#,
        );
        let op = doc.operations.iter().next().unwrap();
        let (_, user_fields) = collect_fields(&doc, &op.selection_set)
            .into_iter()
            .find(|(name, _)| name == "user")
            .expect("expected a `user` group");

        let user_selection_set = merged_child_selections(&user_fields);
        let names: Vec<String> = collect_fields(&doc, &user_selection_set)
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert_eq!(names, vec!["__typename", "email", "distance"]);
    }

    #[test]
    fn trace_build_skips_typename() {
        let doc = parse(r#"{ user(id: "1") { __typename name } }"#);
        let op = doc.operations.iter().next().unwrap();
        let trace = Trace::build(op, &doc);

        let root = trace.root.expect("trace should have a root node");
        let user = &root.child[0];

        assert_eq!(
            user.child.len(),
            1,
            "`__typename` should be skipped, leaving only `name`"
        );
        assert_eq!(user.child[0].response_name, "name");
    }

    #[test]
    fn trace_build_timing_nests_and_orders_siblings() {
        let doc = parse(r#"{ user(id: "1") { name address { city } } }"#);
        let op = doc.operations.iter().next().unwrap();
        let trace = Trace::build(op, &doc);

        let root = trace.root.expect("trace should have a root node");
        assert_eq!(
            trace.duration_ns, root.end_time,
            "duration_ns should equal the root's own end_time"
        );

        fn assert_nested(node: &Node) {
            assert!(node.start_time <= node.end_time);
            let mut cursor = node.start_time;
            for child in &node.child {
                assert!(
                    child.start_time >= cursor && child.end_time <= node.end_time,
                    "child `{}` span escapes parent `{}`",
                    child.response_name,
                    node.response_name
                );
                cursor = child.end_time;
                assert_nested(child);
            }
        }
        assert_nested(&root);
    }
}
