use super::{Published, catalog, connection, journal};
use ivy_agent_manager::{
    inventory::NativeInventory, inventory_manager::InventoryManager,
    notice_policy::NativeNoticePolicy,
};
use ivy_contracts::{IvyJson, IvyResult, JsString};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const METHODS: [&str; 3] = ["thread/list", "thread/loaded/list", "project/list"];

fn page(rows: Vec<IvyJson>, next: IvyJson) -> IvyJson {
    IvyJson::object([("data", IvyJson::Array(rows)), ("nextCursor", next)])
}
fn thread(id: &str) -> IvyJson {
    IvyJson::object([
        ("id", id.into()),
        ("name", IvyJson::Null),
        ("preview", "original".into()),
        ("cwd", "/work".into()),
        ("status", IvyJson::object([("type", "idle".into())])),
        ("source", "cli".into()),
        ("threadSource", "cli".into()),
        ("updatedAt", 1.0.into()),
        ("ephemeral", false.into()),
    ])
}
fn project(id: &str) -> IvyJson {
    IvyJson::object([
        ("id", id.into()),
        ("name", "Original".into()),
        (
            "roots",
            IvyJson::Array(vec![IvyJson::object([("path", "/work".into())])]),
        ),
        ("position", 0.0.into()),
        ("recencyAt", IvyJson::Null),
    ])
}
fn reader(
    target: &'static str,
    pages: impl Fn(&IvyJson, usize) -> IvyJson + Send + Sync + 'static,
) -> (NativeInventory, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let inventory = NativeInventory::with_reader(
        &catalog(),
        "original-epoch".into(),
        Arc::new({
            let calls = calls.clone();
            move |method, params, _| {
                let result = if method.eq_str(target)
                    && !(target == "thread/list" && params.get("archived") == Some(&true.into()))
                {
                    assert_eq!(params.get("limit"), Some(&100.0.into()));
                    pages(&params, calls.fetch_add(1, Ordering::SeqCst))
                } else if method.eq_str("thread/read") {
                    IvyJson::object([(
                        "thread",
                        thread(&params.required("threadId").unwrap().as_str().unwrap()),
                    )])
                } else {
                    page(vec![], IvyJson::Null)
                };
                Box::pin(async move { Ok(IvyJson::object([("result", result)])) })
            }
        }),
        Arc::new(|| 0.0),
        Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
    )
    .unwrap();
    (inventory, calls)
}
async fn observe(inventory: &NativeInventory, target: &str) -> IvyResult<IvyJson> {
    match target {
        "project/list" => inventory.collect_projects(None).await,
        "thread/loaded/list" => inventory.has_active_threads().await.map(IvyJson::Bool),
        _ => inventory.collect_threads(None).await,
    }
}

#[tokio::test]
async fn inventory_rejects_oversized_pages_before_materializing_or_advancing_them() {
    for target in METHODS {
        let (inventory, calls) =
            reader(target, |_, _| page(vec![IvyJson::Null; 101], "next".into()));
        assert_eq!(
            observe(&inventory, target).await.unwrap_err().code,
            "native_inventory_capacity",
            "{target}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{target}");
    }
}

#[tokio::test]
async fn inventory_rejects_unbounded_or_empty_continuations_before_reusing_them() {
    for target in METHODS {
        for length in [0, 8193] {
            let (inventory, calls) =
                reader(target, move |_, _| page(vec![], "x".repeat(length).into()));
            let expected = if length == 0 {
                "native_inventory_invalid"
            } else {
                "native_inventory_capacity"
            };
            assert_eq!(
                observe(&inventory, target).await.unwrap_err().code,
                expected,
                "{target}/{length}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{target}/{length}");
        }
    }
}

#[tokio::test]
async fn inventory_bounds_empty_page_work_and_preserves_opaque_utf16_continuations() {
    for target in METHODS {
        for complete in [true, false] {
            let (inventory, calls) = reader(target, move |params, index| {
                if index > 0 {
                    assert_eq!(
                        params.get("cursor"),
                        Some(&IvyJson::String(JsString(vec![0xd800, index as u16])))
                    );
                }
                page(
                    vec![],
                    if complete && index == 63 {
                        IvyJson::Null
                    } else {
                        IvyJson::String(JsString(vec![0xd800, index as u16 + 1]))
                    },
                )
            });
            let result = observe(&inventory, target).await;
            if complete {
                result.unwrap();
            } else {
                assert_eq!(
                    result.unwrap_err().code,
                    "native_inventory_capacity",
                    "{target}"
                );
            }
            assert_eq!(calls.load(Ordering::SeqCst), 64, "{target}/{complete}");
        }
        let original = JsString([vec![0xd800], vec![120; 8191]].concat());
        let (inventory, calls) = reader(target, move |params, index| {
            if index == 0 {
                page(vec![], IvyJson::String(original.clone()))
            } else {
                assert_eq!(
                    params.get("cursor"),
                    Some(&IvyJson::String(original.clone()))
                );
                page(vec![], IvyJson::Null)
            }
        });
        observe(&inventory, target).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{target}");
    }
}

