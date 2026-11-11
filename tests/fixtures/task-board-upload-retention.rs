use super::*;

fn upload_request(id: &str, body: String) -> Json {
    Json::object([
        ("action", "uploadAttachment".into()),
        ("operationId", id.into()),
        ("taskId", "upload-task".into()),
        ("expectedRevision", 1.0.into()),
        ("attachmentId", "file".into()),
        ("filename", "file.bin".into()),
        ("mediaType", "application/octet-stream".into()),
        ("bytesBase64", body.into()),
    ])
}
fn upload_actor() -> Json {
    Json::object([
        ("principalId", "upload-user".into()),
        ("serviceNodeId", "task-board".into()),
        ("generation", 1.0.into()),
        ("source", "user".into()),
    ])
}
fn upload_outcome(request: &Json, actor: &Json) -> Json {
    let bytes = ivy_host_runtime::base64_content::Base64Content::inspect(
        request
            .required("bytesBase64")
            .unwrap()
            .as_js_string()
            .unwrap(),
        8 * 1024 * 1024,
    )
    .unwrap();
    let mut attachment = Json::object([
        (
            "object",
            Json::object([
                ("objectId", "published-file".into()),
                ("revision", 1.0.into()),
            ]),
        ),
        ("byteLength", Json::Number(bytes.byte_length() as f64)),
        ("contentHash", bytes.content_hash().into()),
        ("uploader", actor.clone()),
        ("createdAt", "2026-11-09T12:00:00.000Z".into()),
    ]);
    for key in ["attachmentId", "filename", "mediaType"] {
        attachment.insert(key, request.required(key).unwrap().clone());
    }
    Json::object([
        (
            "operationId",
            request.required("operationId").unwrap().clone(),
        ),
        ("task", Json::Null),
        ("plan", Json::Null),
        ("run", Json::Null),
        ("history", Json::Null),
        ("result", Json::Null),
        ("review", Json::Null),
        ("attachment", attachment),
    ])
}

