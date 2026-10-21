use super::value::*;
use ivy_contracts::{IvyJson as Json, JsString, json};
use std::collections::HashMap;

fn serialized(value: &Json) -> String {
    // No validator/size policy is introduced at the original JSON.stringify site.
    json::stringify(value, usize::MAX).expect("unbounded JSON serialization")
}

fn pretty(value: &Json) -> String {
    let compact = serialized(value);
    let mut output = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut prior = None;
    let mut chars = compact.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            output.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                output.push(c);
            }
            '[' | '{' => {
                output.push(c);
                depth += 1;
                if !matches!((c, chars.peek()), ('[', Some(']')) | ('{', Some('}'))) {
                    output.push('\n');
                    output.push_str(&"  ".repeat(depth));
                }
            }
            ']' | '}' => {
                depth -= 1;
                if !matches!((prior, c), (Some('['), ']') | (Some('{'), '}')) {
                    output.push('\n');
                    output.push_str(&"  ".repeat(depth));
                }
                output.push(c);
            }
            ',' => {
                output.push_str(",\n");
                output.push_str(&"  ".repeat(depth));
            }
            ':' => output.push_str(": "),
            _ => output.push(c),
        }
        prior = Some(c);
    }
    output
}

pub(super) fn text(findings: &Json) -> JsString {
    let text = pretty(findings);
    let mut longest = 2;
    let mut run = 0;
    for byte in text.bytes() {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    format!("## Preserved verification findings\n\n{fence}json\n{text}\n{fence}").into()
}

// A Map replaces the value of an existing string key without changing its order.
pub(super) fn dedup(values: impl IntoIterator<Item = Json>) -> Vec<Json> {
    let mut indices = HashMap::new();
    let mut output = Vec::new();
    for value in values {
        let key = serialized(&value);
        if let Some(index) = indices.get(&key) {
            output[*index] = value;
        } else {
            indices.insert(key, output.len());
            output.push(value);
        }
    }
    output
}

pub(super) fn evidence(findings: &Json) -> Result<Vec<Json>> {
    let findings = findings
        .as_array()
        .ok_or_else(|| type_error("findings.flatMap is not a function"))?;
    let mut values = Vec::new();
    for finding in findings {
        match field(Some(finding), "evidence")? {
            None | Some(Json::Null) => {}
            Some(Json::Array(items)) => values.extend(items.iter().cloned()),
            Some(value) => values.push(value.clone()),
        }
    }
    Ok(dedup(values))
}

fn author(task: &Json, comments: &[Json]) -> Result<Json> {
    let last = comments.last();
    let node = if let Some(last) = last {
        field(field(Some(last), "author")?, "serviceNodeId")?
    } else {
        None
    };
    let node = if nullish(node) {
        let claim = field(Some(task), "claim")?;
        if nullish(claim) {
            None
        } else {
            field(field(claim, "owner")?, "serviceNodeId")?
        }
    } else {
        node
    };
    let generation = if let Some(last) = last {
        field(field(Some(last), "author")?, "generation")?
    } else {
        None
    };
    Ok(Json::object([
        ("principalId", "task-board-record-upgrade".into()),
        (
            "serviceNodeId",
            if nullish(node) {
                "task-board".into()
            } else {
                node.unwrap().clone()
            },
        ),
        (
            "generation",
            if nullish(generation) {
                Json::Number(1.0)
            } else {
                generation.unwrap().clone()
            },
        ),
        ("source", "recovery".into()),
    ]))
}

pub(super) fn preserve_task(task: &Json) -> Result<Json> {
    let mut current = object_rest(Some(task), "task", "latestChecks")?;
    let mut groups = Vec::new();
    let latest = field(Some(task), "latestChecks")?;
    if has_length(latest) {
        groups.push((JsString::from("task"), latest.unwrap().clone()));
    }
    let original_comments = field(Some(&current), "comments")?;
    if nullish(original_comments) {
        field(original_comments, "map")?;
    }
    let original_comments = original_comments
        .and_then(Json::as_array)
        .ok_or_else(|| type_error("current.comments.map is not a function"))?;
    let mut comments = Vec::with_capacity(original_comments.len());
    for comment in original_comments {
        let handoff = field(Some(comment), "handoff")?;
        if !truthy(handoff) {
            comments.push(comment.clone());
            continue;
        }
        let clean = object_rest(handoff, "comment.handoff", "checks")?;
        let checks = field(handoff, "checks")?;
        if has_length(checks) {
            groups.push((
                concat([
                    "comment-".into(),
                    string(field(Some(comment), "sequence")?)?,
                ]),
                checks.unwrap().clone(),
            ));
        }
        if truthy(checks) {
            let mut changed = spread(Some(comment));
            changed.insert("handoff", clean);
            comments.push(changed);
        } else {
            comments.push(comment.clone());
        }
    }
    let author = author(task, &comments)?;
    let mut sequence: f64 = 0.0;
    for comment in &comments {
        let next = number(field(Some(comment), "sequence")?)?;
        sequence = if sequence.is_nan() || next.is_nan() {
            f64::NAN
        } else {
            sequence.max(next)
        };
    }
    for (source, findings) in groups {
        let text = text(&findings);
        let mut fragments = Vec::new();
        let mut offset = 0;
        while offset < text.0.len() {
            let mut end = (offset + 48_000).min(text.0.len());
            if end < text.0.len() && (0xd800..=0xdbff).contains(&text.0[end - 1]) {
                end -= 1;
            }
            fragments.push(JsString(text.0[offset..end].to_vec()));
            offset = end;
        }
        let mut retained = Vec::new();
        for artifact in evidence(&findings)? {
            if field(Some(&artifact), "byteLength")?.is_none() {
                continue;
            }
            retained.push(artifact);
        }
        let mut attachments = Vec::new();
        for artifact in retained {
            let object = field(Some(&artifact), "object")?;
            let id = concat([
                string(field(object, "objectId")?)?,
                "-".into(),
                string(field(object, "revision")?)?,
            ]);
            let mut attachment = Json::object([("attachmentId", Json::String(id))]);
            put(&mut attachment, "object", object);
            put(
                &mut attachment,
                "filename",
                field(Some(&artifact), "label")?,
            );
            put(
                &mut attachment,
                "mediaType",
                field(Some(&artifact), "mediaType")?,
            );
            put(
                &mut attachment,
                "byteLength",
                field(Some(&artifact), "byteLength")?,
            );
            put(
                &mut attachment,
                "contentHash",
                field(Some(&artifact), "contentHash")?,
            );
            attachment.insert("uploader", author.clone());
            put(
                &mut attachment,
                "createdAt",
                field(Some(task), "updatedAt")?,
            );
            attachments.push(attachment);
        }
        for index in 0..fragments.len().max(attachments.len().div_ceil(32)) {
            let comment_id = concat([
                "retained-findings-".into(),
                source.clone(),
                "-".into(),
                string(field(Some(task), "updatedAt")?)?,
                "-".into(),
                index.to_string().into(),
            ]);
            let body = concat([
                "Preserved from ".into(),
                source.clone(),
                "; existing work and questions are unchanged.\n\n".into(),
                fragments
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| "Additional retained verification evidence.".into()),
            ]);
            if comments.iter().any(|comment| {
                comment.get("commentId").and_then(Json::as_js_string) == Some(&comment_id)
            }) {
                continue;
            }
            sequence += 1.0;
            let start = (index * 32).min(attachments.len());
            let end = ((index + 1) * 32).min(attachments.len());
            let mut comment = Json::object([
                ("commentId", Json::String(comment_id.clone())),
                ("sequence", Json::Number(sequence)),
                ("author", author.clone()),
                ("authorKind", "system".into()),
                ("purpose", "update".into()),
                ("body", Json::String(body)),
                ("requests", Json::Array(vec![])),
                ("responses", Json::Array(vec![])),
                ("attachments", Json::Array(attachments[start..end].to_vec())),
                ("run", Json::Null),
                ("turnId", Json::Null),
                ("replyTo", Json::Null),
                ("delivery", Json::Null),
            ]);
            put(&mut comment, "createdAt", field(Some(task), "updatedAt")?);
            comment.insert("operationId", Json::String(comment_id));
            comments.push(comment);
        }
    }
    current.insert("comments", Json::Array(comments));
    Ok(current)
}
