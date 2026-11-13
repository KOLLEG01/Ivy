use super::*;
use ivy_contracts::{
    chat_schema::validate_chat,
    json::{parse, stringify},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicI64, Ordering},
    time::Instant,
};

fn json(text: &str) -> Json {
    parse(text, usize::MAX).unwrap()
}
fn file(bytes: &[u8], mime: &str) -> Json {
    Json::object([
        ("filename", "Grüße.txt".into()),
        ("mediaType", mime.into()),
        ("byteLength", (bytes.len() as f64).into()),
        ("contentHash", digest(bytes).into()),
        ("bytesBase64", STANDARD.encode(bytes).into()),
    ])
}
fn config() -> Json {
    json(
        r#"{"accountId":"account","botPhoneNumber":"+15555550100","senders":[{"phoneNumber":"+15555550200","principalId":"alice","channelId":"main"}]}"#,
    )
}
fn workspace() -> Json {
    json(
        r#"{"expectedBridge":{"principalId":"chat","callerPrincipalId":"alice","workspaceId":"main","definitionHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"binding":{"object":{"objectId":"binding","revision":1}},"channels":[{"channel":{"accountId":"account","adapter":"whatsapp","channelId":"main"}}]}"#,
    )
}
fn request(id: &str, target: &Json) -> Json {
    Json::object([
        ("action", "sendFile".into()),
        ("operationId", id.into()),
        (
            "expectedBridge",
            target.required("expectedBridge").unwrap().clone(),
        ),
        (
            "expectedBinding",
            target.required("expectedBinding").unwrap().clone(),
        ),
        (
            "file",
            file("ä\r\nExact original bytes\n".as_bytes(), "text/plain"),
        ),
        ("caption", Json::Null),
    ])
}
fn request_bytes(request: &Json) -> Vec<u8> {
    outgoing_file_bytes(request.required("file").unwrap())
        .unwrap()
        .0
}
fn count(files: &ChatFiles, table: &str) -> i64 {
    files
        .journal
        .access(|j| {
            Ok(j.connection()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap())
        })
        .unwrap()
}
struct Fixed(AtomicI64);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-chat-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn open(&self, clock: Arc<dyn Clock>) -> ChatFiles {
        ChatFiles::with_clock(
            Journal::new(JournalIo::open(self.0.join("journal.sqlite")).unwrap()),
            config(),
            clock,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn exact_content_mime_and_native_image_bounds() {
    let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jN1kAAAAASUVORK5CYII=").unwrap();
    assert!(outgoing_file_bytes(&file(&png, "image/png")).unwrap().1);
    for (bytes, mime) in [
        (
            &b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"[..],
            "image/svg+xml",
        ),
        (&b"{\"count\":1}"[..], "application/json"),
        (&b"\x00\xff\x01"[..], "application/octet-stream"),
    ] {
        assert!(!outgoing_file_bytes(&file(bytes, mime)).unwrap().1);
    }
    let gif = STANDARD
        .decode("R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==")
        .unwrap();
    assert!(!outgoing_file_bytes(&file(&gif, "image/gif")).unwrap().1);
    for changed in [
        file(&png, "text/plain"),
        file(b"plain text", "image/png"),
        file(b"\x00\xff\x01", "text/plain"),
        file(b"{broken", "application/json"),
    ] {
        assert!(outgoing_file_bytes(&changed).is_err());
    }
    let mut changed = file(b"ok", "text/plain");
    changed.insert("bytesBase64", "b2s=\n".into());
    assert!(outgoing_file_bytes(&changed).is_err());
    for encoded in ["b2s", "b2t=", "b2s=====", "b2s=b2s=", "b2s\u{100}"] {
        changed.insert("bytesBase64", encoded.into());
        assert!(outgoing_file_bytes(&changed).is_err(), "{encoded}");
    }
    let mut padded_chunk = "b2s=".repeat(4096);
    padded_chunk.push_str("b2s=");
    changed.insert("bytesBase64", padded_chunk.into());
    assert!(outgoing_file_bytes(&changed).is_err());
    for key in ["byteLength", "contentHash"] {
        let mut changed = file(b"ok", "text/plain");
        changed.insert(
            key,
            if key == "byteLength" {
                1.0.into()
            } else {
                digest(b"other").into()
            },
        );
        assert_eq!(
            outgoing_file_bytes(&changed).unwrap_err().code,
            "chat_file_integrity"
        );
    }
    let mut large = png;
    large.resize(4 * 1024 * 1024 + 1, 0);
    assert_eq!(
        outgoing_file_bytes(&file(&large, "image/png"))
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    for name in ["../other", "folder\\other", "line\nfeed"] {
        let mut metadata = file(b"ok", "text/plain");
        metadata.remove("bytesBase64");
        metadata.insert("filename", name.into());
        assert!(validate_chat("OutgoingFile", &metadata).is_err());
    }
}

#[tokio::test]
async fn durable_order_concurrent_replay_receipts_and_expiry() {
    let fixture = Fixture::new();
    let clock = Arc::new(Fixed(AtomicI64::new(1_792_800_000_000)));
    let files = fixture.open(clock.clone());
    let target = files.target(&workspace()).unwrap();
    validate_chat("FileTarget", &target).unwrap();
    let request = request("original", &target);
    validate_chat("SendFileRequest", &request).unwrap();
    let hash = hash_json(&request).unwrap();
    let caller: JsString = "alice".into();
    let (first, second) = tokio::join!(
        files.enqueue(
            &caller,
            &request,
            &target,
            &hash,
            request_bytes(&request),
            false
        ),
        files.enqueue(
            &caller,
            &request,
            &target,
            &hash,
            request_bytes(&request),
            false
        )
    );
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    validate_chat("FileDelivery", &first).unwrap();
    assert!(
        !stringify(&first, usize::MAX)
            .unwrap()
            .contains("bytesBase64")
    );
    assert_eq!(
        files.journal.access(|j| Ok(j.outgoing()?.len())).unwrap(),
        1
    );
    let id = first.required("messageId").unwrap().as_str().unwrap();
    files
        .journal
        .access(|j| {
            j.enqueue(
                &"after".into(),
                "15555550200@s.whatsapp.net",
                &"Following answer".into(),
                false,
                clock.utc_millis(),
            )?;
            Ok(())
        })
        .unwrap();
    let rows = files.journal.access(|j| j.outgoing()).unwrap();
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[1].text, "Following answer");
    files.journal.flush().await.unwrap();
    files.journal.close().await.unwrap();
    let files = fixture.open(clock.clone());
    assert_eq!(
        files.payload(&id).unwrap().bytes,
        "ä\r\nExact original bytes\n".as_bytes()
    );
    assert_eq!(
        files
            .read_operation(&"bob".into(), &"original".into())
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        files
            .replay(&files.saved(&id).unwrap(), &digest(b"changed"))
            .await
            .unwrap_err()
            .code,
        "chat_identity_conflict"
    );
    let mut replacement = target.clone();
    replacement.insert(
        "expectedBinding",
        json(r#"{"objectId":"other","revision":1}"#),
    );
    assert_eq!(
        files.admitted(&id, &replacement).unwrap_err().code,
        "chat_file_target_changed"
    );
    files.mark(&id, OutputState::Sending, None).unwrap();
    files
        .mark(&id, OutputState::Unknown, Some("whatsapp_delivery_unknown"))
        .unwrap();
    assert_eq!(
        files
            .read_operation(&"alice".into(), &"original".into())
            .await
            .unwrap()
            .get("state"),
        Some(&"outcome_unknown".into())
    );
    files.mark(&id, OutputState::Sent, None).unwrap();
    files.mark(&id, OutputState::Unknown, None).unwrap();
    assert_eq!(
        files
            .read_operation(&"alice".into(), &"original".into())
            .await
            .unwrap()
            .get("state"),
        Some(&"sent".into())
    );
    assert!(files.saved(&id).unwrap().get("bytesBase64").is_none());
    assert_eq!(count(&files, "file_bytes"), 0);
    clock.0.fetch_add(8 * 86_400_000, Ordering::SeqCst);
    files.prune().unwrap();
    files.journal.flush().await.unwrap();
    files.journal.close().await.unwrap();
    let files = fixture.open(clock);
    assert!(files.has(&"alice".into(), &"original".into()).unwrap());
    assert_eq!(
        files
            .read_operation(&"alice".into(), &"original".into())
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert!(
        files
            .journal
            .get(&format!("out-file:{id}"))
            .unwrap()
            .is_null()
    );
    files.journal.close().await.unwrap();
}

#[tokio::test]
async fn bounded_queue_and_transport_identity() {
    let fixture = Fixture::new();
    let files = fixture.open(Arc::new(SystemClock));
    let target = files.target(&workspace()).unwrap();
    for index in 0..32 {
        let request = request(&format!("file-{index}"), &target);
        files
            .enqueue(
                &"alice".into(),
                &request,
                &target,
                &hash_json(&request).unwrap(),
                request_bytes(&request),
                false,
            )
            .await
            .unwrap();
    }
    let request = request("overflow", &target);
    assert_eq!(
        files
            .enqueue(
                &"alice".into(),
                &request,
                &target,
                &hash_json(&request).unwrap(),
                request_bytes(&request),
                false
            )
            .await
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    let id = message_id(&"alice".into(), &"file-0".into()).unwrap();
    files.mark(&id, OutputState::Sent, None).unwrap();
    files
        .enqueue(
            &"alice".into(),
            &request,
            &target,
            &hash_json(&request).unwrap(),
            request_bytes(&request),
            false,
        )
        .await
        .unwrap();
    let mut changed_config = config();
    changed_config.insert("botPhoneNumber", "+15555550300".into());
    let changed = ChatFiles::new(files.journal.clone(), changed_config);
    let id = message_id(&"alice".into(), &"overflow".into()).unwrap();
    assert_eq!(
        changed.admitted(&id, &target).unwrap_err().code,
        "chat_file_target_changed"
    );
    files
        .journal
        .access(|j| {
            Ok(j.write(
                "UPDATE file_bytes SET byte_length=?,content=? WHERE id=?",
                vec![
                    Value::Integer(7),
                    Value::Blob(b"changed".to_vec()),
                    Value::Text(id.clone()),
                ],
            )?)
        })
        .unwrap();
    assert_eq!(
        files.payload(&id).err().unwrap().code,
        "chat_file_integrity"
    );
    files.journal.close().await.unwrap();
}

#[tokio::test]
async fn large_originals_reopen_and_metadata_queries_never_read_content() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let fixture = Fixture::new();
    let clock = Arc::new(Fixed(AtomicI64::new(1_792_800_000_000)));
    let files = fixture.open(clock.clone());
    let target = files.target(&workspace()).unwrap();
    let mut original = request("large-document", &target);
    let bytes = vec![b'x'; MAX_BYTES];
    original.insert("file", file(&bytes, "text/plain"));
    let hash = hash_json(&original).unwrap();
    let delivery = files
        .enqueue(
            &"alice".into(),
            &original,
            &target,
            &hash,
            request_bytes(&original),
            false,
        )
        .await
        .unwrap();
    let document_id = delivery.required("messageId").unwrap().as_str().unwrap();
    let mut too_large = bytes.clone();
    too_large.push(b'x');
    assert_eq!(
        outgoing_file_bytes(&file(&too_large, "text/plain"))
            .unwrap_err()
            .code,
        "chat_file_integrity"
    );
    drop(too_large);
    // The 64 MiB queue budget includes original BLOB lengths, without decoding
    // earlier operations again. Fill the remaining seven document slots.
    for index in 1..8 {
        original.insert("operationId", format!("large-document-{index}").into());
        files
            .enqueue(
                &"alice".into(),
                &original,
                &target,
                &hash_json(&original).unwrap(),
                bytes.clone(),
                false,
            )
            .await
            .unwrap();
    }
    files.journal.close().await.unwrap();
    let files = fixture.open(clock.clone());
    assert_eq!(files.payload(&document_id).unwrap().bytes, bytes);
    files
        .journal
        .access(|j| {
            j.connection()
                .authorizer(Some(|context: AuthContext<'_>| match context.action {
                    AuthAction::Read {
                        table_name: "file_bytes",
                        column_name: "content",
                        ..
                    } => Authorization::Deny,
                    _ => Authorization::Allow,
                }))
                .unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        files
            .read_operation(&"alice".into(), &"large-document".into())
            .await
            .unwrap()
            .get("state"),
        Some(&"queued".into())
    );
    let overflow = request("bytes-overflow", &target);
    assert_eq!(
        files
            .enqueue(
                &"alice".into(),
                &overflow,
                &target,
                &hash_json(&overflow).unwrap(),
                request_bytes(&overflow),
                false
            )
            .await
            .unwrap_err()
            .code,
        "limit_exceeded"
    );
    // Removal must also work without selecting any original contents.
    files.mark(&document_id, OutputState::Sent, None).unwrap();
    clock.0.fetch_add(8 * 86_400_000, Ordering::SeqCst);
    files.prune().unwrap();
    assert_eq!(count(&files, "file_bytes"), 0);
    files
        .journal
        .access(|j| {
            j.connection()
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
                .unwrap();
            Ok(())
        })
        .unwrap();
    let mut png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jN1kAAAAASUVORK5CYII=").unwrap();
    png.resize(4 * 1024 * 1024, 0);
    let mut image_request = request("large-image", &target);
    image_request.insert("file", file(&png, "image/png"));
    let (decoded, image) = outgoing_file_bytes(image_request.required("file").unwrap()).unwrap();
    assert!(image);
    let delivery = files
        .enqueue(
            &"alice".into(),
            &image_request,
            &target,
            &hash_json(&image_request).unwrap(),
            decoded,
            image,
        )
        .await
        .unwrap();
    let image_id = delivery.required("messageId").unwrap().as_str().unwrap();
    files.journal.close().await.unwrap();
    let files = fixture.open(clock);
    let payload = files.payload(&image_id).unwrap();
    assert!(payload.image);
    assert_eq!(payload.bytes, png);
    assert_eq!(payload.file.get("contentHash"), Some(&digest(&png).into()));
    files.journal.close().await.unwrap();
}

#[tokio::test]
async fn rejected_receipt_rolls_back_original_blob_and_output_identity() {
    let fixture = Fixture::new();
    let files = fixture.open(Arc::new(SystemClock));
    let target = files.target(&workspace()).unwrap();
    files.journal.access(|j| {
        j.connection().execute_batch("CREATE TRIGGER reject_receipt BEFORE INSERT ON kv WHEN NEW.key LIKE 'file:%' BEGIN SELECT RAISE(FAIL,'fixture'); END").unwrap();
        Ok(())
    }).unwrap();
    let request = request("rejected", &target);
    assert_eq!(
        files
            .enqueue(
                &"alice".into(),
                &request,
                &target,
                &hash_json(&request).unwrap(),
                request_bytes(&request),
                false
            )
            .await
            .unwrap_err()
            .code,
        "storage_unavailable"
    );
    assert!(files.journal.close().await.is_err());
    let files = fixture.open(Arc::new(SystemClock));
    assert_eq!(count(&files, "outgoing"), 0);
    assert_eq!(count(&files, "file_bytes"), 0);
    assert_eq!(count(&files, "kv"), 0);
    files.journal.close().await.unwrap();
}

#[tokio::test]
async fn current_file_intents_adopt_atomically_without_changing_receipts() {
    let fixture = Fixture::new();
    let clock = Arc::new(SystemClock);
    let files = fixture.open(clock.clone());
    let target = files.target(&workspace()).unwrap();
    let request = request("adopt-original", &target);
    let hash = hash_json(&request).unwrap();
    let delivery = files
        .enqueue(
            &"alice".into(),
            &request,
            &target,
            &hash,
            request_bytes(&request),
            false,
        )
        .await
        .unwrap();
    let id = delivery.required("messageId").unwrap().as_str().unwrap();
    files.mark(&id, OutputState::Unknown, None).unwrap();
    let key = key(&"alice".into(), &"adopt-original".into()).unwrap();
    let mut old = files.saved(&id).unwrap();
    old.insert(
        "bytesBase64",
        request
            .required("file")
            .unwrap()
            .required("bytesBase64")
            .unwrap()
            .clone(),
    );
    files.journal.access(|j| j.try_transaction(|j| -> IvyResult<()> {
        j.set(&key, &old)?;
        j.write("DELETE FROM file_bytes WHERE id=?", vec![Value::Text(id.clone())])?;
        j.connection().execute_batch("CREATE TRIGGER reject_adoption BEFORE UPDATE ON kv WHEN NEW.key LIKE 'file:%' BEGIN SELECT RAISE(FAIL,'fixture'); END").unwrap();
        Ok(())
    })).unwrap();
    files.journal.close().await.unwrap();
    assert!(JournalIo::open(fixture.0.join("journal.sqlite")).is_err());
    let db = rusqlite::Connection::open(fixture.0.join("journal.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM file_bytes", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let retained: String = db
        .query_row("SELECT value FROM kv WHERE key=?", [&key], |r| r.get(0))
        .unwrap();
    assert_eq!(json(&retained), old);
    db.execute_batch("DROP TRIGGER reject_adoption").unwrap();
    drop(db);
    for _ in 0..2 {
        let files = fixture.open(clock.clone());
        assert_eq!(files.payload(&id).unwrap().bytes, request_bytes(&request));
        let saved = files.saved(&id).unwrap();
        assert!(saved.get("bytesBase64").is_none());
        assert_eq!(saved.get("requestHash"), Some(&hash.clone().into()));
        assert_eq!(count(&files, "file_bytes"), 1);
        assert_eq!(
            files
                .read_operation(&"alice".into(), &"adopt-original".into())
                .await
                .unwrap()
                .get("state"),
            Some(&"outcome_unknown".into())
        );
        files.journal.close().await.unwrap();
    }
}

#[tokio::test]
async fn terminal_receipt_limit_prunes_more_than_one_metadata_page() {
    let fixture = Fixture::new();
    let clock = Arc::new(Fixed(AtomicI64::new(1_792_800_000_000)));
    let files = fixture.open(clock.clone());
    let target = files.target(&workspace()).unwrap();
    for index in 0..321 {
        let request = request(&format!("receipt-{index}"), &target);
        let delivery = files
            .enqueue(
                &"alice".into(),
                &request,
                &target,
                &hash_json(&request).unwrap(),
                request_bytes(&request),
                false,
            )
            .await
            .unwrap();
        files
            .mark(
                &delivery.required("messageId").unwrap().as_str().unwrap(),
                OutputState::Sent,
                None,
            )
            .unwrap();
        clock.0.fetch_add(1, Ordering::SeqCst);
    }
    files.prune().unwrap();
    assert_eq!(
        files
            .journal
            .access(|j| Ok(j
                .connection()
                .query_row("SELECT count(*) FROM kv WHERE key LIKE 'file:%'", [], |r| r
                    .get::<_, i64>(0))
                .unwrap()))
            .unwrap(),
        256
    );
    assert_eq!(count(&files, "retired_outgoing"), 65);
    assert_eq!(count(&files, "file_bytes"), 0);
    assert!(files.has(&"alice".into(), &"receipt-0".into()).unwrap());
    assert_eq!(
        files
            .read_operation(&"alice".into(), &"receipt-0".into())
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    files
        .read_operation(&"alice".into(), &"receipt-320".into())
        .await
        .unwrap();
    clock.0.fetch_add(8 * 86_400_000, Ordering::SeqCst);
    files.prune().unwrap();
    assert_eq!(
        files
            .journal
            .access(|j| Ok(j
                .connection()
                .query_row("SELECT count(*) FROM kv WHERE key LIKE 'file:%'", [], |r| r
                    .get::<_, i64>(0))
                .unwrap()))
            .unwrap(),
        0
    );
    assert_eq!(count(&files, "retired_outgoing"), 321);
    files.journal.close().await.unwrap();
}