#[tokio::test(flavor = "current_thread")]
async fn completed_upload_retains_compact_evidence_and_original_replay_after_restart() {
    let path =
        PathBuf::from(std::env::var_os("IVY_TEST_TEMP").unwrap()).join("completed-upload.sqlite");
    assert!(!path.exists());
    let current = DurableAdmission::new(bundle());
    let retained = Retained { schemas: bundle() };
    let contracts = DurableContracts {
        retained: &retained,
        current: &current,
    };
    let client = Arc::new(Client {
        epoch: "upload-epoch".into(),
        calls: Mutex::default(),
    });
    let counter = std::cell::Cell::new(0);
    let request = upload_request("large-upload", "A".repeat(11_184_811) + "=");
    let actor = upload_actor();
    let outcome = upload_outcome(&request, &actor);
    let request_hash = hash_json(&request).unwrap();
    let saved_id;
    macro_rules! bound_upload {
        ($db:expr) => {
            Operations {
                store: JournalStore {
                    journal: LocalJournal {
                        connection: $db,
                        root_object_id: Some("root".into()),
                    },
                    client: client.clone(),
                    contracts: &contracts,
                    limits: HistoryLimits::default(),
                    now: || "2026-11-09T12:00:00.000Z".into(),
                    new_id: || {
                        let n = counter.get();
                        counter.set(n + 1);
                        format!("upload-{n}").into()
                    },
                    cache: None,
                },
                now: || "2026-11-09T12:00:00.000Z".into(),
            }
        };
    }
    {
        let db = Connection::open(&path).unwrap();
        initialize(&db).unwrap();
        let ops = bound_upload!(&db);
        current.request(&request).unwrap();
        let accepted = complete(ops.begin(&request, &actor)).unwrap();
        saved_id = accepted.pin.object_id.clone();
        let accepted_hash = accepted.content_hash.clone();
        let mut preparing = accepted.value;
        let mut entry = Json::object([
            ("mutationId", "original-publication".into()),
            ("contractKey", "task-board/history".into()),
            ("contractVersion", "1.0.0".into()),
            (
                "payload",
                Json::object([
                    (
                        "object",
                        Json::object([
                            ("objectId", "prepared-history".into()),
                            ("revision", 1.0.into()),
                        ]),
                    ),
                    ("label", "Prepared history".into()),
                    ("mediaType", "application/json".into()),
                    ("contentHash", hash_json(&Json::Null).unwrap().into()),
                ]),
            ),
            (
                "destination",
                Json::object([(
                    "create",
                    Json::object([("parentId", "root".into()), ("name", "History".into())]),
                )]),
            ),
            ("outcome", Json::Null),
        ]);
        preparing.insert("writes", Json::Array(vec![entry.clone()]));
        preparing.insert("phase", "preparing".into());
        let mut pin = complete(ops.store.write(
            "task-board/operation",
            &preparing,
            "prepare-upload",
            &Json::object([
                ("objectId", Json::String(saved_id.clone())),
                ("expectedRevision", 1.0.into()),
            ]),
        ))
        .unwrap();
        assert_eq!(
            complete(ops.finish(&saved_id, &outcome)).unwrap_err().code,
            "task_board_publication_pending"
        );
        entry.insert(
            "outcome",
            Json::object([
                ("objectId", "published-history".into()),
                ("revision", 1.0.into()),
            ]),
        );
        preparing.insert("writes", Json::Array(vec![entry]));
        pin = complete(ops.store.write(
            "task-board/operation",
            &preparing,
            "receipt-upload",
            &Json::object([
                ("objectId", Json::String(saved_id.clone())),
                (
                    "expectedRevision",
                    pin.required("revision").unwrap().clone(),
                ),
            ]),
        ))
        .unwrap();
        drop(preparing);
        let mut wrong = outcome.clone();
        wrong
            .get_mut("attachment")
            .unwrap()
            .insert("attachmentId", "other".into());
        assert_eq!(
            complete(ops.finish(&saved_id, &wrong)).unwrap_err().code,
            "task_board_evidence_mismatch"
        );
        same(
            &complete(ops.finish(&saved_id, &outcome)).unwrap(),
            &outcome,
            "saved final outcome",
        );
        let saved = complete(ops.find(&"upload-user".into(), &"large-upload".into()))
            .unwrap()
            .unwrap();
        current.operation(&saved.value).unwrap();
        let compact = saved.value.required("request").unwrap();
        assert!(compact.get("bytesBase64").is_none());
        assert!(canonical(compact, MANAGEMENT_FRAME_BYTES).unwrap().len() < 1024);
        assert_eq!(
            compact
                .required("content")
                .unwrap()
                .required("object")
                .unwrap(),
            outcome
                .required("attachment")
                .unwrap()
                .required("object")
                .unwrap()
        );
        assert_eq!(
            saved.value.required("requestHash").unwrap(),
            &Json::from(request_hash.clone())
        );
        assert!(
            current.request(compact).is_err(),
            "Evidence is never an admitted upload request"
        );
        for phase in [
            "accepted",
            "preparing",
            "publishing",
            "needs_attention",
            "failed",
        ] {
            let mut invalid = saved.value.clone();
            invalid.insert("phase", phase.into());
            assert!(
                current.operation(&invalid).is_err(),
                "Compact request cannot authorize {phase}"
            );
        }
        let mut invalid = saved.value.clone();
        let Json::Array(writes) = invalid.get_mut("writes").unwrap() else {
            panic!("Expected writes")
        };
        writes[0].insert("outcome", Json::Null);
        assert!(current.operation(&invalid).is_err());
        let mut invalid = saved.value.clone();
        invalid
            .get_mut("outcome")
            .unwrap()
            .insert("attachment", Json::Null);
        assert!(current.operation(&invalid).is_err());
        let receipt = ivy_task_board::journal_store::persistent_reads::operation_receipt::operation_receipt::<IvyError>(&saved.value).unwrap();
        current.receipt(&receipt).unwrap();
        assert_eq!(
            receipt.required("requestHash").unwrap(),
            &Json::from(request_hash.clone())
        );
        // Historical pins remain exact; compaction cannot rewrite or delete the
        // accepted request or any intermediate evidence held by another record.
        let historical = complete(ops.store.read(
            "task-board/operation",
            &Json::object([
                ("objectId", Json::String(saved_id.clone())),
                ("revision", 1.0.into()),
            ]),
            false,
        ))
        .unwrap();
        assert_eq!(historical.content_hash, accepted_hash);
        same(
            historical.value.required("request").unwrap(),
            &request,
            "immutable original body",
        );
        assert_eq!(revisions(&db), 4);
        assert_eq!(pin.required("revision").unwrap(), &Json::from(3.0));
    }
    {
        let db = Connection::open(&path).unwrap();
        let ops = bound_upload!(&db);
        let before = revisions(&db);
        let replay = complete(ops.begin(&request, &actor)).unwrap();
        assert_eq!(replay.pin.object_id, saved_id);
        assert!(
            replay
                .value
                .required("request")
                .unwrap()
                .get("bytesBase64")
                .is_none()
        );
        same(
            &complete(ops.finish(&saved_id, &outcome)).unwrap(),
            &outcome,
            "restart original outcome",
        );
        let mut changed = request.clone();
        changed.insert("filename", "other.bin".into());
        assert_eq!(
            complete(ops.begin(&changed, &actor)).unwrap_err().code,
            "mutation_conflict"
        );
        let mut changed = request.clone();
        changed.insert("bytesBase64", "AQI=".into());
        assert_eq!(
            complete(ops.begin(&changed, &actor)).unwrap_err().code,
            "mutation_conflict"
        );
        assert!(
            complete(ops.find(&"other-user".into(), &"large-upload".into()))
                .unwrap()
                .is_none()
        );
        let mut changed = outcome.clone();
        changed.insert(
            "task",
            Json::object([("objectId", "different".into()), ("revision", 1.0.into())]),
        );
        assert_eq!(
            complete(ops.finish(&saved_id, &changed)).unwrap_err().code,
            "mutation_conflict"
        );
        assert_eq!(
            before,
            revisions(&db),
            "Replay and rejected replacements create no revision"
        );
    }
    assert!(
        client.calls.lock().unwrap().is_empty(),
        "No remote or native effect is replayed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unresolved_and_failed_uploads_keep_their_original_recovery_body() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let current = DurableAdmission::new(bundle());
    let retained = Retained { schemas: bundle() };
    let contracts = DurableContracts {
        retained: &retained,
        current: &current,
    };
    let client = Arc::new(Client {
        epoch: "upload-epoch".into(),
        calls: Mutex::default(),
    });
    let counter = std::cell::Cell::new(0);
    let ops = Operations {
        store: JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: &contracts,
            limits: HistoryLimits::default(),
            now: || "2026-11-09T12:00:00.000Z".into(),
            new_id: || {
                let n = counter.get();
                counter.set(n + 1);
                format!("pending-{n}").into()
            },
            cache: None,
        },
        now: || "2026-11-09T12:00:00.000Z".into(),
    };
    let actor = upload_actor();
    for (name, unknown, attention) in [
        ("unknown", true, true),
        ("failed-unknown", true, false),
        ("rejected", false, false),
    ] {
        let request = upload_request(name, "AQI=".into());
        let saved = complete(ops.begin(&request, &actor)).unwrap();
        let mut error = IvyError::new("upload_unavailable", "Preserve original upload");
        if unknown {
            error = error.with_outcome(Outcome::Unknown);
        }
        let failed = complete(ops.failure(&saved.pin.object_id, &error, attention)).unwrap();
        assert!(
            failed
                .value
                .required("request")
                .unwrap()
                .get("bytesBase64")
                .is_none()
        );
        same(
            &complete(ivy_task_board::operations::original_upload_request(
                &ops.store, &failed,
            ))
            .unwrap()
            .unwrap(),
            &request,
            name,
        );
        current.operation(&failed.value).unwrap();
        assert_eq!(
            failed.value.required("requestHash").unwrap(),
            &Json::from(hash_json(&request).unwrap())
        );
        let replay = complete(ops.begin(&request, &actor)).unwrap();
        assert_eq!(replay.pin, failed.pin);
        same(
            &complete(ivy_task_board::operations::original_upload_request(
                &ops.store, &replay,
            ))
            .unwrap()
            .unwrap(),
            &request,
            "unresolved replay original",
        );
        assert!(
            current
                .request(replay.value.required("request").unwrap())
                .is_err()
        );
        for changed in [
            "operation",
            "revision",
            "caller",
            "actor",
            "hash",
            "metadata",
        ] {
            let mut invalid = replay.clone();
            match changed {
                "operation" => {
                    invalid
                        .value
                        .get_mut("request")
                        .unwrap()
                        .get_mut("originalRequest")
                        .unwrap()
                        .insert("objectId", "other-operation".into());
                }
                "revision" => {
                    invalid
                        .value
                        .get_mut("request")
                        .unwrap()
                        .get_mut("originalRequest")
                        .unwrap()
                        .insert("revision", Json::Number(2.0));
                }
                "caller" => {
                    invalid
                        .value
                        .insert("callerPrincipalId", "other-user".into());
                }
                "actor" => {
                    invalid
                        .value
                        .get_mut("actor")
                        .unwrap()
                        .insert("principalId", "other-user".into());
                }
                "hash" => {
                    invalid
                        .value
                        .insert("requestHash", hash_json(&Json::Null).unwrap().into());
                }
                _ => {
                    invalid
                        .value
                        .get_mut("request")
                        .unwrap()
                        .insert("filename", "different.bin".into());
                }
            }
            let error = complete(ivy_task_board::operations::original_upload_request(
                &ops.store, &invalid,
            ))
            .unwrap_err();
            assert_eq!(error.code, "task_board_evidence_mismatch", "{changed}");
            assert_eq!(error.outcome, Outcome::Unknown);
        }
        let id = replay.pin.object_id.to_utf8().unwrap();
        db.execute("UPDATE revisions SET value_json=json_set(value_json,'$.request.bytesBase64','AAE=') WHERE id=? AND revision=1", [&id]).unwrap();
        assert_eq!(
            complete(ivy_task_board::operations::original_upload_request(
                &ops.store, &replay
            ))
            .unwrap_err()
            .outcome,
            Outcome::Unknown,
            "Changed original bytes cannot be replayed"
        );
        db.execute("DELETE FROM revisions WHERE id=? AND revision=1", [&id])
            .unwrap();
        assert_eq!(
            complete(ivy_task_board::operations::original_upload_request(
                &ops.store, &replay
            ))
            .unwrap_err()
            .outcome,
            Outcome::Unknown,
            "Missing evidence cannot turn accepted work into not executed"
        );
    }
    assert!(client.calls.lock().unwrap().is_empty());
}

