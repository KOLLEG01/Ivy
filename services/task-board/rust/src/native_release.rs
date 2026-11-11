//! Small completion declaration and dispatch fence; original calls keep receipts.
use ivy_contracts::IvyJson as Json;

fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
pub fn comment_count(task: &Json) -> f64 {
    f(f(task, "commentStore"), "length")
        .as_f64()
        .unwrap_or_else(|| {
            f(task, "comments")
                .as_array()
                .map_or(0, |values| values.len()) as f64
        })
}
pub fn pending(run: &Json) -> bool {
    f(run, "contextRelease").get("state").is_some_and(|state| {
        ["pending", "observing", "outcome_unknown"]
            .iter()
            .any(|s| state.is_str(s))
    })
}
pub fn call(run: &Json) -> &Json {
    f(f(run, "contextRelease"), "call")
}
pub fn owns_fence(task: &Json, run_pin: &Json) -> bool {
    !f(task, "contextRelease").is_null()
        && f(f(task, "contextRelease"), "objectId") == f(run_pin, "objectId")
}
/// Rechecked against the current Task before an unobserved original is sent.
/// Native idle, pending inputs and subscriber ownership are checked by AgentManager.
pub fn eligible(task: &Json, run: &Json, run_pin: &Json) -> bool {
    let release = f(run, "contextRelease");
    f(run, "phase").is_str("completed")
        && f(f(run, "externalOutcome"), "state").is_str("succeeded")
        && !f(run, "result").is_null()
        && f(run, "cancellation").is_null()
        && f(release, "state").is_str("pending")
        && owns_fence(task, run_pin)
        && f(task, "claim").is_null()
        && f(task, "publication").is_null()
        && f(task, "completionIssue").is_null()
        && f(task, "runtimeWait").is_null()
        && f(task, "executionChangeRequested").as_bool() != Some(true)
        && f(task, "lastRun") == run_pin
        && f(task, "primaryResourceRef") == f(run, "primaryResourceRef")
        && f(task, "primaryBackend") == f(f(run, "target"), "backend")
        && f(task, "workRevision") == f(release, "workRevision")
        && Some(comment_count(task)) == f(release, "commentCount").as_f64()
}

/// Parameter shape is managed, while its checked native definition is unsubscribe.
pub fn definition(method: &ivy_contracts::JsString) -> &ivy_contracts::JsString {
    static UNSUBSCRIBE: std::sync::OnceLock<ivy_contracts::JsString> = std::sync::OnceLock::new();
    if method.eq_str("agent.release") {
        UNSUBSCRIBE.get_or_init(|| "thread/unsubscribe".into())
    } else {
        method
    }
}
