//! Selected compiler branches and reference targets, before runtime evaluation.
use super::{CompilerNode, CompilerResources, CompilerTarget, compiler_walk, uri};
use crate::{IvyError, IvyJson, IvyResult, JsString};
use std::collections::HashMap;

pub struct CompilerUnit<'a> {
    pub target: CompilerTarget<'a>,
    /// Inline branches relative to this validation root.
    pub nodes: Vec<CompilerNode<'a>>,
}
pub struct CompilerReference {
    pub from_unit: usize,
    pub from_node: usize,
    pub to_unit: usize,
}
pub struct CompilerPlan<'a> {
    /// Unit zero is the submitted root. Boolean targets are retained as units
    /// for evaluation even though the original compiler inlines their constants.
    pub units: Vec<CompilerUnit<'a>>,
    pub references: Vec<CompilerReference>,
}

fn identity(target: &CompilerTarget<'_>) -> (usize, usize, JsString) {
    (
        target.schema as *const _ as usize,
        target.root_schema as *const _ as usize,
        target.base_id.clone(),
    )
}
fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}

pub(super) fn plan(schema: &IvyJson) -> IvyResult<CompilerPlan<'_>> {
    let resources = CompilerResources::new(schema)?;
    let root = resources.submitted_root();
    let mut known = HashMap::from([(identity(&root), 0)]);
    let mut units = vec![CompilerUnit {
        target: root,
        nodes: Vec::new(),
    }];
    let mut pending = vec![0];
    let mut references = Vec::new();
    while let Some(index) = pending.pop() {
        let target = units[index].target.clone();
        let scope = if target.base_id.0.is_empty() {
            uri::full_path(&target.root_base_id)?
        } else {
            target.base_id.clone()
        };
        let walk = compiler_walk::walk_scoped(target.schema, target.base_id.clone(), scope)?;
        let mut children = Vec::new();
        for (node_index, node) in walk.nodes.iter().enumerate() {
            let Some(reference) = node.reference else {
                continue;
            };
            let next = if [JsString::from("#"), JsString::from("#/")].contains(reference)
                && node.base_id == target.root_base_id
            {
                target.root()
            } else {
                resources
                    .resolve_from(&target, &node.base_id, reference)?
                    .ok_or_else(invalid)?
            };
            let key = identity(&next);
            let to_unit = if let Some(index) = known.get(&key) {
                *index
            } else {
                let index = units.len();
                known.insert(key, index);
                units.push(CompilerUnit {
                    target: next,
                    nodes: Vec::new(),
                });
                children.push(index);
                index
            };
            references.push(CompilerReference {
                from_unit: index,
                from_node: node_index,
                to_unit,
            });
        }
        units[index].nodes = walk.nodes;
        pending.extend(children.into_iter().rev());
    }
    Ok(CompilerPlan { units, references })
}
