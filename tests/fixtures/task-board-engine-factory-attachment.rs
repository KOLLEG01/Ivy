use super::*;
use driver::{count, frames, reached};
use ivy_contracts::{json::canonical, limits::MANAGEMENT_FRAME_BYTES};

pub(super) fn stale(row: &Json) -> bool {
    f(f(row, "input"), "staleAttachment").as_bool() == Some(true)
}
fn retained_run(fixture: &Fixture) -> (String, u64, Json) {
    fixture
        .rows("task-board/run")
        .into_iter()
        .find(|row| row.0 == "run")
        .unwrap()
}
fn preserved_review(fixture: &Fixture, records: &[(&str, Json, &str)]) {
    let task = &fixture.rows("task-board/task")[0].2;
    for key in [
        "fields",
        "workflowState",
        "claim",
        "primaryResourceRef",
        "latestResult",
        "acceptedReview",
        "comments",
        "commentDeliveries",
        "workRevision",
        "attemptCount",
        "lastRun",
        "runtimeWait",
        "publication",
    ] {
        assert_eq!(
            hash_json(f(task, key)).unwrap(),
            hash_json(f(&records[3].1, key)).unwrap(),
            "Changed current review {key}"
        );
    }
    for (key, value, id) in &records[..2] {
        let retained = fixture
            .rows(key)
            .into_iter()
            .find(|row| row.0 == *id)
            .unwrap();
        assert_eq!(retained.1, 1);
        assert_eq!(hash_json(&retained.2).unwrap(), hash_json(value).unwrap());
    }
    assert_eq!(
        count(
            fixture,
            "SELECT count(*) FROM workspace_reservations WHERE task_id='task' AND operation_id='current-workspace'"
        ),
        1
    );
    assert!(fixture.rows("task-board/delivery").is_empty());
}
fn operation_bytes(fixture: &Fixture, parent: &str) -> Json {
    let manifest = driver::evidence(fixture)
        .into_iter()
        .find(|row| row.1 == parent)
        .unwrap()
        .2;
    let mut bytes = vec![];
    for part in f(&manifest, "chunks").as_array().unwrap() {
        let (key, owner, chunk): (String, String, Vec<u8>) = fixture
            .database()
            .query_row(
                "SELECT key,parent_id,bytes FROM blobs WHERE id=?",
                [f(f(part, "object"), "objectId").as_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(key, ChunkKey::NativeEvidence.as_str());
        assert_eq!(owner, parent);
        assert_eq!(f(part, "byteLength"), &Json::Number(chunk.len() as f64));
        assert_eq!(f(part, "contentHash"), &Json::from(digest(&chunk)));
        bytes.extend(chunk);
    }
    assert_eq!(
        f(&manifest, "byteLength"),
        &Json::Number(bytes.len() as f64)
    );
    assert_eq!(f(&manifest, "contentHash"), &Json::from(digest(&bytes)));
    let observed = json(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(
        canonical(&observed, MANAGEMENT_FRAME_BYTES)
            .unwrap()
            .as_bytes(),
        bytes
    );
    observed
}

#[tokio::test(flavor = "current_thread")]
async fn live_attachment_recovers_uncertain_resume_in_a_new_owner_before_terminal_publication() {
    for prior_attachment in [false, true] {
        let peer = Peer::start_mode(Some("--factory-observations")).await;
        let mut row = observation_row("cancelled retained terminal turn saves snapshot");
        let mut input = f(&row, "input").clone();
        input.insert("liveAttachment", true.into());
        input.insert("staleAttachment", prior_attachment.into());
        row.insert("input", input);
        let mut fixture = Fixture::new(false);
        fixture.page_held.release();
        fixture.observations = Some(Scenario {
            row: row.clone(),
            ready: Gate::default(),
            replay: RefCell::default(),
            response: RefCell::default(),
        });
        let scenario = fixture.observations.as_ref().unwrap();
        let contracts = ContractsBinding;
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let mut records = scenario.records(&catalogs);
        let factory = make_factory(&fixture, &contracts, &catalogs);
        let (service, owner) = make_service(configured(&peer), &contracts).unwrap();
        let script = async {
            service.start();
            let first = bounded(service.client.wait_ready(None)).await.unwrap();
            rpc(
                &first,
                "test.nativeObservations",
                Json::object([("row", row.clone()), ("unsupported", false.into())]),
            )
            .await
            .unwrap();
            scenario.ready.release();
            reached(
                &fixture,
                &peer,
                "retained uncertain live attachment",
                || {
                    (if prior_attachment {
                        fixture.rows("task-board/native-call").iter().any(|row| {
                            f(&row.2, "ownerPhase") == &"outcome_unknown".into()
                                && f(f(&retained_run(&fixture).2, "calls"), "thread")
                                    == &pin(&row.0, row.1 as f64)
                        })
                    } else {
                        !frames(&peer, "attachment-receipt-unavailable").is_empty()
                    }) && fixture
                        .rows("task-board/operation")
                        .iter()
                        .all(|row| f(&row.2, "phase") == &"succeeded".into())
                },
            )
            .await;
            let original = fixture
                .rows("task-board/native-call")
                .into_iter()
                .find(|row| row.0 != driver::ATTACHMENT)
                .unwrap();
            assert_eq!(
                f(&original.2, "ownerPhase"),
                &if prior_attachment {
                    "outcome_unknown".into()
                } else {
                    Json::Null
                }
            );
            assert_eq!(
                f(f(&retained_run(&fixture).2, "calls"), "thread"),
                &pin(&original.0, original.1 as f64)
            );
            assert_eq!(f(&retained_run(&fixture).2, "turnId"), &"turn".into());
            assert_eq!(f(&retained_run(&fixture).2, "nativeEpoch"), &"epoch".into());
            let origin = f(&original.2, "origin");
            assert_eq!(
                f(origin, "kind"),
                &Json::from(if prior_attachment {
                    "reattach"
                } else {
                    "initial"
                })
            );
            if prior_attachment {
                assert_eq!(f(origin, "predecessor"), &pin(driver::ATTACHMENT, 2.0));
                assert_eq!(f(origin, "preparedEpoch"), &"first-replacement".into());
            }
            let first_dispatch = frames(&peer, "attachment-invoke");
            assert_eq!(first_dispatch.len(), 1);
            let old_proof = if prior_attachment {
                let proof = operation_bytes(&fixture, &original.0);
                assert_eq!(
                    hash_json(&proof).unwrap(),
                    hash_json(f(&first_dispatch[0], "operation")).unwrap()
                );
                Some(proof)
            } else {
                None
            };
            if !prior_attachment {
                // The accepted initial attachment still belongs to the active
                // claim. After its lost receipt, preserve a newer reviewed task
                // while recovering only that already dispatched resume.
                let current = fixture.rows("task-board/task")[0].clone();
                let mut reviewed = current.2;
                reviewed.insert("claim", Json::Null);
                reviewed.insert("workflowState", "review".into());
                reviewed.insert("runtimeWait", Json::Null);
                reviewed.insert("lastRun", pin("newer-run", 1.0));
                reviewed.insert("attemptCount", Json::Number(2.0));
                admission("Task", &reviewed).unwrap();
                let database = fixture.database();
                ivy_task_board::local_journal::LocalJournal {
                    connection: &database,
                    root_object_id: None,
                }
                .update(
                    "task-board/task",
                    &"task".into(),
                    current.1 as f64,
                    &reviewed,
                    &clock().into(),
                )
                .unwrap();
                records[3].1 = reviewed;
            }
            preserved_review(&fixture, &records);
            rpc(
                &first,
                "test.close",
                Json::object(Vec::<(&str, Json)>::new()),
            )
            .await
            .unwrap();
            until(|| peer.methods(2).contains(&"registry.sync".into())).await;
            let second = bounded(service.client.wait_ready(None)).await.unwrap();
            assert_eq!(second.generation(), 2);
            reached(&fixture, &peer, "reattached terminal publication", || {
                operation(&fixture, "completion")
                    .is_some_and(|row| f(&row.2, "phase") == &"succeeded".into())
            })
            .await;
            let calls = fixture.rows("task-board/native-call");
            assert_eq!(calls.len(), if prior_attachment { 3 } else { 1 });
            let old = calls.iter().find(|row| row.0 == original.0).unwrap();
            if prior_attachment {
                assert_eq!(old.1, original.1);
                assert_eq!(hash_json(&old.2).unwrap(), hash_json(&original.2).unwrap());
            } else {
                for key in [
                    "operationId",
                    "preparedAt",
                    "origin",
                    "request",
                    "expectedDefinitionHash",
                ] {
                    assert_eq!(
                        hash_json(f(&old.2, key)).unwrap(),
                        hash_json(f(&original.2, key)).unwrap()
                    );
                }
            }
            let current = if prior_attachment {
                calls
                    .iter()
                    .find(|row| row.0 != original.0 && row.0 != driver::ATTACHMENT)
                    .unwrap()
            } else {
                old
            };
            assert_eq!(f(&current.2, "ownerPhase"), &"succeeded".into());
            let epoch = Json::from(if prior_attachment {
                "replacement-epoch"
            } else {
                "epoch"
            });
            assert_eq!(f(&current.2, "epoch"), &epoch);
            assert_eq!(
                f(f(&current.2, "origin"), "kind"),
                &Json::from(if prior_attachment {
                    "reattach"
                } else {
                    "initial"
                })
            );
            if prior_attachment {
                assert_eq!(
                    f(f(&current.2, "origin"), "predecessor"),
                    &pin(&original.0, original.1 as f64)
                );
                assert_eq!(f(f(&current.2, "origin"), "preparedEpoch"), &epoch);
            }
            for key in ["nativeVersion", "catalogSourceHash", "method"] {
                assert_eq!(
                    f(f(&current.2, "request"), key),
                    f(f(&original.2, "request"), key)
                );
            }
            let expected_dispatches = if prior_attachment { 2 } else { 1 };
            let invoked = frames(&peer, "attachment-invoke");
            assert_eq!(invoked.len(), expected_dispatches);
            let new_proof = operation_bytes(&fixture, &current.0);
            assert_eq!(
                hash_json(&new_proof).unwrap(),
                hash_json(f(invoked.last().unwrap(), "operation")).unwrap()
            );
            if let Some(old_proof) = old_proof {
                assert_eq!(f(&new_proof, "params"), f(&old_proof, "params"));
                assert_eq!(f(&new_proof, "requestHash"), f(&old_proof, "requestHash"));
                assert_eq!(operation_bytes(&fixture, &original.0), old_proof);
            }
            let run = retained_run(&fixture);
            assert_eq!(f(&run.2, "phase"), &"cancelled".into());
            assert_eq!(f(&run.2, "turnId"), &"turn".into());
            assert_eq!(f(&run.2, "nativeEpoch"), &epoch);
            assert!(
                f(&run.2, "calls")
                    .as_object()
                    .unwrap()
                    .iter()
                    .all(|(_, value)| value.is_null())
            );
            for key in ["cancellation", "primaryResourceRef", "originalRequest"] {
                assert_eq!(
                    hash_json(f(&run.2, key)).unwrap(),
                    hash_json(f(&records[2].1, key)).unwrap()
                );
            }
            let snapshots = children(&fixture, "task-board/native-turn-snapshot");
            assert_eq!(snapshots.len(), 1);
            assert_eq!(f(&snapshots[0].3, "epoch"), &epoch);
            assert_eq!(f(&snapshots[0].3, "status"), &"interrupted".into());
            let completion = operation(&fixture, "completion").unwrap();
            assert_eq!(
                f(f(&completion.2, "actor"), "generation"),
                &Json::Number(2.0)
            );
            assert_eq!(fixture.rows("task-board/result").len(), 2);
            assert_eq!(
                fixture.rows("task-board/history").len(),
                if prior_attachment { 3 } else { 2 }
            );
            preserved_review(&fixture, &records);
            let trace = peer.frames.lock().unwrap().clone();
            if prior_attachment {
                let dispatch = trace
                    .iter()
                    .position(|v| {
                        f(v, "generation") == &Json::Number(2.0)
                            && f(v, "kind") == &"attachment-invoke".into()
                    })
                    .unwrap();
                assert!(
                    trace[..dispatch]
                        .iter()
                        .any(|v| f(v, "generation") == &Json::Number(2.0)
                            && f(v, "kind") == &"attachment-observation".into()
                            && f(f(v, "observation"), "method") == &"thread/read".into())
                );
                assert!(
                    trace[..dispatch]
                        .iter()
                        .filter(|v| f(v, "generation") == &Json::Number(2.0)
                            && f(v, "kind") == &"attachment-observation".into())
                        .all(|v| f(f(v, "observation"), "method") == &"thread/read".into()),
                    "Recover the uncertain resume before inspecting or controlling its known turn"
                );
            }
            *scenario.replay.borrow_mut() = Some(f(&completion.2, "request").clone());
            let revisions = count(&fixture, "SELECT count(*) FROM revisions");
            let blobs = count(&fixture, "SELECT count(*) FROM blobs");
            let notices = fixture.notices.get();
            rpc(
                &second,
                "test.close",
                Json::object(Vec::<(&str, Json)>::new()),
            )
            .await
            .unwrap();
            until(|| scenario.response.borrow().is_some()).await;
            let third = bounded(service.client.wait_ready(None)).await.unwrap();
            assert_eq!(third.generation(), 3);
            let response = scenario.response.borrow_mut().take().unwrap();
            assert_eq!(
                hash_json(&bounded(response).await.unwrap()).unwrap(),
                hash_json(f(&completion.2, "outcome")).unwrap()
            );
            assert_eq!(count(&fixture, "SELECT count(*) FROM revisions"), revisions);
            assert_eq!(count(&fixture, "SELECT count(*) FROM blobs"), blobs);
            assert_eq!(fixture.notices.get(), notices);
            assert_eq!(
                frames(&peer, "attachment-invoke").len(),
                expected_dispatches
            );
            preserved_review(&fixture, &records);
            bounded(service.close()).await.unwrap();
        };
        let (result, ()) = tokio::join!(owner.run(&factory), script);
        result.unwrap();
        assert_eq!(fixture.drops.borrow().as_slice(), [1, 2, 3]);
        assert_eq!(count(&fixture, "SELECT count(*) FROM active_gates"), 0);
        peer.finish().await;
    }
}
