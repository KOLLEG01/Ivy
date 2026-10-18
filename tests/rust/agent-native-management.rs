use super::super::{Handle, Store, equal, field, id, outcome, string, text};
use super::{Fixture, catalog};
use ivy_agent_manager::{
    interactions::{Limits as InteractionLimits, NativeInteractions},
    native_management::{NativeManagement, NativeManagementOwner},
    native_operations::{NativeOperationStore, SqliteOperationStore},
};
use ivy_contracts::{
    IvyJson, IvyResult, JsString,
    hash::{hash_json, tool_definition_hash},
    json::parse,
};
use ivy_sdk::service::{InvocationContext, ToolHandler};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let result = Command::new("node")
            .arg("tests/fixtures/agent-native-management-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(
            std::str::from_utf8(&result.stdout).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap()
    })
}
fn management(fixture: &Fixture) -> NativeManagement {
    management_for(fixture, catalog())
}
fn management_for(fixture: &Fixture, catalog: IvyJson) -> NativeManagement {
    NativeManagement::with_clock(
        NativeManagementOwner {
            service_node_id: "agent".into(),
            epoch: "live".into(),
            catalog,
            rpc: fixture.rpc.clone(),
            closing: fixture.closing.clone(),
            platform: "win32".into(),
        },
        fixture.operations.clone(),
        NativeInteractions::with_clock(
            "live".into(),
            InteractionLimits::default(),
            Arc::new(|| 1_792_281_600_000),
        ),
        Arc::new(|| "2026-10-18T00:00:00.000Z".into()),
    )
    .unwrap()
}
fn context(step: &IvyJson) -> InvocationContext {
    let context = field(step, "context");
    let context = InvocationContext {
        caller_principal_id: text(context, "callerPrincipalId").clone(),
        operation_id: context
            .get("operationId")
            .and_then(IvyJson::as_js_string)
            .cloned(),
        generation: 1,
        signal: Default::default(),
    };
    if step.get("cancelled") == Some(&true.into()) {
        context.signal.cancel();
    }
    context
}
fn random_id(id: &JsString) -> bool {
    let Ok(text) = id.to_utf8() else { return false };
    let text = text.strip_prefix("interaction-").unwrap_or(&text);
    uuid::Uuid::parse_str(text).is_ok_and(|id| id.get_version_num() == 4)
}
#[derive(Default)]
struct Normalize {
    ids: HashMap<JsString, JsString>,
    requests: usize,
    observations: usize,
}
impl Normalize {
    fn frame(&mut self, frame: &IvyJson) {
        if let Some(id) = frame
            .get("id")
            .and_then(IvyJson::as_js_string)
            .filter(|id| random_id(id))
        {
            self.requests += 1;
            assert!(
                self.ids
                    .insert(id.clone(), format!("request-{}", self.requests).into())
                    .is_none()
            );
        }
    }
    fn value(&mut self, value: &mut IvyJson) {
        match value {
            IvyJson::String(text) => {
                if let Some(replacement) = self.ids.get(text) {
                    *text = replacement.clone();
                }
            }
            IvyJson::Array(values) => {
                for value in values {
                    self.value(value);
                }
            }
            IvyJson::Object(pairs) => {
                for (key, value) in pairs {
                    if key == &JsString::from("observationId") {
                        let id = value.as_js_string().unwrap();
                        assert!(random_id(id));
                        if !self.ids.contains_key(id) {
                            self.observations += 1;
                            self.ids.insert(
                                id.clone(),
                                format!("observation-{}", self.observations).into(),
                            );
                        }
                    }
                    self.value(value);
                }
            }
            _ => {}
        }
    }
}
async fn execute(
    fixture: &mut Fixture,
    handlers: &HashMap<JsString, ToolHandler>,
    step: &IvyJson,
) -> IvyResult<IvyJson> {
    let action = string(step, "action");
    if action == "manage" || action == "startManage" {
        let handler = handlers
            .get(&JsString::from(format!("agent.{}", string(step, "tool"))))
            .unwrap();
        let work = handler(field(step, "input").clone(), context(step));
        let summarize = step.get("summary") == Some(&true.into());
        let work = async move {
            let value = work.await?;
            if summarize {
                Ok(IvyJson::object([("hash", hash_json(&value)?.into())]))
            } else {
                Ok(value)
            }
        };
        if action == "manage" {
            return work.await;
        }
        fixture
            .handles
            .insert(string(step, "handle"), Handle::track(work));
        Ok(IvyJson::Null)
    } else if action == "replaceEpoch" {
        fixture
            .operations
            .journal()
            .lock()
            .unwrap()
            .begin_epoch(&"replacement".into())?;
        Ok(IvyJson::Null)
    } else {
        fixture.execute(step).await
    }
}