fn upload_history(actor: &Json) -> Json {
    let mut history = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", "upload-task".into()),
        ("operationId", "staged-upload".into()),
        ("actor", actor.clone()),
        ("createdAt", "2026-11-09T12:00:00.000Z".into()),
        ("kind", "edited".into()),
        ("detail", "Retained upload publication".into()),
    ]);
    for key in [
        "previousTask",
        "previousHistory",
        "fromTarget",
        "toTarget",
        "priorRun",
        "result",
        "review",
    ] {
        history.insert(key, Json::Null);
    }
    history
}

#[tokio::test(flavor = "current_thread")]
async fn staged_eight_mib_upload_keeps_one_original_and_resolves_after_restart() {
    let path =
        PathBuf::from(std::env::var_os("IVY_TEST_TEMP").unwrap()).join("staged-upload.sqlite");
    assert!(!path.exists());
    let current = DurableAdmission::new(bundle());
    let retained = Retained { schemas: bundle() };
    let contracts = DurableContracts {
        retained: &retained,
        current: &current,
    };
    let client = Arc::new(Client {
        epoch: "upload-epoch".into(),
        calls: Mutex::default(),
    });
    let counter = std::cell::Cell::new(0);
    let actor = upload_actor();
    let request = upload_request("staged-upload", "A".repeat(11_184_811) + "=");
    let outcome = upload_outcome(&request, &actor);
    let saved_id;
    let accepted_hash;
    macro_rules! staged_ops {
        ($db:expr) => {
            Operations {
                store: JournalStore {
                    journal: LocalJournal {
                        connection: $db,
                        root_object_id: Some("root".into()),
                    },
                    client: client.clone(),
                    contracts: &contracts,
                    limits: HistoryLimits::default(),
                    now: || "2026-11-09T12:00:00.000Z".into(),
                    new_id: || {
                        let n = counter.get();
                        counter.set(n + 1);
                        format!("staged-{n}").into()
                    },
                    cache: None,
                },
                now: || "2026-11-09T12:00:00.000Z".into(),
            }
        };
    }
    {
        let db = Connection::open(&path).unwrap();
        initialize(&db).unwrap();
        let ops = staged_ops!(&db);
        let accepted = complete(ops.begin_owned(request.clone(), &actor)).unwrap();
        saved_id = accepted.pin.object_id.clone();
        accepted_hash = accepted.content_hash;
        let destination = Json::object([(
            "create",
            Json::object([("parentId", "root".into()), ("name", "History".into())]),
        )]);
        complete(ops.stage(
            &saved_id,
            0.0,
            "task-board/history",
            &destination,
            &upload_history(&actor),
        ))
        .unwrap();
        assert_eq!(
            complete(ops.finish(&saved_id, &outcome)).unwrap_err().code,
            "task_board_publication_pending"
        );
        complete(ops.apply(&saved_id, 0)).unwrap();
        let pending = complete(
            ops.failure(
                &saved_id,
                &IvyError::new("reply_lost", "Original outcome must be reconciled")
                    .with_outcome(Outcome::Unknown),
                true,
            ),
        )
        .unwrap();
        current.operation(&pending.value).unwrap();
        let metadata = pending.value.required("request").unwrap();
        assert!(canonical(metadata, MANAGEMENT_FRAME_BYTES).unwrap().len() < 1024);
        assert!(metadata.get("bytesBase64").is_none());
        assert_eq!(
            metadata
                .required("originalRequest")
                .unwrap()
                .required("objectId")
                .unwrap(),
            &Json::String(saved_id.clone())
        );
        assert!(current.request(metadata).is_err());
        let id = saved_id.to_utf8().unwrap();
        let full_bodies: i64 = db.query_row("SELECT count(*) FROM revisions WHERE id=? AND json_type(value_json,'$.request.bytesBase64')='text'", [&id], |r| r.get(0)).unwrap();
        assert_eq!(
            full_bodies, 1,
            "Only the immutable acceptance retains the8MiB body"
        );
        let bytes: i64 = db
            .query_row(
                "SELECT sum(length(value_json)) FROM revisions WHERE id=?",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            bytes < 11_184_812 + 16_384,
            "Intermediate revisions must remain small: {bytes}"
        );
    }
    {
        let db = Connection::open(&path).unwrap();
        let ops = staged_ops!(&db);
        let replay = complete(ops.begin(&request, &actor)).unwrap();
        assert_eq!(replay.pin.object_id, saved_id);
        let before = revisions(&db);
        same(
            &complete(ivy_task_board::operations::original_upload_request(
                &ops.store, &replay,
            ))
            .unwrap()
            .unwrap(),
            &request,
            "Restart materializes the exact original request",
        );
        assert_eq!(before, revisions(&db));
        same(
            &complete(ops.finish(&saved_id, &outcome)).unwrap(),
            &outcome,
            "Original completion",
        );
        let saved = complete(ops.find(&"upload-user".into(), &"staged-upload".into()))
            .unwrap()
            .unwrap();
        current.operation(&saved.value).unwrap();
        assert!(
            saved
                .value
                .required("request")
                .unwrap()
                .get("originalRequest")
                .is_none()
        );
        assert!(
            saved
                .value
                .required("request")
                .unwrap()
                .get("content")
                .is_some()
        );
        let original = complete(ops.store.read(
            "task-board/operation",
            &Json::object([
                ("objectId", Json::String(saved_id)),
                ("revision", Json::Number(1.0)),
            ]),
            false,
        ))
        .unwrap();
        assert_eq!(original.content_hash, accepted_hash);
        same(
            original.value.required("request").unwrap(),
            &request,
            "Accepted body remains immutable",
        );
    }
    same(
        &Json::Array(client.calls.lock().unwrap().clone()),
        &Json::Array(vec![Json::Array(vec![
            "system.status".into(),
            Json::object([] as [(&str, Json); 0]),
        ])]),
        "Only staging reads the runtime epoch; restart and replay add no calls",
    );
}
