use ivy_contracts::{IvyError, IvyJson, Outcome};
#[path = "hive_admission.rs"]
mod admission;
#[path = "hive_dispatch_gate.rs"]
mod dispatch_gate;
#[path = "hive_worker_process.rs"]
mod worker_process;
use ivy_hive::worker_client::{Lifecycle, MAX_QUEUED_BYTES, RequestOptions, WorkerClient};

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
fn expired_queue_waits_are_settled_before_equal_running_deadline_retirement() {
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
fn native_deadlines_keep_zero_and_full_width_millisecond_durations() {
    for timeout in [0, 2_147_483_648, u64::MAX] {
        let mut client = ready();
        let id = client.post(7, options(timeout, false)).unwrap();
        assert_eq!(client.next_deadline(), Some(7u64.saturating_add(timeout)));
        client.request_started(id, 19);
        assert_eq!(client.next_deadline(), Some(19u64.saturating_add(timeout)));
    }
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
    assert!(client.expire(120_099).retire.is_none());
    let effects = client.expire(120_100);
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
fn byte_reservations_are_integer_bounded_and_released_on_completion() {
    for (reservation, expected) in [(0, 1024), (100, 1024), (1024, 1024), (4096, 4096)] {
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
    let mut too_large = ready();
    assert_eq!(
        too_large
            .post(
                0,
                RequestOptions {
                    reserved_bytes: u64::MAX,
                    ..RequestOptions::default()
                }
            )
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    assert_eq!(too_large.queued_bytes(), 0);
    let mut client = ready();
    let id = client
        .post(
            0,
            RequestOptions {
                reserved_bytes: MAX_QUEUED_BYTES,
                ..RequestOptions::default()
            },
        )
        .unwrap();
    assert!(client.post(0, RequestOptions::default()).is_err());
    assert!(client.reply(id, Ok(IvyJson::Null)).is_some());
    assert_eq!(client.queued_bytes(), 0);
}
