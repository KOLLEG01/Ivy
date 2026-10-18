//! Stack-independent operations for values returned by ordinary JSON parsing.
use super::{IvyJson, JsString};
use std::fmt;

impl Drop for IvyJson {
    fn drop(&mut self) {
        fn children(value: &mut IvyJson, pending: &mut Vec<IvyJson>) {
            match value {
                IvyJson::Array(values) => pending.append(values),
                IvyJson::Object(pairs) => pending.extend(pairs.drain(..).map(|(_, value)| value)),
                _ => {}
            }
        }
        let mut pending = Vec::new();
        children(self, &mut pending);
        while let Some(mut value) = pending.pop() {
            children(&mut value, &mut pending);
            // Its containers are empty, so the nested Drop cannot recurse.
        }
    }
}

impl Clone for IvyJson {
    fn clone(&self) -> Self {
        enum Part<'a> {
            Value(&'a IvyJson),
            Array(usize),
            Object(&'a [(JsString, IvyJson)]),
        }
        let mut pending = vec![Part::Value(self)];
        let mut ready = Vec::new();
        while let Some(part) = pending.pop() {
            match part {
                Part::Array(count) => {
                    let children = ready.split_off(ready.len() - count);
                    ready.push(Self::Array(children));
                }
                Part::Object(pairs) => {
                    let children = ready.split_off(ready.len() - pairs.len());
                    ready.push(Self::Object(
                        pairs
                            .iter()
                            .map(|(key, _)| key.clone())
                            .zip(children)
                            .collect(),
                    ));
                }
                Part::Value(value) => match value {
                    Self::Null => ready.push(Self::Null),
                    Self::Bool(value) => ready.push(Self::Bool(*value)),
                    Self::Number(value) => ready.push(Self::Number(*value)),
                    Self::String(value) => ready.push(Self::String(value.clone())),
                    Self::Array(values) => {
                        pending.push(Part::Array(values.len()));
                        pending.extend(values.iter().rev().map(Part::Value));
                    }
                    Self::Object(pairs) => {
                        pending.push(Part::Object(pairs));
                        pending.extend(pairs.iter().rev().map(|(_, value)| Part::Value(value)));
                    }
                },
            }
        }
        ready.pop().unwrap()
    }
}

impl PartialEq for IvyJson {
    fn eq(&self, other: &Self) -> bool {
        let mut pending = vec![(self, other)];
        while let Some((left, right)) = pending.pop() {
            match (left, right) {
                (Self::Null, Self::Null) => {}
                (Self::Bool(a), Self::Bool(b)) if a == b => {}
                (Self::Number(a), Self::Number(b)) if a == b => {}
                (Self::String(a), Self::String(b)) if a == b => {}
                (Self::Array(a), Self::Array(b)) if a.len() == b.len() => {
                    pending.extend(a.iter().zip(b))
                }
                (Self::Object(a), Self::Object(b)) if a.len() == b.len() => {
                    for ((key_a, value_a), (key_b, value_b)) in a.iter().zip(b) {
                        if key_a != key_b {
                            return false;
                        }
                        pending.push((value_a, value_b));
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

// Keep the derived Debug shape (including alternate formatting), but schedule
// nested fields explicitly instead of recursively invoking their formatters.
impl fmt::Debug for IvyJson {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        enum Node<'a> {
            Value(&'a IvyJson),
            Bool(bool),
            Number(f64),
            String(&'a JsString),
            Unit(u16),
            Units(&'a [u16]),
            Values(&'a [IvyJson]),
            Pairs(&'a [(JsString, IvyJson)]),
            Pair(&'a (JsString, IvyJson)),
        }
        enum Part<'a> {
            Node(Node<'a>, usize),
            Text(&'static str),
            Indent(usize),
        }
        fn group<'a>(
            f: &mut fmt::Formatter<'_>,
            pending: &mut Vec<Part<'a>>,
            depth: usize,
            name: &str,
            open: &'static str,
            close: &'static str,
            nodes: Vec<Node<'a>>,
        ) -> fmt::Result {
            f.write_str(name)?;
            f.write_str(open)?;
            if nodes.is_empty() {
                return f.write_str(close);
            }
            pending.push(Part::Text(close));
            if f.alternate() {
                f.write_str("\n")?;
                pending.push(Part::Indent(depth));
                for node in nodes.into_iter().rev() {
                    pending.push(Part::Text(",\n"));
                    pending.push(Part::Node(node, depth + 1));
                    pending.push(Part::Indent(depth + 1));
                }
            } else {
                for (index, node) in nodes.into_iter().enumerate().rev() {
                    pending.push(Part::Node(node, depth + 1));
                    if index > 0 {
                        pending.push(Part::Text(", "));
                    }
                }
            }
            Ok(())
        }
        let mut pending = vec![Part::Node(Node::Value(self), 0)];
        while let Some(part) = pending.pop() {
            let (node, depth) = match part {
                Part::Text(text) => {
                    f.write_str(text)?;
                    continue;
                }
                Part::Indent(depth) => {
                    for _ in 0..depth {
                        f.write_str("    ")?;
                    }
                    continue;
                }
                Part::Node(node, depth) => (node, depth),
            };
            let (name, open, close, nodes) = match node {
                Node::Bool(value) => {
                    write!(f, "{value:?}")?;
                    continue;
                }
                Node::Number(value) => {
                    write!(f, "{value:?}")?;
                    continue;
                }
                Node::Unit(value) => {
                    write!(f, "{value:?}")?;
                    continue;
                }
                Node::Value(IvyJson::Null) => {
                    f.write_str("Null")?;
                    continue;
                }
                Node::Value(IvyJson::Bool(value)) => ("Bool", "(", ")", vec![Node::Bool(*value)]),
                Node::Value(IvyJson::Number(value)) => {
                    ("Number", "(", ")", vec![Node::Number(*value)])
                }
                Node::Value(IvyJson::String(value)) => {
                    ("String", "(", ")", vec![Node::String(value)])
                }
                Node::Value(IvyJson::Array(values)) => {
                    ("Array", "(", ")", vec![Node::Values(values)])
                }
                Node::Value(IvyJson::Object(pairs)) => {
                    ("Object", "(", ")", vec![Node::Pairs(pairs)])
                }
                Node::String(value) => ("JsString", "(", ")", vec![Node::Units(&value.0)]),
                Node::Units(units) => (
                    "",
                    "[",
                    "]",
                    units.iter().copied().map(Node::Unit).collect(),
                ),
                Node::Values(values) => ("", "[", "]", values.iter().map(Node::Value).collect()),
                Node::Pairs(pairs) => ("", "[", "]", pairs.iter().map(Node::Pair).collect()),
                Node::Pair((key, value)) => {
                    ("", "(", ")", vec![Node::String(key), Node::Value(value)])
                }
            };
            group(f, &mut pending, depth, name, open, close, nodes)?;
        }
        Ok(())
    }
}
