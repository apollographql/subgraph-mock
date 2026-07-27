//! Shared GraphQL selection traversal.
//!
//! Response generation ([`crate::handle::graphql`]) and FTV1 tracing ([`crate::ftv1`]) both walk an
//! operation's selection set, and the trace tree only stays faithful to the emitted response if the
//! two agree on *which* operation runs and *how* its fields are grouped. Keeping both decisions here
//! — rather than in each module — means they cannot silently drift apart.

use apollo_compiler::executable::{Field, Operation, Selection, SelectionSet};
use apollo_compiler::{ExecutableDocument, Node};
use std::collections::HashMap;

/// The operation a request executes: the first operation defined in the document.
///
/// Both the response builder and the trace builder pick this same operation, so a request always
/// gets a trace describing the operation it actually ran.
pub fn primary_operation(doc: &ExecutableDocument) -> Option<&Node<Operation>> {
    doc.operations.iter().next()
}

/// Flattens a selection set into `(response_name, fields)` groups in query order, following fragment
/// spreads and inline fragments (their type conditions are ignored — extra fields are harmless).
/// Fields sharing a response key are merged into one group, so a field selected from several places
/// yields a single node whose sub-selections are the union of those selections.
pub fn collect_fields<'a>(
    doc: &'a ExecutableDocument,
    selection_set: &'a SelectionSet,
) -> Vec<(String, Vec<&'a Node<Field>>)> {
    let mut out = Vec::new();
    let mut index = HashMap::new();
    collect_into(doc, selection_set, &mut out, &mut index);
    out
}

fn collect_into<'a>(
    doc: &'a ExecutableDocument,
    selection_set: &'a SelectionSet,
    out: &mut Vec<(String, Vec<&'a Node<Field>>)>,
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
