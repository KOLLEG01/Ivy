use super::value::*;
use ivy_contracts::{IvyJson as J, JsString};
pub(super) fn tool_item(id: JsString, e: &J, cwd: &J) -> J {
    let name = field(e, "toolName");
    let input = field(e, "input");
    let tool_id = string(field(e, "toolUseId"));
    if is(name, "Bash") {
        return J::object([
            ("type", "commandExecution".into()),
            ("id", id.into_json()),
            (
                "command",
                string(&coalesce(field(input, "command"), &"".into())).into_json(),
            ),
            (
                "cwd",
                string(&coalesce(field(input, "cwd"), cwd)).into_json(),
            ),
            (
                "processId",
                concat(&[&"claude:".into(), &tool_id]).into_json(),
            ),
            ("source", "agent".into()),
            ("status", "inProgress".into()),
            ("commandActions", J::Array(vec![])),
            ("aggregatedOutput", J::Null),
            ("exitCode", J::Null),
            ("durationMs", J::Null),
        ]);
    }
    if ["Edit", "Write", "MultiEdit"].iter().any(|n| is(name, n)) {
        return J::object([
            ("type", "fileChange".into()),
            ("id", id.into_json()),
            ("changes", J::Array(file_changes(name, input))),
            ("status", "inProgress".into()),
        ]);
    }
    if is(name, "WebSearch") {
        let q = string(&coalesce(field(input, "query"), &"".into()));
        return J::object([
            ("type", "webSearch".into()),
            ("id", id.into_json()),
            ("query", q.clone().into_json()),
            (
                "action",
                J::object([
                    ("type", "search".into()),
                    (
                        "query",
                        if q.0.is_empty() {
                            J::Null
                        } else {
                            q.into_json()
                        },
                    ),
                    ("queries", J::Null),
                ]),
            ),
        ]);
    }
    let mut display = string(name);
    if is(name, "Read") {
        let raw = string(&if truthy(field(input, "file_path")) {
            field(input, "file_path").clone()
        } else if truthy(field(input, "path")) {
            field(input, "path").clone()
        } else {
            "".into()
        });
        if !raw.0.is_empty() {
            let cwd = string(cwd);
            let path = if !cwd.0.is_empty() && raw.0.starts_with(&cwd.0) {
                let tail = raw.0[cwd.0.len()..]
                    .iter()
                    .copied()
                    .skip_while(|c| *c == 47)
                    .collect::<Vec<_>>();
                let tail = JsString(tail);
                if tail.0.is_empty() { raw.clone() } else { tail }
            } else {
                short_path(&raw)
            };
            display = concat(&[&"Read ".into(), &path])
        }
    } else if is(name, "Grep") {
        let pattern = string(&coalesce(field(input, "pattern"), &"".into()));
        let path = if truthy(field(input, "path")) {
            concat(&[
                &" (".into(),
                &short_path(&string(field(input, "path"))),
                &")".into(),
            ])
        } else {
            "".into()
        };
        if !pattern.0.is_empty() {
            display = concat(&[&"Grep ".into(), &pattern, &path])
        }
    } else if is(name, "Glob") {
        let pattern = string(&coalesce(field(input, "pattern"), &"".into()));
        if !pattern.0.is_empty() {
            display = concat(&[&"Glob ".into(), &pattern])
        }
    }
    J::object([
        ("type", "mcpToolCall".into()),
        ("id", id.into_json()),
        ("server", "claude-code".into()),
        ("tool", display.into_json()),
        ("status", "inProgress".into()),
        ("arguments", input.clone()),
        ("result", J::Null),
        ("error", J::Null),
        ("durationMs", J::Null),
    ])
}
fn short_path(raw: &JsString) -> JsString {
    let parts = raw
        .0
        .split(|u| *u == 47)
        .filter(|s| !s.is_empty())
        .map(|s| JsString(s.to_vec()))
        .collect::<Vec<_>>();
    if parts.len() > 2 {
        join(&parts[parts.len() - 2..], "/")
    } else {
        raw.clone()
    }
}
fn diff(path: &JsString, old: &JsString, new: &JsString) -> J {
    let mut lines = vec![
        concat(&[&"--- a/".into(), path]),
        concat(&[&"+++ b/".into(), path]),
        "@@".into(),
    ];
    for (prefix, text) in [("-", old), ("+", new)] {
        for line in text.0.split(|u| *u == 10).filter(|l| !l.is_empty()) {
            lines.push(concat(&[&prefix.into(), &JsString(line.to_vec())]))
        }
    }
    lines.push("".into());
    join(&lines, "\n").into_json()
}
fn file_changes(name: &J, input: &J) -> Vec<J> {
    let edit = |path: J, old: J, new: J, add: bool| {
        let path = string(&path);
        J::object([
            ("path", path.clone().into_json()),
            (
                "kind",
                if add {
                    J::object([("type", "add".into())])
                } else {
                    J::object([("type", "update".into()), ("move_path", J::Null)])
                },
            ),
            ("diff", diff(&path, &string(&old), &string(&new))),
        ])
    };
    if is(name, "MultiEdit") {
        if let Some(edits) = field(input, "edits").as_array() {
            return edits
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    edit(
                        coalesce(
                            field(input, "file_path"),
                            &coalesce(field(input, "path"), &format!("edit-{i}").into()),
                        ),
                        coalesce(field(e, "old_string"), &"".into()),
                        coalesce(field(e, "new_string"), &"".into()),
                        false,
                    )
                })
                .collect();
        }
    }
    let path = coalesce(
        field(input, "file_path"),
        &coalesce(
            field(input, "path"),
            &coalesce(field(input, "filename"), &"unknown".into()),
        ),
    );
    vec![if is(name, "Write") {
        edit(
            path,
            "".into(),
            coalesce(field(input, "content"), &"".into()),
            true,
        )
    } else {
        edit(
            path,
            coalesce(field(input, "old_string"), &"".into()),
            coalesce(field(input, "new_string"), &"".into()),
            false,
        )
    }]
}
pub(super) fn result_text(content: &J) -> JsString {
    if let Some(s) = content.as_js_string() {
        return s.clone();
    }
    let from = |v: &J| {
        if let Some(s) = v.as_js_string() {
            return s.clone();
        }
        for key in ["text", "content"] {
            if let Some(s) = field(v, key).as_js_string() {
                return s.clone();
            }
        }
        "".into()
    };
    if let Some(values) = content.as_array() {
        join(
            &values
                .iter()
                .map(from)
                .filter(|s| !s.0.is_empty())
                .collect::<Vec<_>>(),
            "\n",
        )
    } else {
        from(content)
    }
}
pub(super) fn result_wrap(content: &J) -> J {
    let (blocks, structured) = if content.is_null() {
        (vec![], J::Null)
    } else if let Some(a) = content.as_array() {
        (a.to_vec(), J::Null)
    } else {
        let text = if content.as_js_string().is_some() {
            content.clone()
        } else {
            encode(content).unwrap_or_default().into()
        };
        (
            vec![J::object([("type", "text".into()), ("text", text)])],
            if content.as_object().is_some() {
                content.clone()
            } else {
                J::Null
            },
        )
    };
    J::object([
        ("content", J::Array(blocks)),
        ("structuredContent", structured),
        ("_meta", J::Null),
    ])
}
pub(super) fn error_wrap(content: &J) -> J {
    let message = if content.is_null() {
        "tool call failed".into()
    } else if content.as_js_string().is_some() {
        content.clone()
    } else if let Some(a) = content.as_array() {
        let text = join(
            &a.iter()
                .filter(|b| is(field(b, "type"), "text"))
                .map(|b| string(&coalesce(field(b, "text"), &"".into())))
                .filter(|s| !s.0.is_empty())
                .collect::<Vec<_>>(),
            "\n",
        );
        if text.0.is_empty() {
            encode(content).unwrap_or_default().into()
        } else {
            text.into_json()
        }
    } else if field(content, "message").as_js_string().is_some() {
        field(content, "message").clone()
    } else {
        encode(content).unwrap_or_default().into()
    };
    J::object([("message", message)])
}
pub(super) fn exit_code(content: &J) -> Option<f64> {
    let text = if let Some(s) = content.as_js_string() {
        s.clone()
    } else if let Some(a) = content.as_array() {
        join(
            &a.iter()
                .map(|p| {
                    if p.as_js_string().is_some() {
                        string(p)
                    } else {
                        string(&coalesce(field(p, "text"), &"".into()))
                    }
                })
                .collect::<Vec<_>>(),
            "",
        )
    } else {
        "".into()
    };
    let lower = text.to_english_lowercase();
    for i in 0..lower.0.len() {
        for prefix in ["exit code:", "exit status:"] {
            let prefix = JsString::from(prefix);
            if !lower.0[i..].starts_with(&prefix.0) {
                continue;
            }
            let start = i + prefix.0.len();
            let mut digits = lower.0[start..]
                .iter()
                .copied()
                .skip_while(|u| JsString(vec![*u]).trim().0.is_empty())
                .collect::<Vec<_>>();
            let negative = digits.first() == Some(&45);
            if negative {
                digits.remove(0);
            }
            let count = digits.iter().take_while(|u| (48..=57).contains(*u)).count();
            if count > 0 {
                let value = number(&J::String(JsString(digits[..count].to_vec())))
                    * if negative { -1.0 } else { 1.0 };
                if value.is_finite() {
                    return Some(value);
                }
            }
        }
    }
    None
}
pub(super) fn web_action(query: &J, text: &JsString) -> J {
    if truthy(query) {
        for i in 0..text.0.len() {
            if ["http://", "https://"]
                .iter()
                .any(|p| text.0[i..].starts_with(&JsString::from(*p).0))
            {
                let end = text.0[i..]
                    .iter()
                    .position(|u| {
                        [41, 93, 34, 39, 60].contains(u) || JsString(vec![*u]).trim().0.is_empty()
                    })
                    .map(|n| i + n)
                    .unwrap_or(text.0.len());
                let prefix = if text.0[i..].starts_with(&JsString::from("https://").0) {
                    8
                } else {
                    7
                };
                if end > i + prefix {
                    return J::object([
                        ("type", "openPage".into()),
                        ("url", slice(text, i, end).into_json()),
                    ]);
                }
            }
        }
    }
    J::object([
        ("type", "search".into()),
        (
            "query",
            if truthy(query) {
                query.clone()
            } else {
                J::Null
            },
        ),
        ("queries", J::Null),
    ])
}
pub(super) fn normalize_answers(response: &J, questions: &[J]) -> J {
    let root = coalesce(field(response, "answers"), response);
    let mut answers = object();
    for q in questions {
        let key = string(field(q, "id"));
        let raw = root.get_key(&key).unwrap_or(&NULL);
        let list = if let Some(a) = raw.as_array() {
            a.iter().map(|v| string(v).into_json()).collect()
        } else if raw.as_js_string().is_some() {
            vec![raw.clone()]
        } else {
            field(raw, "answers")
                .as_array()
                .unwrap_or(&[])
                .iter()
                .map(|v| string(v).into_json())
                .collect()
        };
        let mut slot = J::object([("answers", J::Array(list))]);
        if field(raw, "notes").as_js_string().is_some() {
            slot.insert("notes", field(raw, "notes").clone())
        }
        answers.insert(key, slot);
    }
    J::object([("answers", answers)])
}
pub(super) fn answer_content(questions: &[J], answers: &J) -> J {
    J::Array(
        questions
            .iter()
            .map(|q| {
                let key = string(field(q, "id"));
                let slot = field(answers, "answers").get_key(&key).unwrap_or(&NULL);
                let selected = field(slot, "answers")
                    .as_array()
                    .unwrap_or(&[])
                    .iter()
                    .filter(|v| truthy(v) && !is(v, "Other"))
                    .map(string)
                    .collect::<Vec<_>>();
                let notes = field(slot, "notes");
                let body = if truthy(notes) && selected.is_empty() {
                    string(notes)
                } else {
                    let joined = join(&selected, ", ");
                    if joined.0.is_empty() {
                        "(no answer)".into()
                    } else {
                        joined
                    }
                };
                J::object([
                    ("type", "inputText".into()),
                    (
                        "text",
                        concat(&[&string(field(q, "header")), &": ".into(), &body]).into_json(),
                    ),
                ])
            })
            .collect(),
    )
}
