use ivy_contracts::{IvyError, IvyJson, Outcome};
use ivy_hive::worker_client::{
    Effects, Lifecycle, MAX_QUEUED_BYTES, RequestId, RequestOptions, WorkerClient,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn ready() -> WorkerClient {
    let mut client = WorkerClient::default();
    let generation = client.start(0).unwrap();
    assert!(client.started(generation));
    client
}
fn options(timeout: u64, terminate: bool) -> RequestOptions {
    RequestOptions {
        timeout_ms: timeout,
        terminate_on_timeout: terminate,
        ..RequestOptions::default()
    }
}

#[test]
fn rearmed_timer_keeps_queue_timeout_order_at_equal_deadlines() {
    let mut client = ready();
    let running = client.post(0, options(10, true)).unwrap();
    let queued = client.post(0, options(10, true)).unwrap();
    client.request_started(running, 0);
    let effects = client.expire(10);
    assert_eq!(effects.settlements[0].id, queued);
    assert_eq!(
        effects.settlements[0].result.as_ref().unwrap_err().message,
        "Hive storage queue wait expired; reconcile operation identity."
    );
    assert_eq!(effects.settlements[1].id, running);
    assert_eq!(effects.retire, Some(running.generation));
}

#[test]
fn timed_out_posted_work_retains_count_and_bytes_until_actual_completion() {
    let mut client = ready();
    let ids: Vec<_> = (0..512)
        .map(|_| client.post(0, options(10, true)).unwrap())
        .collect();
    let expired = client.expire(10);
    assert_eq!(expired.settlements.len(), 512);
    assert!(expired.retire.is_none());
    assert!(
        expired
            .settlements
            .iter()
            .all(|s| s.result.as_ref().unwrap_err().outcome == Outcome::Unknown)
    );
    assert_eq!(client.pending_count(), 512);
    assert_eq!(client.queued_bytes(), 512 * 4096);
    let error = client.post(10, options(10, true)).unwrap_err();
    assert_eq!(error.code, "limit_exceeded");
    assert_eq!(error.outcome, Outcome::NotExecuted);
    assert!(client.reply(ids[0], Ok(IvyJson::Null)).is_none());
    assert_eq!(client.pending_count(), 511);
    assert!(client.post(10, options(10, true)).is_ok());
}

#[test]
fn queued_work_gets_execution_deadline_even_after_its_caller_has_timed_out() {
    let mut client = ready();
    let id = client.post(0, options(10, true)).unwrap();
    assert_eq!(client.expire(10).settlements.len(), 1);
    client.request_started(id, 50);
    assert_eq!(client.next_deadline(), Some(60));
    assert!(client.expire(59).retire.is_none());
    let expired = client.expire(60);
    assert!(expired.settlements.is_empty());
    assert_eq!(expired.retire, Some(id.generation));
    assert!(!client.ready());
}

#[test]
fn read_timeout_does_not_kill_running_mutation_or_release_read_reservation() {
    let mut client = ready();
    let mutation = client.post(0, options(1000, true)).unwrap();
    let read = client.post(0, options(5, false)).unwrap();
    client.request_started(mutation, 0);
    client.request_started(read, 0);
    let effects = client.expire(5);
    assert_eq!(effects.settlements.len(), 1);
    assert_eq!(effects.settlements[0].id, read);
    assert!(effects.retire.is_none());
    assert!(client.ready());
    assert_eq!(client.queued_bytes(), 8192);
    assert!(client.reply(read, Ok(true.into())).is_none());
    assert!(client.reply(mutation, Ok(true.into())).is_some());
    assert_eq!(client.queued_bytes(), 0);
}

#[test]
fn backup_progress_extends_only_its_deadline_and_nonbackup_progress_is_ignored() {
    let mut client = ready();
    let backup = client
        .post(
            0,
            RequestOptions {
                backup: true,
                ..options(10, true)
            },
        )
        .unwrap();
    let read = client.post(0, options(5, false)).unwrap();
    client.request_started(backup, 0);
    client.request_started(read, 0);
    client.progress(backup, 4);
    client.progress(read, 4);
    assert_eq!(client.expire(5).settlements[0].id, read);
    assert!(client.expire(13).retire.is_none());
    assert_eq!(client.expire(14).retire, Some(backup.generation));
}

#[test]
fn unconfirmed_stop_fences_replacement_and_stale_generation_cannot_complete_new_work() {
    let mut client = ready();
    let old = client.post(0, options(5, true)).unwrap();
    client.request_started(old, 0);
    assert_eq!(client.expire(5).retire, Some(old.generation));
    assert!(client.start(6).is_err());
    assert!(!client.retired(old.generation, false));
    assert_eq!(
        client.lifecycle(),
        Lifecycle::StopUnconfirmed(old.generation)
    );
    assert!(client.start(7).is_err());
    assert!(client.retired(old.generation, true));
    let new = client.start(8).unwrap();
    assert_ne!(new, old.generation);
    assert!(client.started(new));
    let request = client.post(8, options(5, true)).unwrap();
    assert!(client.reply(old, Ok("stale".into())).is_none());
    client.request_started(old, 9);
    client.progress(old, 9);
    assert!(client.connection_failed(old.generation).retire.is_none());
    assert!(!client.retired(old.generation, true));
    assert_eq!(client.pending_count(), 1);
    assert_eq!(client.next_deadline(), Some(13));
    assert_eq!(
        client
            .reply(request, Ok("current".into()))
            .unwrap()
            .result
            .unwrap(),
        "current".into()
    );
}

#[test]
fn startup_deadline_and_shutdown_retain_their_distinct_public_errors() {
    let mut client = WorkerClient::default();
    let generation = client.start(100).unwrap();
    assert_eq!(client.start(200).unwrap(), generation);
    assert!(client.expire(30_099).retire.is_none());
    let effects = client.expire(30_100);
    assert_eq!(effects.retire, Some(generation));
    assert_eq!(
        effects.failure.unwrap(),
        IvyError::new(
            "deadline_exceeded",
            "Hive storage startup exceeded its deadline."
        )
    );
    assert!(!client.started(generation));
    assert!(client.retired(generation, true));
    let generation = client.start(30_101).unwrap();
    assert!(client.started(generation));
    client.post(30_101, RequestOptions::default()).unwrap();
    let effects = client.shutdown();
    assert!(effects.failure.is_none());
    assert_eq!(
        effects.settlements[0].result.as_ref().unwrap_err(),
        &IvyError::new("service_unavailable", "Hive is shutting down.")
            .with_outcome(Outcome::Unknown)
    );
    assert!(client.shutdown().retire.is_none());
}

#[test]
fn byte_limit_and_javascript_reservation_coercion_are_preserved() {
    for (reservation, expected) in [
        (0.0, 1024),
        (100.0, 1024),
        (1024.0, 1024),
        (-1.0, 4096),
        (1.5, 4096),
        (f64::NAN, 4096),
        (f64::INFINITY, 4096),
        (9_007_199_254_740_992.0, 4096),
    ] {
        let mut client = ready();
        client
            .post(
                0,
                RequestOptions {
                    reserved_bytes: reservation,
                    ..RequestOptions::default()
                },
            )
            .unwrap();
        assert_eq!(client.queued_bytes(), expected);
    }
    let mut client = ready();
    let id = client
        .post(
            0,
            RequestOptions {
                reserved_bytes: MAX_QUEUED_BYTES as f64,
                ..RequestOptions::default()
            },
        )
        .unwrap();
    assert!(client.post(0, RequestOptions::default()).is_err());
    assert!(client.reply(id, Ok(IvyJson::Null)).is_some());
    assert_eq!(client.queued_bytes(), 0);
}

fn apply_effects(client: &mut WorkerClient, effects: Effects, settled: &mut Vec<Value>) {
    for result in effects.settlements {
        settled.push(match result.result {
            Ok(value) => json!({"id":result.id.sequence,"value":value.to_serde().unwrap()}),
            Err(error) => {
                json!({"id":result.id.sequence,"error":error.to_wire().to_serde().unwrap()})
            }
        });
    }
    // Oracle has a fake terminate() that succeeds immediately. The dedicated
    // retirement test above covers the stronger approved process-owner fence.
    if let Some(generation) = effects.retire {
        assert!(client.retired(generation, true));
    }
}

#[test]
fn request_outcomes_and_accounting_match_unchanged_typescript_worker_client() {
    let trace = json!([
        {"op":"start"},
        {"op":"post","id":1,"timeout":10,"terminate":true,"bytes":4096,"backup":true},
        {"op":"post","id":2,"timeout":5,"terminate":false,"bytes":0},
        {"op":"started","id":1},{"op":"started","id":2},
        {"op":"advance","ms":4},{"op":"progress","id":1},{"op":"progress","id":2},
        {"op":"advance","ms":1},{"op":"reply","id":2,"value":{"ready":true}},
        {"op":"post","id":3,"timeout":4,"terminate":true,"bytes":8192},
        {"op":"advance","ms":4},{"op":"started","id":3},
        {"op":"progress","id":1},{"op":"advance","ms":3},
        {"op":"reply","id":3,"value":{"completed":true}},
        {"op":"post","id":4,"timeout":100,"terminate":true,"bytes":4096},
        {"op":"advance","ms":7},
        {"op":"start"},
        {"op":"post","id":5,"timeout":10,"terminate":true,"bytes":67108864},
        {"op":"post","id":6,"timeout":10,"terminate":true,"bytes":4096},
        {"op":"reply","id":5,"value":true}
    ]);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut oracle = Command::new("node")
        .arg("tests/fixtures/hive-worker-client-oracle.mjs")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    oracle
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&trace).unwrap().as_bytes())
        .unwrap();
    let output = oracle.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut client = WorkerClient::default();
    let mut requests = BTreeMap::<u64, RequestId>::new();
    let mut now = 0;
    let mut settled = Vec::new();
    let mut actual = Vec::new();
    for step in trace.as_array().unwrap() {
        let id = step["id"].as_u64().unwrap_or(0);
        match step["op"].as_str().unwrap() {
            "start" => {
                let generation = client.start(now).unwrap();
                assert!(client.started(generation));
            }
            "post" => {
                let options = RequestOptions {
                    timeout_ms: step["timeout"].as_u64().unwrap(),
                    terminate_on_timeout: step["terminate"].as_bool().unwrap(),
                    reserved_bytes: step["bytes"].as_f64().unwrap(),
                    backup: step["backup"].as_bool().unwrap_or(false),
                };
                match client.post(now, options) {
                    Ok(request) => {
                        assert_eq!(request.sequence, id);
                        requests.insert(id, request);
                    }
                    Err(error) => {
                        settled.push(json!({"id":id,"error":error.to_wire().to_serde().unwrap()}))
                    }
                }
            }
            "started" => client.request_started(requests[&id], now),
            "progress" => client.progress(requests[&id], now),
            "reply" => {
                if let Some(result) = client.reply(
                    requests[&id],
                    Ok(IvyJson::from_serde(&step["value"]).unwrap()),
                ) {
                    apply_effects(
                        &mut client,
                        Effects {
                            settlements: vec![result],
                            ..Effects::default()
                        },
                        &mut settled,
                    );
                }
            }
            "advance" => {
                let until = now + step["ms"].as_u64().unwrap();
                while let Some(deadline) = client.next_deadline().filter(|d| *d <= until) {
                    now = deadline;
                    let effects = client.expire(now);
                    apply_effects(&mut client, effects, &mut settled);
                }
                now = until;
            }
            _ => unreachable!(),
        }
        actual.push(json!({"ready":client.ready(),"count":client.pending_count(),"bytes":client.queued_bytes(),"settled":settled}));
    }
    assert_eq!(Value::Array(actual), expected);
}
