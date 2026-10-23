use super::{COMMENT_NODE_KEY, INCOMPLETE, INDEX, RANGE, SEQUENCE, SNAPSHOT};
use crate::{IvyError, IvyJson as Json, IvyResult, JsString};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct NodePin {
    object_id: JsString,
    revision: usize,
}
impl NodePin {
    pub fn read(pin: &Json) -> IvyResult<Self> {
        let object_id = pin
            .get("objectId")
            .and_then(Json::as_js_string)
            .filter(|id| !id.0.is_empty())
            .ok_or_else(|| invalid(INDEX))?;
        let revision = count(pin.get("revision"))
            .filter(|revision| *revision > 0)
            .ok_or_else(|| invalid(INDEX))?;
        Ok(Self {
            object_id: object_id.clone(),
            revision,
        })
    }
}

pub(super) fn invalid(message: &str) -> IvyError {
    IvyError::new("internal_error", message)
}
fn count(value: Option<&Json>) -> Option<usize> {
    let value = value?.as_f64()?;
    (value.is_finite()
        && value >= 0.0
        && value <= 9_007_199_254_740_991.0
        && value < usize::MAX as f64
        && value.fract() == 0.0)
        .then_some(value as usize)
}
fn text(value: Option<&Json>, expected: &str) -> bool {
    value.and_then(Json::as_js_string) == Some(&JsString::from(expected))
}
pub(super) fn node_owner(node: &Json, task_id: &JsString) -> IvyResult<()> {
    if task_id.0.is_empty() || node.get("taskId").and_then(Json::as_js_string) != Some(task_id) {
        return Err(invalid(SNAPSHOT));
    }
    Ok(())
}
pub(super) fn snapshot<'a>(
    saved: &'a Json,
    pin: &NodePin,
    task_id: &JsString,
) -> IvyResult<&'a Json> {
    let object = saved.get("object");
    let revision = saved.get("revision");
    let content = saved.get("content");
    if !text(object.and_then(|v| v.get("contractKey")), COMMENT_NODE_KEY)
        || !["1.0.0", "2.0.0"]
            .iter()
            .any(|version| text(revision.and_then(|v| v.get("contractVersion")), version))
        || object
            .and_then(|v| v.get("parentId"))
            .and_then(Json::as_js_string)
            != Some(task_id)
        || object
            .and_then(|v| v.get("ownerObjectId"))
            .and_then(Json::as_js_string)
            != Some(task_id)
        || !text(content.and_then(|v| v.get("encoding")), "json")
        || count(revision.and_then(|v| v.get("revision"))) != Some(pin.revision)
    {
        return Err(invalid(SNAPSHOT));
    }
    let node = content
        .and_then(|v| v.get("value"))
        .ok_or_else(|| invalid(SNAPSHOT))?;
    node_owner(node, task_id)?;
    Ok(node)
}

pub(super) struct Cache<N> {
    pub nodes: HashMap<NodePin, (N, usize)>,
    order: VecDeque<NodePin>,
    bytes: usize,
}
impl<N> Default for Cache<N> {
    fn default() -> Self {
        Self {
            nodes: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }
}
impl<N> Cache<N> {
    pub fn insert(&mut self, key: NodePin, node: N, bytes: usize, maximum: usize) {
        if bytes > maximum || self.nodes.contains_key(&key) {
            return;
        }
        while self.bytes.saturating_add(bytes) > maximum {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((_, charged)) = self.nodes.remove(&oldest) {
                self.bytes -= charged;
            }
        }
        self.order.push_back(key.clone());
        self.nodes.insert(key, (node, bytes));
        self.bytes += bytes;
    }
}

pub(super) struct Item {
    index: Json,
    offset: usize,
    length: usize,
    depth: usize,
}
pub(super) struct RangeState {
    start: usize,
    end: usize,
    pending: VecDeque<Item>,
    result: Vec<Option<Json>>,
}
impl RangeState {
    pub fn new(tree: &Json, start: Option<usize>, end: Option<usize>) -> IvyResult<Self> {
        let length = count(tree.get("length")).ok_or_else(|| invalid(RANGE))?;
        let (start, end) = (start.unwrap_or(0), end.unwrap_or(length));
        if end > length || start > end {
            return Err(invalid(RANGE));
        }
        let root = tree.get("root").filter(|root| !root.is_null());
        if root
            .map(|root| count(root.get("length")))
            .unwrap_or(Some(0))
            != Some(length)
        {
            return Err(invalid(RANGE));
        }
        let mut state = Self {
            start,
            end,
            pending: VecDeque::new(),
            result: Vec::new(),
        };
        if let Some(root) = root {
            state.enqueue(root.clone(), 0, 0)?;
        }
        Ok(state)
    }
    fn enqueue(&mut self, index: Json, offset: usize, depth: usize) -> IvyResult<()> {
        let length = count(index.get("length")).ok_or_else(|| invalid(INDEX))?;
        let end = offset.checked_add(length).ok_or_else(|| invalid(INDEX))?;
        if offset < self.end && self.start < end {
            self.pending.push_back(Item {
                index,
                offset,
                length,
                depth,
            });
        }
        Ok(())
    }
    pub fn batch(&mut self) -> Vec<Item> {
        self.pending.drain(..self.pending.len().min(8)).collect()
    }
    pub fn pin(item: &Item) -> IvyResult<&Json> {
        if item.depth > 12 || item.length == 0 {
            return Err(invalid(INDEX));
        }
        let pin = item.index.get("object").ok_or_else(|| invalid(INDEX))?;
        NodePin::read(pin)?;
        Ok(pin)
    }
    pub fn complete(&mut self, item: Item, node: &Json) -> IvyResult<()> {
        if text(node.get("kind"), "comment") {
            if item.length != 1 {
                return Err(invalid(SEQUENCE));
            }
            let entry = node.get("entry").ok_or_else(|| invalid(SEQUENCE))?;
            let sequence = entry.get("comment").and_then(|v| count(v.get("sequence")));
            if sequence != item.offset.checked_add(1) {
                return Err(invalid(SEQUENCE));
            }
            let position = item.offset - self.start;
            if self.result.len() <= position {
                self.result.resize_with(position + 1, || None);
            }
            self.result[position] = Some(entry.clone());
        } else {
            let children = node
                .get("children")
                .and_then(Json::as_array)
                .filter(|children| !children.is_empty())
                .ok_or_else(|| invalid(INCOMPLETE))?;
            let mut total = 0usize;
            for child in children {
                let length = count(child.get("length"))
                    .filter(|n| *n > 0)
                    .ok_or_else(|| invalid(INCOMPLETE))?;
                total = total
                    .checked_add(length)
                    .ok_or_else(|| invalid(INCOMPLETE))?;
            }
            if total != item.length {
                return Err(invalid(INCOMPLETE));
            }
            let mut offset = item.offset;
            for child in children {
                let length = count(child.get("length")).ok_or_else(|| invalid(INCOMPLETE))?;
                self.enqueue(child.clone(), offset, item.depth + 1)?;
                offset = offset
                    .checked_add(length)
                    .ok_or_else(|| invalid(INCOMPLETE))?;
            }
        }
        Ok(())
    }
    pub fn finish(self) -> IvyResult<Vec<Json>> {
        if self.result.len() != self.end - self.start {
            return Err(invalid(INCOMPLETE));
        }
        self.result
            .into_iter()
            .map(|entry| entry.ok_or_else(|| invalid(INCOMPLETE)))
            .collect()
    }
}
