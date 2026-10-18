//! Scheduled cursor commits continue through two disconnected service boots.
use super::*;

fn committed(fixture: &Fixture) -> Vec<IvyJson> {
    let values = runs(fixture);
    assert!(
        values
            .iter()
            .all(|r| !["failed", "interrupted", "cancelled"]
                .iter()
                .any(|s| r.get("status") == Some(&(*s).into()))),
        "{values:?}"
    );
    values
        .into_iter()
        .filter(|r| r.get("executionCommitted") == Some(&true.into()))
        .collect()
}

async fn idle_commits(fixture: &Fixture, minimum: usize) -> Vec<IvyJson> {
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            let values = committed(fixture);
            let row = snapshot(fixture);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as f64;
            if values.len() >= minimum
                && values.len() == runs(fixture).len()
                && model::number(&row, "nextAt").unwrap() - now > 3500.0
            {
                return values;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap()
}

fn assert_cursor(fixture: &Fixture, values: &[IvyJson]) -> IvyJson {
    let expected = IvyJson::Array(
        (1..=values.len())
            .map(|cursor| {
                IvyJson::object([
                    ("cursor", IvyJson::Number(cursor as f64)),
                    (
                        "previous",
                        if cursor == 1 {
                            IvyJson::Null
                        } else {
                            IvyJson::Number((cursor - 1) as f64)
                        },
                    ),
                    ("input", IvyJson::Null),
                ])
            })
            .collect(),
    );
    assert!(json_equal(&markers(fixture), &expected));
    let mut ids = std::collections::BTreeSet::new();
    for (i, run) in values.iter().enumerate() {
        assert!(ids.insert(model::text(run, "id").unwrap()));
        assert_eq!(run.get("input"), Some(&IvyJson::Null));
        if run.get("status") == Some(&"succeeded".into()) {
            // Completed local records discard payloads; Hive history below
            // verifies their data against the same markers and run identities.
            assert_eq!(run.get("output"), Some(&IvyJson::Null));
        } else {
            assert!(json_equal(
                run.required("output").unwrap().required("data").unwrap(),
                &expected.as_array().unwrap()[i]
            ));
        }
    }
    let row = snapshot(fixture);
    assert_eq!(
        row.get("state"),
        Some(&IvyJson::object([(
            "cursor",
            IvyJson::Number(values.len() as f64)
        )]))
    );
    assert_eq!(row.get("lastRunId"), values.last().unwrap().get("id"));
    expected
}

fn assert_pending(values: &[IvyJson]) {
    assert!(!values.is_empty());
    assert!(
        values
            .iter()
            .all(|r| r.get("status") == Some(&"publishing".into())
                && r.get("executionCommitted") == Some(&true.into())
                && r.get("eventIndex") == Some(&IvyJson::Number(0.0)))
    );
}

async fn stop_periodic(fixture: &Fixture, child: &OwnedProcess, original: bool, phase: &str) {
    let health = fixture.health().unwrap();
    fixture.control(&health, true).await;
    let result = complete(child).await;
    assert!(
        result.exit_code == Some(0) && result.error_code.is_none() && result.stderr.is_empty(),
        "periodic original={original} phase={phase} helperPid={} boot={:?} result={result:?}",
        child.pid,
        health.get("bootId")
    );
}

async fn scenario(original: bool) {
    let Running {
        fixture,
        mut hive,
        mut output,
        client,
        mut child,
        initial_health,
    } = start(original).await;
    let saved = invoke(
        &client,
        SOURCE,
        IvyJson::object([
            ("action", "save".into()),
            ("expectedRevision", IvyJson::Number(0.0)),
            ("task", definition(&fixture, 6)),
        ]),
    )
    .await
    .unwrap();
    invoke(
        &client,
        SOURCE,
        activation(
            saved.required("jobId").unwrap(),
            &IvyJson::Number(0.0),
            SOURCE,
            true,
        ),
    )
    .await
    .unwrap();
    // No manual run or HTTP input is used: every worker is scheduler-admitted.
    let initial = idle_commits(&fixture, 1).await;
    wait_runs(&fixture, initial.len(), "succeeded").await;
    let result_id = snapshot(&fixture)
        .required("resultObjectId")
        .unwrap()
        .clone();
    hive_state(&mut hive, &mut output, "offline").await;
    let offline = fixture.wait_health(false).await;
    assert_eq!(offline.get("ready"), Some(&true.into()));
    assert_eq!(offline.get("generation"), Some(&IvyJson::Null));
    let baseline = committed(&fixture).len();
    let offline_start = snapshot(&fixture);
    let first_boot = idle_commits(&fixture, baseline + 2).await;
    assert_pending(&first_boot[baseline..]);
    assert_cursor(&fixture, &first_boot);
    let before_restart = snapshot(&fixture);
    assert!(
        model::number(&before_restart, "nextAt").unwrap()
            > model::number(&offline_start, "nextAt").unwrap()
    );
    stop_periodic(&fixture, &child, original, "offline restart").await;
    assert_eq!(runs(&fixture), first_boot);
    assert_eq!(snapshot(&fixture).get("state"), before_restart.get("state"));
    assert_eq!(
        snapshot(&fixture).get("nextAt"),
        before_restart.get("nextAt")
    );

    child = launch_service(&fixture, original).await;
    let restarted = ready(&fixture, &child, initial_health.get("bootId")).await;
    assert_eq!(restarted.get("generation"), Some(&IvyJson::Null));
    let second_boot = idle_commits(&fixture, first_boot.len() + 2).await;
    assert_pending(&second_boot[first_boot.len()..]);
    assert_eq!(&second_boot[..first_boot.len()], first_boot.as_slice());
    assert_cursor(&fixture, &second_boot);
    let before_reconnect = snapshot(&fixture);
    assert!(
        model::number(&before_reconnect, "nextAt").unwrap()
            > model::number(&before_restart, "nextAt").unwrap()
    );

    hive_state(&mut hive, &mut output, "online").await;
    online(&fixture, &child, None, &client, SOURCE).await;
    // Reconnect can span another interval. Quiesce just after a completed worker
    // through the real routed provider; elapsed time never determines a count.
    let settled = idle_commits(&fixture, second_boot.len()).await;
    let latest = snapshot(&fixture);
    assert!(
        model::number(&latest, "nextAt").unwrap()
            >= model::number(&before_reconnect, "nextAt").unwrap()
    );
    let disabled = invoke(
        &client,
        SOURCE,
        IvyJson::object([
            ("action", "disable".into()),
            ("id", TASK.into()),
            ("expectedRevision", IvyJson::Number(1.0)),
        ]),
    )
    .await
    .unwrap();
    assert!(disabled.get("assignment").is_some());
    let disabled_row = wait_row(&fixture, "periodic definition disabled", |r| {
        r.get("task").and_then(|t| t.get("enabled")) == Some(&false.into())
    })
    .await;
    assert_eq!(disabled_row.get("state"), latest.get("state"));
    // Adopting a changed enabled flag intentionally resets the next due time.
    // The older outbox must preserve that new schedule as it finishes publishing.
    assert!(
        model::number(&disabled_row, "nextAt").unwrap()
            >= model::number(&latest, "nextAt").unwrap()
    );
    let completed = wait_runs(&fixture, settled.len(), "succeeded").await;
    assert_eq!(
        completed
            .iter()
            .map(|r| r.required("id").unwrap())
            .collect::<Vec<_>>(),
        settled
            .iter()
            .map(|r| r.required("id").unwrap())
            .collect::<Vec<_>>()
    );
    assert!(
        completed
            .iter()
            .all(|r| r.get("eventIndex") == Some(&IvyJson::Number(2.0)))
    );
    let expected = assert_cursor(&fixture, &completed);
    let published = snapshot(&fixture);
    assert_eq!(published.get("resultObjectId"), Some(&result_id));
    assert_eq!(published.get("state"), latest.get("state"));
    assert_eq!(published.get("nextAt"), disabled_row.get("nextAt"));
    let timeline = verify_publication(
        &client,
        published.required("resultObjectId").unwrap(),
        &completed,
        &expected,
    )
    .await;
    assert_eq!(timeline.len(), completed.len() * 3);
    // Stay alive beyond the last due time, proving the adopted disable prevents
    // another scheduler admission after the outbox has drained.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64;
    let until_due = (model::number(&disabled_row, "nextAt").unwrap() - now).max(0.0) as u64;
    tokio::time::sleep(Duration::from_millis(until_due + 1200)).await;
    assert_eq!(runs(&fixture), completed);
    assert!(json_equal(&markers(&fixture), &expected));
    stop_periodic(&fixture, &child, original, "disabled final stop").await;
    hive.stdin
        .take()
        .unwrap()
        .write_all(b"close\n")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(12), hive.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn isolated_services_continue_periodic_cursor_across_two_offline_boots() {
    tokio::time::timeout(Duration::from_secs(240), async {
        scenario(true).await;
        scenario(false).await;
    })
    .await
    .unwrap();
}
