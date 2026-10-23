//! Current ticket instructions and retained contact text. Formatting neither
//! changes workflow state nor dispatches a native request or user message.
use crate::wake::missing_wake_trigger;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::stringify};

pub const ATTACHMENT_GUIDANCE: &str = "Attach files only when explicitly requested or when they add clear value. Limit attachments to what is necessary and keep them as small as possible. Attach build artifacts only when explicitly requested by the user.";

/// The current prompt policy consumes the declared execution-change boolean.
#[derive(Clone, Copy, Default)]
pub struct CurrentTaskPrompts;

impl crate::native_intent::TaskPrompts for CurrentTaskPrompts {
    fn task_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        flag: Option<&Json>,
    ) -> IvyResult<JsString> {
        let changed = match flag {
            None => false,
            Some(Json::Bool(value)) => *value,
            _ => {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Execution-change prompt flag must be a boolean.",
                ));
            }
        };
        task_prompt(task, owner, changed)
    }

    fn task_update_prompt(
        &self,
        task: &Json,
        owner: &JsString,
        changed: bool,
    ) -> IvyResult<JsString> {
        task_prompt(task, owner, changed)
    }
}

fn text<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn push(output: &mut JsString, text: &str) {
    output.0.extend(text.encode_utf16());
}

/// Execution-setting changes stop at a safe boundary. Ordinary input asks the
/// same conversation to decide from the current ticket, including closed work.
pub fn task_prompt(
    task: &Json,
    service_node_id: &JsString,
    execution_change_requested: bool,
) -> IvyResult<JsString> {
    let mut output = JsString::from("TaskBoard ");
    output.0.extend(&text(task, "taskKey")?.0);
    push(&mut output, ", Ivy MCP service ");
    output.0.extend(&service_node_id.0);
    if execution_change_requested {
        push(
            &mut output,
            ". Your execution settings have changed. Read the ticket and new comments. At the next safe stopping point, save your work, briefly describe what remains, and end this turn. TaskBoard will continue this same conversation with the latest settings. Do not mark the ticket ready for review merely for this settings change.",
        );
        return Ok(output);
    }
    push(
        &mut output,
        ". Read the ticket and new comments. Decide from the current ticket whether to reply, continue work or wait, and act in this turn. ",
    );
    let questions = ivy_sdk::open_task_questions(task, None)?;
    if missing_wake_trigger(task, !questions.is_empty()) {
        push(
            &mut output,
            "This ticket was waiting without a wake-up trigger. Continue work, submit the completed result for review, or record a question, watched ticket or next appointment. ",
        );
    }
    push(&mut output, ATTACHMENT_GUIDANCE);
    Ok(output)
}

fn requests(comment: &Json) -> IvyResult<JsString> {
    if let Some(native) = comment.get("nativeInput").filter(|value| !value.is_null()) {
        let mut output = JsString::from("\nNative request identity: ");
        push(
            &mut output,
            &stringify(native.required("identity")?, usize::MAX)?,
        );
        return Ok(output);
    }
    let requests = comment.required("requests")?;
    let values = requests.as_array().ok_or_else(IvyError::internal)?;
    let mut output = JsString::default();
    if !values.is_empty() {
        push(&mut output, "\nTyped requests: ");
        push(&mut output, &stringify(requests, usize::MAX)?);
    }
    Ok(output)
}
fn instructions(comment: &Json) -> &'static str {
    if comment.get("purpose") == Some(&Json::from("handoff")) {
        "The task is ready for review. Notify the user of the delivery; no answer is required. "
    } else {
        "Discuss this important question with the user. "
    }
}

pub fn handoff(task: &Json, comment: &Json) -> IvyResult<JsString> {
    let requests = requests(comment)?;
    let mut output = JsString::from("TaskBoard ");
    output.0.extend(&text(task, "taskKey")?.0);
    push(&mut output, " has a new ticket comment from ");
    output.0.extend(&text(comment, "authorKind")?.0);
    push(&mut output, ":\n\n");
    output.0.extend(&text(comment, "body")?.0);
    output.0.extend(requests.0);
    push(&mut output, "\n\n");
    push(&mut output, instructions(comment));
    push(&mut output, "Tell the user about this comment now. Keep ");
    output.0.extend(&text(task, "taskKey")?.0);
    push(
        &mut output,
        " and its context in this current Main conversation. When the user answers or adds relevant information, use the connected Ivy Hive TaskBoard tools: read the current Task revision and append one comment action to this ticket. A normal answer is one comment with authorKind=user and empty requests/responses. For a native request, answer through the owning AgentManager using the exact native request identity; TaskBoard records that response. For a retained typed request, attach the response referencing its exact requestId. Do not treat sending this message as the user response.",
    );
    output.0.truncate(16_384);
    Ok(output)
}

pub fn voice_prompt(task: &Json, comment: &Json) -> IvyResult<JsString> {
    let requests = requests(comment)?;
    let mut output = JsString::from("You are the Voice task for a call about TaskBoard ");
    output.0.extend(&text(task, "taskKey")?.0);
    push(
        &mut output,
        ". This call concerns the following ticket comment:\n\n",
    );
    output.0.extend(&text(comment, "body")?.0);
    output.0.extend(requests.0);
    push(&mut output, "\n\n");
    push(&mut output, instructions(comment));
    push(
        &mut output,
        "When the user answers or supplies relevant information, use the connected Ivy Hive TaskBoard tools: read ",
    );
    output.0.extend(&text(task, "taskKey")?.0);
    push(
        &mut output,
        " at its current revision and append one comment action. A normal answer is one comment with authorKind=user and empty requests/responses; a native request must be answered through its owning AgentManager with its exact identity; TaskBoard records that response. The ticket is the source of truth and wakes the working task. Do not involve Main chat.",
    );
    output.0.truncate(4_096);
    Ok(output)
}
