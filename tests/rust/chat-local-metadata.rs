use super::super::{LocalStore, OperationCandidate};
use super::*;
use crate::store::Destination;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

#[test]
fn retirement_and_quota_queries_do_not_read_original_payload_columns() {
    let mut store = LocalStore::open(None).unwrap();
    let now = 1790000000123;
    let iso = "2026-09-21T14:13:20.123Z";
    let body = Json::object([
        ("operationId", "metadata:1790000000123:original".into()),
        ("phase", "succeeded".into()),
        ("updatedAt", iso.into()),
        (
            "request",
            Json::object([
                ("action", "send".into()),
                (
                    "payload",
                    Json::object([("text", "x".repeat(512 * 1024).into())]),
                ),
            ]),
        ),
    ]);
    let pin = store
        .write(
            "chat-bridge/operation",
            &body,
            Destination::Create {
                parent_id: Some("root"),
                name: "Original",
            },
            now,
            iso,
        )
        .unwrap();
    drop(body);
    store
        .save_blob(
            "chat-bridge/evidence-chunk",
            "Original bytes",
            &pin.object_id,
            &vec![7; 1024 * 1024],
            now,
        )
        .unwrap();
    store.limits.count = 0;
    store
        .db
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read {
                table_name: "revisions",
                column_name: "value_json",
            }
            | AuthAction::Read {
                table_name: "blobs",
                column_name: "bytes",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    assert!(
        store
            .db
            .prepare("SELECT value_json FROM revisions")
            .is_err()
    );
    assert!(store.db.prepare("SELECT bytes FROM blobs").is_err());
    assert!(store.needs_room().unwrap());
    let candidate = OperationCandidate {
        id: pin.object_id,
        operation_id: "metadata:1790000000123:original".into(),
        issued_at: now,
        sort: Value::Text(iso.into()),
        order: 1,
        input_id: None,
        notice_hash: None,
        reply_id: None,
    };
    store
        .retire_operation(
            &candidate,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        )
        .unwrap();
    let usage: (i64, i64, i64) = store
        .db
        .query_row(
            "SELECT entries,bytes,reserved_bytes FROM journal_usage WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(usage, (0, 0, 0));
    store
        .db
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
    store.close().unwrap();
}
