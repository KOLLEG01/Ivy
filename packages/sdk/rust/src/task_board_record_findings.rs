use super::value::{self, *};
use ivy_contracts::{IvyJson as Json, JsString};
use std::collections::HashMap;

fn pretty(compact: &str) -> String {
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

pub(super) fn text_in<'a>(findings: &Value<'a>) -> Result<JsString> {
    let text = value::serialized(findings)?.ok_or_else(value::invalid)?;
    Ok(text_serialized(&text))
}

pub(super) fn fragments(text: &JsString) -> Vec<(usize, JsString)> {
    let mut fragments = Vec::new();
    let mut offset = 0;
    while offset < text.0.len() {
        let mut end = (offset + 48_000).min(text.0.len());
        if end < text.0.len() && (0xd800..=0xdbff).contains(&text.0[end - 1]) {
            end -= 1;
        }
        fragments.push((offset, JsString(text.0[offset..end].to_vec())));
        offset = end;
    }
    fragments
}

fn text_serialized(compact: &str) -> JsString {
    let text = pretty(compact);
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

// Replacing a duplicate preserves the first position in the retained list.
pub(super) fn dedup_by<T>(
    values: impl IntoIterator<Item = T>,
    mut serialized: impl FnMut(&T) -> Result<Option<String>>,
) -> Result<Vec<T>> {
    let mut indices = HashMap::new();
    let mut output = Vec::new();
    for value in values {
        let key = serialized(&value)?;
        if let Some(index) = indices.get(&key) {
            output[*index] = value;
        } else {
            indices.insert(key, output.len());
            output.push(value);
        }
    }
    Ok(output)
}

pub(super) fn evidence_in<'a>(findings: &Value<'a>) -> Result<Vec<Value<'a>>> {
    let findings = value::array_items(findings)?;
    let mut values = Vec::new();
    for finding in findings {
        let evidence = value::field(&finding, "evidence")?;
        if value::nullish(&evidence) {
            continue;
        }
        if let Some(items) = value::elements(&evidence) {
            values.extend(items);
        } else {
            values.push(evidence);
        }
    }
    dedup_by(values, value::serialized)
}

fn author<'a>(task: &Value<'a>, comments: &[Value<'a>]) -> Result<Json> {
    let last = comments.last().cloned().unwrap_or_else(value::missing);
    let node = if value::nullish(&last) {
        value::missing()
    } else {
        value::field(&value::field(&last, "author")?, "serviceNodeId")?
    };
    let node = if value::nullish(&node) {
        let claim = value::field(task, "claim")?;
        if value::nullish(&claim) {
            value::missing()
        } else {
            value::field(&value::field(&claim, "owner")?, "serviceNodeId")?
        }
    } else {
        node
    };
    let generation = if value::nullish(&last) {
        value::missing()
    } else {
        value::field(&value::field(&last, "author")?, "generation")?
    };
    Ok(value::object([
        ("principalId", value::str("task-board-record-upgrade")),
        (
            "serviceNodeId",
            if value::nullish(&node) {
                value::str("task-board")
            } else {
                node
            },
        ),
        (
            "generation",
            if value::nullish(&generation) {
                value::num(1.0)
            } else {
                generation
            },
        ),
        ("source", value::str("recovery")),
    ]))
}

