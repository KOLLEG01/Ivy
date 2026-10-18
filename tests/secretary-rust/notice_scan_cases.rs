use super::*;
use ivy_contracts::JsString;
use ivy_secretary::notice_scan::NoticeScan;

struct QueryFixture {
    rows: Vec<IvyJson>,
    fail_next: Mutex<bool>,
    requests: Mutex<Vec<IvyJson>>,
}

fn matches(predicate: &IvyJson, row: &IvyJson) -> bool {
    match predicate.required("op").unwrap().as_str().unwrap().as_str() {
        "eq" => {
            row.get(&predicate.required("field").unwrap().as_str().unwrap())
                == predicate.get("value")
        }
        "and" => predicate
            .required("args")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|p| matches(p, row)),
        "or" => predicate
            .required("args")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|p| matches(p, row)),
        _ => panic!("Unexpected query predicate"),
    }
}

impl RpcClient for QueryFixture {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "objects.query");
            self.requests.lock().unwrap().push(params.clone());
            if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
                return Err(IvyError::new("fixture_unavailable", "Query unavailable."));
            }
            assert_eq!(params.get("contractKey"), Some(&"secretary/item".into()));
            assert!(params.get("includeContent").is_none());
            assert_eq!(
                params.required("orderBy")?,
                &IvyJson::Array(vec![IvyJson::object([
                    ("field", "object.id".into()),
                    ("direction", "asc".into()),
                ])])
            );
            let offset: usize = params
                .get("cursor")
                .map(|v| v.as_str().unwrap().parse().unwrap())
                .unwrap_or(0);
            let limit = params.required("limit")?.as_f64().unwrap() as usize;
            let rows: Vec<_> = self
                .rows
                .iter()
                .filter(|row| matches(params.get("where").unwrap(), row))
                .collect();
            let end = (offset + limit).min(rows.len());
            Ok(IvyJson::object([
                (
                    "items",
                    IvyJson::Array(
                        rows[offset..end]
                            .iter()
                            .map(|row| {
                                IvyJson::object([("objectId", row.required("id").unwrap().clone())])
                            })
                            .collect(),
                    ),
                ),
                (
                    "nextCursor",
                    if end < rows.len() {
                        end.to_string().into()
                    } else {
                        IvyJson::Null
                    },
                ),
            ]))
        })
    }
}

fn row(id: &str, root: &str, state: &str, reason: Option<&str>, voice: Option<&str>) -> IvyJson {
    let mut row = IvyJson::object([
        ("id", id.into()),
        ("object.parentId", root.into()),
        ("data:/notice/state", state.into()),
    ]);
    if let Some(reason) = reason {
        row.insert("data:/notice/reason", reason.into());
    }
    if let Some(voice) = voice {
        row.insert("data:/notice/voice/state", voice.into());
    }
    row
}

#[tokio::test]
async fn late_display_receipts_reenter_notice_pages_without_admitting_other_unknowns() {
    let client = QueryFixture {
        rows: vec![
            row("01", "root", "queued", None, None),
            row("02", "root", "dispatching", Some("awaiting_main"), None),
            row("03", "root", "confirmed", None, Some("queued")),
            row("04", "root", "confirmed", None, Some("outcome_unknown")),
            row(
                "05",
                "root",
                "outcome_unknown",
                Some("display_outcome_unknown"),
                None,
            ),
            row(
                "06",
                "root",
                "outcome_unknown",
                Some("chat_operation_failed"),
                Some("queued"),
            ),
            row(
                "07",
                "root",
                "outcome_unknown",
                Some("secretary_notice_mismatch"),
                None,
            ),
            row("08", "root", "outcome_unknown", None, None),
            row("09", "root", "confirmed", None, Some("confirmed")),
            row("10", "root", "confirmed", None, None),
            row(
                "11",
                "root",
                "suppressed",
                Some("display_outcome_unknown"),
                Some("queued"),
            ),
            row(
                "12",
                "other",
                "outcome_unknown",
                Some("display_outcome_unknown"),
                None,
            ),
            row("13", "other", "queued", None, None),
        ],
        fail_next: Mutex::new(false),
        requests: Mutex::new(Vec::new()),
    };
    let mut scan = NoticeScan::default();
    let mut ids = Vec::new();
    loop {
        ids.extend(scan.page(&client, &"root".into(), 2).await.unwrap());
        if scan.cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids, ["01", "02", "03", "04", "05"].map(JsString::from));
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    // Cursor exhaustion and a restarted worker both rediscover the same retained
    // late receipt; page selection neither changes nor sends its operation.
    assert_eq!(scan.page(&client, &"root".into(), 20).await.unwrap(), ids);
    assert_eq!(
        NoticeScan::default()
            .page(&client, &"root".into(), 20)
            .await
            .unwrap(),
        ids
    );
}

#[tokio::test]
async fn notice_query_failure_keeps_original_page_cursor() {
    let client = QueryFixture {
        rows: vec![
            row("01", "root", "queued", None, None),
            row("02", "root", "queued", None, None),
        ],
        fail_next: Mutex::new(false),
        requests: Mutex::new(Vec::new()),
    };
    let mut scan = NoticeScan::default();
    assert_eq!(
        scan.page(&client, &"root".into(), 1).await.unwrap(),
        vec![JsString::from("01")]
    );
    *client.fail_next.lock().unwrap() = true;
    assert_eq!(
        scan.page(&client, &"root".into(), 1)
            .await
            .unwrap_err()
            .code,
        "fixture_unavailable"
    );
    assert_eq!(scan.cursor, Some("1".into()));
    assert_eq!(
        scan.page(&client, &"root".into(), 1).await.unwrap(),
        vec![JsString::from("02")]
    );
    assert!(scan.cursor.is_none());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[1], requests[2]);
}