#[tokio::test]
async fn native_management_matches_original_guards_receipts_observations_and_input_authority() {
    let rows = field(data(), "cases").as_array().unwrap();
    assert!(rows.len() >= 50);
    for row in rows {
        let catalog = field(data(), "catalog").clone();
        let store = Arc::new(Store::new());
        let mut fixture = Fixture::with_catalog(row, store.clone(), store, catalog.clone());
        let management = management_for(&fixture, catalog);
        let handlers = management.handlers();
        assert_eq!(handlers.len(), 12);
        let mut normalize = Normalize::default();
        for (index, step) in field(row, "commands")
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let action = string(step, "action");
            let label = format!("{} step {index}: {action}", string(row, "name"));
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                execute(&mut fixture, &handlers, step),
            )
            .await
            .expect(&label);
            if action == "sent" {
                normalize.frame(result.as_ref().unwrap());
            }
            let mut actual = if action == "wait" {
                result.unwrap()
            } else {
                outcome(result)
            };
            normalize.value(&mut actual);
            equal(
                &actual,
                &field(row, "expected").as_array().unwrap()[index],
                &label,
            );
        }
        tokio::time::timeout(Duration::from_secs(5), fixture.finish())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_management_binding_refuses_before_sqlite_and_persists_before_native_bytes() {
    let root = super::super::super::Fixture::new();
    let state = Arc::new(Mutex::new(root.open(10, 128 * 1024 * 1024)));
    let durable = Arc::new(SqliteOperationStore::new(state.clone()).unwrap());
    let mut fixture = Fixture::with_store(&IvyJson::Null, Arc::new(Store::new()), durable.clone());
    let management = management(&fixture);
    let handlers = management.handlers();
    let method = "account/logout";
    let definition = management
        .registry()
        .get("namespaces")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .get("tools")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool.get("nativeMethod") == Some(&method.into()))
        .unwrap();
    let operation_id = format!("epoch:{}:managed", chrono::Utc::now().timestamp_millis());
    let identity = id(&operation_id);
    let key = hash_json(&identity.value()).unwrap();
    let request = IvyJson::object([
        ("operationId", operation_id.clone().into()),
        ("method", method.into()),
        ("params", IvyJson::Null),
        ("nativeVersion", catalog().get("version").unwrap().clone()),
        (
            "expectedDefinitionHash",
            tool_definition_hash(definition).unwrap().into(),
        ),
    ]);
    let context = InvocationContext {
        caller_principal_id: "caller".into(),
        operation_id: Some(operation_id.into()),
        generation: 1,
        signal: Default::default(),
    };
    let mut wrong = request.clone();
    wrong.insert(
        "expectedDefinitionHash",
        format!("sha256:{}", "f".repeat(64)).into(),
    );
    let handler = handlers.get(&JsString::from("agent.invoke")).unwrap();
    assert_eq!(
        handler(wrong, context.clone()).await.unwrap_err().code,
        "native_definition_mismatch"
    );
    assert!(durable.read(key.clone()).await.unwrap().is_none());
    assert!(fixture.trace.lock().unwrap().frames.is_empty());
    let work = tokio::spawn(handler(request.clone(), context.clone()));
    let sent = fixture.peer.sent().await;
    assert_eq!(
        durable
            .read(key.clone())
            .await
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&"dispatched".into())
    );
    fixture
        .peer
        .emit(&IvyJson::object([
            ("id", field(&sent, "id").clone()),
            ("result", IvyJson::Null),
        ]))
        .await;
    let receipt = work.await.unwrap().unwrap();
    assert_eq!(receipt.get("phase"), Some(&"succeeded".into()));
    fixture.receipts.drain().await;
    fixture.finish().await;
    drop(handlers);
    drop(management);
    drop(durable);
    super::super::close_state(state).await;
    let reopened = root.open(10, 128 * 1024 * 1024);
    assert_eq!(
        reopened
            .read("operation", &key)
            .unwrap()
            .unwrap()
            .value
            .get("phase"),
        Some(&"succeeded".into())
    );
    reopened.close().unwrap();
}