pub(super) fn preserve_task<'a>(task: Value<'a>) -> Result<Json> {
    let mut current = value::object_rest(&task, "latestChecks")?;
    let mut groups = Vec::new();
    let latest = value::field(&task, "latestChecks")?;
    if value::has_items(&latest)? {
        groups.push((JsString::from("task"), latest));
    }
    let deliveries = value::field(&task, "deliveries")?;
    if value::has_items(&deliveries)? {
        let mut material = Vec::new();
        for receipt in value::array_items(&deliveries)? {
            let delivery = value::field(&receipt, "delivery")?;
            material.push(value::owned(value::object([
                ("result", value::field(&receipt, "result")?),
                ("codeReferences", value::field(&delivery, "codeReferences")?),
                ("evidence", value::field(&delivery, "artifacts")?),
            ])));
        }
        groups.push((
            JsString::from("saved-material"),
            value::owned(value::array(material)),
        ));
    }
    let mut comments = Vec::new();
    for comment in value::array_items(&value::field(&task, "comments")?)? {
        let handoff = value::field(&comment, "handoff")?;
        if !value::present(&handoff) {
            comments.push(comment);
            continue;
        }
        let clean = value::object_rest(&handoff, "checks")?;
        let checks = value::field(&handoff, "checks")?;
        if value::has_items(&checks)? {
            groups.push((
                concat([
                    "comment-".into(),
                    value::text(&value::field(&comment, "sequence")?)?,
                ]),
                checks.clone(),
            ));
        }
        if value::present(&checks) {
            let mut changed = value::copy_object(&comment)?;
            value::put(&mut changed, "handoff", value::owned(value::finish(clean)));
            comments.push(value::owned(value::finish(changed)));
        } else {
            comments.push(comment);
        }
    }
    if groups.is_empty() {
        value::put(
            &mut current,
            "comments",
            value::owned(value::array(comments)),
        );
        return Ok(value::finish(current));
    }
    let author = author(&task, &comments)?;
    let mut sequence: f64 = 0.0;
    for comment in &comments {
        let next = value::number(&value::field(comment, "sequence")?)?;
        sequence = sequence.max(next);
    }
    for (source, findings) in groups {
        let text = if source == JsString::from("saved-material") {
            format!("Imported references for {} saved results. Details remain with those result records and their attachments.", value::array_items(&findings)?.len()).into()
        } else {
            text_in(&findings)?
        };
        let fragments: Vec<_> = fragments(&text).into_iter().map(|(_, text)| text).collect();
        let mut retained = Vec::new();
        for artifact in evidence_in(&findings)? {
            if !value::is_missing(&value::field(&artifact, "byteLength")?) {
                retained.push(artifact);
            }
        }
        let mut attachments = Vec::new();
        for artifact in retained {
            let object = value::field(&artifact, "object")?;
            let id = concat([
                value::text(&value::field(&object, "objectId")?)?,
                "-".into(),
                value::text(&value::field(&object, "revision")?)?,
            ]);
            let mut attachment = value::empty();
            value::put(&mut attachment, "attachmentId", value::js(id));
            value::put(&mut attachment, "object", object);
            value::put(
                &mut attachment,
                "filename",
                value::field(&artifact, "label")?,
            );
            value::put(
                &mut attachment,
                "mediaType",
                value::field(&artifact, "mediaType")?,
            );
            value::put(
                &mut attachment,
                "byteLength",
                value::field(&artifact, "byteLength")?,
            );
            value::put(
                &mut attachment,
                "contentHash",
                value::field(&artifact, "contentHash")?,
            );
            value::put(&mut attachment, "uploader", value::borrow(&author));
            value::put(
                &mut attachment,
                "createdAt",
                value::field(&task, "updatedAt")?,
            );
            attachments.push(value::owned(value::finish(attachment)));
        }
        for index in 0..fragments.len().max(attachments.len().div_ceil(32)) {
            let comment_id = concat([
                "retained-findings-".into(),
                source.clone(),
                "-".into(),
                value::text(&value::field(&task, "updatedAt")?)?,
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
            let mut duplicate = false;
            for comment in &comments {
                if value::equal(
                    &value::field(comment, "commentId")?,
                    &value::js(comment_id.clone()),
                ) {
                    duplicate = true;
                    break;
                }
            }
            if duplicate {
                continue;
            }
            sequence += 1.0;
            let start = (index * 32).min(attachments.len());
            let end = ((index + 1) * 32).min(attachments.len());
            let mut comment = value::empty();
            value::put(&mut comment, "commentId", value::js(comment_id.clone()));
            value::put(&mut comment, "sequence", value::num(sequence));
            value::put(&mut comment, "author", value::borrow(&author));
            value::put(&mut comment, "authorKind", value::str("system"));
            value::put(&mut comment, "purpose", value::str("update"));
            value::put(&mut comment, "body", value::js(body));
            value::put(
                &mut comment,
                "requests",
                value::owned(value::array(Vec::new())),
            );
            value::put(
                &mut comment,
                "responses",
                value::owned(value::array(Vec::new())),
            );
            value::put(
                &mut comment,
                "attachments",
                value::owned(value::array(attachments[start..end].to_vec())),
            );
            for key in ["run", "turnId", "replyTo", "delivery"] {
                value::put(&mut comment, key, value::null());
            }
            value::put(&mut comment, "createdAt", value::field(&task, "updatedAt")?);
            value::put(&mut comment, "operationId", value::js(comment_id));
            comments.push(value::owned(value::finish(comment)));
        }
    }
    value::put(
        &mut current,
        "comments",
        value::owned(value::array(comments)),
    );
    Ok(value::finish(current))
}
