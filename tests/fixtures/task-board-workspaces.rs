use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    local_journal,
    local_workspaces::{LocalWorkspaces, WorkspaceDocument, WorkspaceOwner, workspace_key},
};
use std::{cell::RefCell, path::Path, process::Command};
fn text(v: &Json, k: &str) -> JsString {
    v.required(k).unwrap().as_js_string().unwrap().clone()
}
fn values(v: &Json) -> &[Json] {
    v.as_array().unwrap()
}
struct Owner<'a> {
    db: &'a Connection,
    input: Json,
    trace: RefCell<Vec<Json>>,
}
impl WorkspaceOwner for Owner<'_> {
    async fn read(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<WorkspaceDocument> {
        self.trace.borrow_mut().push(Json::Array(vec![
            "read".into(),
            key.into(),
            address.clone(),
            primary.into(),
            include_archived.into(),
        ]));
        let id = address
            .as_js_string()
            .cloned()
            .unwrap_or_else(|| text(address, "objectId"))
            .to_utf8()?;
        let value = self
            .input
            .required("reads")?
            .get(&id)
            .ok_or_else(|| IvyError::new("not_found", "fixture read"))?;
        if let Some(code) = value.get("error") {
            return Err(IvyError::new(code.as_str()?, "fixture read"));
        }
        Ok(WorkspaceDocument {
            pin: value.required("pin")?.clone(),
            value: value.required("value")?.clone(),
            effectively_archived: value
                .required("metadata")?
                .required("effectivelyArchived")?
                .as_bool()
                .unwrap(),
        })
    }
    async fn stat(&self, id: &JsString) -> IvyResult<Json> {
        self.trace.borrow_mut().push(Json::Array(vec![
            "objects.stat".into(),
            Json::object([("objectId", Json::String(id.clone()))]),
        ]));
        if let Some(race) = self.input.get("race") {
            if race.as_str()? == "operation" {
                self.db
                    .execute(
                        "UPDATE workspace_reservations SET operation_id=?",
                        ["replacement"],
                    )
                    .unwrap();
            } else {
                self.db
                    .execute(
                        "UPDATE workspace_reservations SET task_id=?",
                        ["replacement"],
                    )
                    .unwrap();
            }
        }
        let result = self.input.required("stat")?;
        if let Some(code) = result.get("error") {
            return Err(IvyError::new(code.as_str()?, "fixture stat"));
        }
        Ok(result.clone())
    }
}
#[tokio::test]
async fn reservations_and_owner_reclamation_match_original_transactions_and_read_fences() {
    let output = Command::new("node")
        .args([
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-workspaces-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for row in values(&rows) {
        let input = row.required("input").unwrap();
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        for entry in values(input.required("reservations").unwrap()) {
            let r = values(entry);
            db.execute(
                "INSERT INTO workspace_reservations VALUES(?,?,?,?)",
                params![
                    r[0].as_str().unwrap(),
                    r[1].as_str().unwrap(),
                    r[2].as_str().unwrap(),
                    r[3].as_str().unwrap()
                ],
            )
            .unwrap();
        }
        if input.get("gate").and_then(Json::as_bool) == Some(true) {
            db.execute(
                "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                params![
                    "gate",
                    "task-board/coordination",
                    "TaskBoard publication gate owner",
                    None::<String>,
                    1,
                    "time",
                    "time"
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO active_gates VALUES(?,?)",
                params!["gate", "operation"],
            )
            .unwrap();
        }
        if let Some(fault) = input.get("fault") {
            db.execute_batch(&fault.as_str().unwrap()).unwrap();
        }
        for key in values(row.required("keys").unwrap()) {
            let pair = values(key);
            assert_eq!(
                workspace_key(pair[0].as_js_string().unwrap()).unwrap(),
                *pair[1].as_js_string().unwrap(),
                "workspace key {:?}",
                pair[0]
            );
        }
        let owner = Owner {
            db: &db,
            input: input.clone(),
            trace: RefCell::default(),
        };
        let subject = LocalWorkspaces {
            connection: &db,
            store: &owner,
            normalize: workspace_key,
        };
        let (host, cwd, task, operation) = (
            text(input, "host"),
            text(input, "cwd"),
            text(input, "task"),
            text(input, "operation"),
        );
        let result = match input.required("action").unwrap().as_str().unwrap().as_str() {
            "reserve" => subject.reserve(&host, &cwd, &task, &operation),
            "owner" => subject.owner(&host, &cwd, &task).await,
            "available" => {
                subject
                    .reserve_available(&host, &cwd, &task, &operation)
                    .await
            }
            "release" => subject.release(&task).map(|()| None),
            _ => subject.release_operation(&operation).map(|()| None),
        };
        let result = match result {
            Ok(value) => value.map(Json::String).unwrap_or(Json::Null),
            Err(failure) if failure.code == "storage_unavailable" => {
                Json::object([("storageError", true.into())])
            }
            Err(failure) => Json::object([
                ("code", failure.code.into()),
                ("message", failure.message.into()),
            ]),
        };
        let final_rows=db.prepare("SELECT host_id,canonical_cwd,task_id,operation_id FROM workspace_reservations ORDER BY host_id,canonical_cwd").unwrap().query_map([],|row|Ok(Json::Array((0..4).map(|i|Json::from(row.get::<_,String>(i).unwrap())).collect()))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(owner.trace.into_inner())),
            ("rows", Json::Array(final_rows)),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(row.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "case {:?}",
            text(row, "name")
        );
        assert!(db.is_autocommit());
    }
}