#[tokio::test]
async fn inventory_bounds_retained_cursor_bytes_even_when_pages_have_no_rows() {
    for target in METHODS {
        let (inventory, calls) = reader(target, |_, index| {
            let mut units = vec![120; 8192];
            units[0] = 0xd800;
            units[1] = index as u16;
            page(vec![], IvyJson::String(JsString(units)))
        });
        assert_eq!(
            observe(&inventory, target).await.unwrap_err().code,
            "native_inventory_capacity",
            "{target}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 32, "{target}");
    }
}

#[tokio::test]
async fn inventory_keeps_original_entry_capacities_across_real_native_pages() {
    for target in METHODS {
        let maximum = if target == "project/list" { 256 } else { 5000 };
        let (inventory, calls) = reader(target, move |params, _| {
            let start = params
                .get("cursor")
                .map(|v| v.as_str().unwrap().parse::<usize>().unwrap())
                .unwrap_or(0);
            let end = (start + 100).min(maximum);
            let rows = (start..end)
                .map(|i| {
                    let id = format!("original-{i}");
                    match target {
                        "project/list" => project(&id),
                        "thread/loaded/list" => id.into(),
                        _ => thread(&id),
                    }
                })
                .collect();
            page(
                rows,
                if end < maximum {
                    end.to_string().into()
                } else {
                    IvyJson::Null
                },
            )
        });
        let result = observe(&inventory, target).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            maximum.div_ceil(100),
            "{target}"
        );
        if target == "thread/loaded/list" {
            assert_eq!(result, false.into());
        } else {
            let key = if target == "project/list" {
                "projects"
            } else {
                "threads"
            };
            let value = result.required(key).unwrap();
            let rows = if target == "project/list" {
                value.required("projects").unwrap()
            } else {
                value
            }
            .as_array()
            .unwrap();
            assert_eq!(rows.len(), maximum, "{target}");
            assert_eq!(
                rows[maximum - 1].get("nativeId"),
                Some(&format!("original-{}", maximum - 1).into())
            );
        }
    }
}

#[tokio::test]
async fn incomplete_page_runs_preserve_original_hive_and_project_state_until_recovery() {
    let fail = Arc::new(AtomicBool::new(false));
    let inventory = Arc::new(
        NativeInventory::with_reader(
            &catalog(),
            "original-epoch".into(),
            Arc::new({
                let fail = fail.clone();
                move |method, params, _| {
                    let rows = if method.eq_str("project/list") {
                        vec![project("p")]
                    } else if method.eq_str("thread/list")
                        && params.get("archived") == Some(&false.into())
                    {
                        vec![thread("t")]
                    } else {
                        vec![]
                    };
                    let next = if fail.load(Ordering::SeqCst) && !rows.is_empty() {
                        "x".repeat(8193).into()
                    } else {
                        IvyJson::Null
                    };
                    Box::pin(async move { Ok(IvyJson::object([("result", page(rows, next))])) })
                }
            }),
            Arc::new(|| 0.0),
            Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
        )
        .unwrap(),
    );
    let client = Arc::new(Published::default());
    let manager = InventoryManager::with_clock(
        inventory.clone(),
        journal(),
        Arc::new(Mutex::new(NativeNoticePolicy::default())),
        Arc::new(|_| Ok(())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|| 0.0),
    )
    .unwrap();
    manager.bind(connection(client.clone())).unwrap();
    manager
        .refresh_threads(connection(client.clone()))
        .unwrap()
        .wait()
        .await
        .unwrap();
    manager
        .refresh_projects(connection(client.clone()))
        .unwrap()
        .wait()
        .await
        .unwrap();
    let original = manager.project_model().unwrap();
    let published = client.0.lock().unwrap().clone();
    fail.store(true, Ordering::SeqCst);
    for projects in [false, true] {
        let work = if projects {
            manager.refresh_projects(connection(client.clone()))
        } else {
            manager.refresh_threads(connection(client.clone()))
        }
        .unwrap();
        assert_eq!(
            work.wait().await.unwrap_err().code,
            "native_inventory_capacity"
        );
        assert_eq!(*client.0.lock().unwrap(), published);
        assert!(Arc::ptr_eq(
            &manager.project_model().unwrap().projects,
            &original.projects
        ));
        assert!(Arc::ptr_eq(
            &inventory
                .current_project_snapshot()
                .unwrap()
                .unwrap()
                .projects,
            &original.projects
        ));
    }
    fail.store(false, Ordering::SeqCst);
    manager
        .refresh_threads(connection(client.clone()))
        .unwrap()
        .wait()
        .await
        .unwrap();
    manager
        .refresh_projects(connection(client.clone()))
        .unwrap()
        .wait()
        .await
        .unwrap();
    let calls = client.0.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[2].get("mode"), Some(&"snapshot".into()));
    assert!(calls[2].get("expectedRevision").is_none());
    assert_eq!(calls[2].get("entries"), published[0].get("entries"));
    assert_eq!(calls[3].get("entries"), published[1].get("entries"));
    assert!(manager.code().unwrap().is_none());
}
