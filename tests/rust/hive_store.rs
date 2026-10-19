use ivy_contracts::{IvyError, IvyJson, JsString, json::stringify};
use ivy_hive::store::{
    AuthContext, AuthenticatedContext, Credential, LedgerResult, Principal, ReceiptKind, Store,
};
#[path = "hive_contracts.rs"]
mod contracts;
#[path = "hive_events.rs"]
mod events;
#[path = "hive_worker_identity.rs"]
mod identity;
#[path = "hive_ledger.rs"]
mod ledger;
#[path = "hive_object_delete.rs"]
mod object_delete;
#[path = "hive_object_move.rs"]
mod object_move;
#[path = "hive_object_mutations.rs"]
mod object_mutations;
#[path = "hive_object_write.rs"]
mod object_write;
#[path = "hive_online_backup.rs"]
mod online_backup;
#[path = "hive_package_catalog.rs"]
mod package_catalog;
#[path = "hive_package_kernel.rs"]
mod package_kernel;
#[path = "hive_packages.rs"]
mod packages;
#[path = "hive_query.rs"]
mod query;
#[path = "hive_registry.rs"]
mod registry;
#[path = "hive_retention.rs"]
mod retention;
#[path = "hive_search.rs"]
mod search;
#[path = "hive_ui_files.rs"]
mod ui_files;
#[path = "hive_wiki.rs"]
mod wiki;
use rusqlite::types::ValueRef;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        Self(super::temporary_directory("ivy-store"))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn rows(store: &Store, sql: &str) -> Value {
    let mut statement = store.connection().prepare(sql).unwrap();
    let names = statement
        .column_names()
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>();
    let values = statement
        .query_map([], |row| {
            let mut value = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let v = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(v) => json!(std::str::from_utf8(v).unwrap()),
                    ValueRef::Blob(_) => panic!("Unexpected blob in observation"),
                };
                value.insert(name.clone(), v);
            }
            Ok(Value::Object(value))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    json!(values)
}
fn snapshot(store: &Store, step: &Value) -> Value {
    let compact_events = step["compactEvents"] == true;
    let changes = if step["lossless"] == true {
        json!({"encoded":stringify(&IvyJson::Array(store.changes().iter().cloned().map(IvyJson::String).collect()),usize::MAX).unwrap()})
    } else {
        json!(
            store
                .changes()
                .iter()
                .map(|s| s.to_utf8().unwrap())
                .collect::<Vec<_>>()
        )
    };
    let table_hash = |table: &str, columns: &str| {
        let encoded:String=store.connection().query_row(&format!("SELECT json_group_array(json_array({columns})) FROM (SELECT rowid AS record_id,* FROM {table} ORDER BY rowid)"),[],|r|r.get(0)).unwrap();
        ivy_contracts::hash::digest(encoded)
    };
    json!({"contracts":table_hash("contracts","key,version,definition"),"families":rows(store,"SELECT * FROM contract_families ORDER BY key"),"objects":rows(store,"SELECT * FROM objects ORDER BY id"),"fts":rows(store,"SELECT rowid,object_id,name,text FROM object_fts ORDER BY rowid"),"revisions":table_hash("revisions","record_id,object_id,revision,contract_key,contract_version,encoding,CASE WHEN typeof(content)='blob' THEN hex(content) ELSE content END,content_hash,byte_length,created_at,references_json"),"references":rows(store,"SELECT * FROM revision_references ORDER BY source_object_id,source_revision,name"),"principals":rows(store,"SELECT * FROM principals ORDER BY id"),"credentials":rows(store,"SELECT * FROM credentials ORDER BY digest"),"nodes":rows(store,"SELECT * FROM service_nodes ORDER BY id"),"sessions":rows(store,"SELECT * FROM browser_sessions ORDER BY digest"),"subscriptions":rows(store,"SELECT * FROM subscriptions ORDER BY node_id,name"),"durable":rows(store,"SELECT * FROM durable_subscriptions ORDER BY node_id,name"),"changes":changes,"mutations":table_hash("mutations","record_id,principal_id,mutation_id,request_hash,issued_at_ms,completed_at_ms,result_kind,result_json,receipt_bytes"),"events":table_hash("events",if compact_events{"sequence,topic,topic_version,source,occurred_at,mutation_id,length(CAST(payload AS BLOB))"}else{"sequence,topic,topic_version,source,occurred_at,mutation_id,payload"}),"metadata":rows(store,"SELECT * FROM metadata WHERE key NOT IN ('cursor_secret','runtime_epoch') ORDER BY key"),"retention":rows(store,"SELECT * FROM retention_families ORDER BY contract_key"),"apps":rows(store,"SELECT * FROM apps ORDER BY app_id"),"releases":rows(store,"SELECT * FROM app_releases ORDER BY app_id,release_id"),"diagnostics":if step["compactDiagnostics"]==true{json!(table_hash("diagnostics","record_id,identity,diagnostic_json"))}else{rows(store,"SELECT * FROM diagnostics ORDER BY identity")},"connections":rows(store,"SELECT * FROM service_connections ORDER BY node_id"),"managed":rows(store,"SELECT * FROM managed_nodes ORDER BY node_id"),"hosts":rows(store,"SELECT * FROM host_reports ORDER BY host_id"),"registries":rows(store,"SELECT * FROM registries ORDER BY node_id"),"inventorySchemas":rows(store,"SELECT * FROM inventory_schemas ORDER BY namespace,kind,version"),"toolDefinitions":rows(store,"SELECT * FROM tool_definitions ORDER BY hash")})
}
fn context(value: &Value) -> AuthenticatedContext {
    AuthenticatedContext {
        principal_id: value["principalId"].as_str().unwrap().into(),
        credential_digest: value["credentialDigest"].as_str().unwrap().into(),
    }
}
fn wire(value: IvyJson) -> Value {
    serde_json::from_str(&stringify(&value, 1 << 26).unwrap()).unwrap()
}
fn native(path: &Path, steps: &[Value]) -> Value {
    native_with_runtime(path, steps, None)
}
fn native_with_runtime(path: &Path, steps: &[Value], runtime: Option<&Value>) -> Value {
    let mut store = Store::open(path).unwrap();
    let mut result = vec![];
    let package_trace = steps
        .iter()
        .any(|step| step["op"] == "packageCatalog" || step["op"] == "packageKernel");
    let mut package_catalog = package_catalog::Trace::default();
    let mut package_kernel = package_kernel::Trace::default();
    let registry_trace = steps.iter().any(|step| {
        step["op"] == "registry"
            || step["op"] == "inventory"
            || step["op"] == "discovery"
            || step["op"] == "configurations"
            || step["op"] == "uis"
    });
    let inventory_trace = steps.iter().any(|step| step["op"] == "inventory");
    let mut registry = registry::Trace::default();
    for step in steps {
        let mut bound;
        let step = if step["bindings"].is_array() || step["identityBindings"].is_array() {
            bound = step.clone();
            if bound["bindings"].is_array() {
                object_write::bind(&mut bound, &result);
            }
            if bound["identityBindings"].is_array() {
                identity::bind(&mut bound, &result);
            }
            &bound
        } else {
            step
        };
        let mut wiki_calls = vec![];
        let outcome = match step["op"].as_str().unwrap() {
            "packageCatalog" => package_catalog.execute(&mut store, step).map(wire),
            "packageKernel" => package_kernel
                .execute(
                    &mut store,
                    step,
                    runtime.expect("Package kernel runtime primitives"),
                )
                .map(wire),
            "uiFiles" => ui_files::execute(&mut store, step, path).map(wire),
            "wiki" => {
                let (result, calls) = wiki::execute(
                    &mut store,
                    step,
                    runtime.expect("Search Unicode observations"),
                );
                wiki_calls = calls;
                result.map(wire)
            }
            "search" => search::execute(
                &mut store,
                step,
                runtime.expect("Search Unicode observations"),
            )
            .map(wire),
            "registry" | "inventory" | "discovery" | "configurations" | "uis" => registry
                .execute(
                    &mut store,
                    step,
                    runtime.expect("Registry runtime observations"),
                    path,
                )
                .map(|value| {
                    if step["lossless"] == true {
                        json!({"encoded":stringify(&value,usize::MAX).unwrap()})
                    } else {
                        wire(value)
                    }
                }),
            "retention" | "pruneRevisions" => retention::native_retention(
                &mut store,
                step,
                runtime.expect("Retention runtime observations"),
            )
            .map(wire),
            "register" => {
                let mut definition = IvyJson::from_serde(&step["definition"]).unwrap();
                if let Some(fields) = step["definitionUnits"].as_object() {
                    for (field, units) in fields {
                        definition.insert(
                            field.as_str(),
                            IvyJson::String(JsString(
                                units
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|v| v.as_u64().unwrap() as u16)
                                    .collect(),
                            )),
                        );
                    }
                }
                ivy_hive::objects::register(
                    &mut store,
                    &definition,
                    &IvyJson::from_serde(&step["owner"]).unwrap(),
                )
                .map(|value| {
                    if step["lossless"] == true {
                        json!({"encoded":stringify(&value,1<<26).unwrap()})
                    } else {
                        wire(value)
                    }
                })
            }
            "registerAgent" => {
                let credential = &step["context"]["credentialDigest"];
                store
                    .authenticate(&AuthContext {
                        credential_digest: credential.as_str().unwrap().into(),
                        session_digest: None,
                    })
                    .and_then(|p| {
                        ivy_hive::objects::register_agent(
                            &mut store,
                            &AuthenticatedContext {
                                principal_id: p.principal_id,
                                credential_digest: p.credential_digest,
                            },
                            &IvyJson::from_serde(&step["params"]).unwrap(),
                            step["now"].as_u64().unwrap(),
                        )
                    })
                    .map(wire)
            }
            "getContract" => ivy_hive::objects::get_contract(
                &store,
                &step["key"].as_str().unwrap().into(),
                step["version"].as_str().map(JsString::from).as_ref(),
            )
            .map(wire),
            "decode" => {
                let mut content = IvyJson::from_serde(&step["content"]).unwrap();
                if let Some(units) = step["units"].as_array() {
                    content.insert(
                        "value",
                        IvyJson::String(JsString(
                            units.iter().map(|v| v.as_u64().unwrap() as u16).collect(),
                        )),
                    );
                }
                ivy_hive::objects::decode(&store,&IvyJson::from_serde(&step["contract"]).unwrap(),&content).map(|result| {
                    let (kind,data)=match result.data {ivy_hive::objects::ContentData::Text(value)=>("text",if result.byte_length<=1024 {json!(value)} else {Value::Null}),ivy_hive::objects::ContentData::Bytes(value)=>{use base64::Engine;("bytes",if result.byte_length<=1024 {json!(base64::engine::general_purpose::STANDARD.encode(value))} else {Value::Null})}};
                    json!({"kind":kind,"data":data,"byteLength":result.byte_length,"contentHash":result.content_hash})
                })
            }
            "identity" => identity::native_identity(&mut store, step).map(wire),
            "events" => events::native_event(&mut store, step).map(|value| {
                if step["lossless"] == true {
                    json!({"encoded":stringify(&value,1<<26).unwrap()})
                } else {
                    wire(value)
                }
            }),
            "write" => object_write::native_write(&mut store, step).map(wire),
            "readObject" => ivy_hive::objects::read(
                &store,
                ivy_hive::objects::Location::Id(&step["objectId"].as_str().unwrap().into()),
                step["revision"].as_u64(),
            )
            .map(wire),
            "configure" => store
                .configure_credentials(
                    &step["credentials"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| Credential {
                            principal_id: c["principalId"].as_str().unwrap().into(),
                            digest: c["digest"].as_str().unwrap().into(),
                        })
                        .collect::<Vec<_>>(),
                )
                .map(|_| Value::Null),
            "authenticate" => {
                let c = &step["context"];
                store.authenticate(&AuthContext{credential_digest:c["credentialDigest"].as_str().unwrap().into(),session_digest:c["sessionDigest"].as_str().map(JsString::from)}).map(|p|json!({"principalId":p.principal_id.to_utf8().unwrap(),"credentialDigest":p.credential_digest.to_utf8().unwrap()}))
            }
            "sql" => store
                .connection()
                .execute_batch(step["sql"].as_str().unwrap())
                .map_err(|_| IvyError::internal())
                .map(|_| Value::Null),
            "invalidate" => {
                store.invalidate(step["scope"].as_str().unwrap().into());
                Ok(Value::Null)
            }
            "transaction" => store.transaction(|s| {
                s.connection()
                    .execute_batch(step["sql"].as_str().unwrap())
                    .map_err(|_| IvyError::internal())?;
                s.invalidate(step["scope"].as_str().unwrap().into());
                if step["fail"] == true {
                    Err(IvyError::new("fixture_failure", "Rollback requested."))
                } else {
                    Ok(Value::Null)
                }
            }),
            "reopen" => {
                package_catalog.reset();
                package_kernel.reset();
                drop(store);
                store = Store::open(path).unwrap();
                registry.reset_owner();
                Ok(Value::Null)
            }
            "reorder" | "archive" | "delete" | "move" => {
                let c = &step["context"];
                store
                    .authenticate(&AuthContext {
                        credential_digest: c["credentialDigest"].as_str().unwrap().into(),
                        session_digest: c["sessionDigest"].as_str().map(JsString::from),
                    })
                    .and_then(|principal| {
                        let context = AuthenticatedContext {
                            principal_id: principal.principal_id,
                            credential_digest: principal.credential_digest,
                        };
                        let node = c["serviceNodeId"].as_str().map(JsString::from);
                        let mut params = IvyJson::from_serde(&step["params"]).unwrap();
                        if step["op"] == "move" {
                            object_write::apply_units(&mut params, step);
                        }
                        let now: JsString = step["nowText"].as_str().unwrap().into();
                        store.with_budget_mut(
                            if step["overdue"] == true {
                                std::time::Duration::ZERO
                            } else {
                                std::time::Duration::from_secs(10)
                            },
                            |store| {
                                if step["op"] == "move" {
                                    ivy_hive::objects::move_object(
                                        store,
                                        &context,
                                        &params,
                                        step["now"].as_u64().unwrap(),
                                        &now,
                                    )
                                } else if step["op"] == "reorder" {
                                    ivy_hive::objects::reorder(
                                        store,
                                        &context,
                                        &params,
                                        step["now"].as_u64().unwrap(),
                                        &now,
                                    )
                                } else if step["op"] == "archive" {
                                    ivy_hive::objects::archive(
                                        store,
                                        &context,
                                        node.as_ref(),
                                        &params,
                                        step["now"].as_u64().unwrap(),
                                        &now,
                                    )
                                } else {
                                    ivy_hive::objects::delete(
                                        store,
                                        &context,
                                        &params,
                                        step["now"].as_u64().unwrap(),
                                        &now,
                                    )
                                }
                            },
                        )
                    })
                    .map(wire)
            }
            "mutate" => {
                let kind = match step["kind"].as_str() {
                    Some("contract") => ReceiptKind::Contract,
                    Some("event") => ReceiptKind::Event,
                    _ => ReceiptKind::Snapshot,
                };
                store
                    .mutate(
                        &context(&step["context"]),
                        &step["method"].as_str().unwrap().into(),
                        &IvyJson::from_serde(&step["params"]).unwrap(),
                        step["now"].as_u64().unwrap(),
                        |s, _| {
                            s.connection()
                                .execute_batch(step["sql"].as_str().unwrap_or(""))
                                .map_err(|_| IvyError::internal())?;
                            if let Some(scope) = step["scope"].as_str() {
                                s.invalidate(scope.into());
                            }
                            if step["fail"] == true {
                                return Err(IvyError::new(
                                    "fixture_failure",
                                    "Rollback requested.",
                                ));
                            }
                            Ok(LedgerResult {
                                value: IvyJson::from_serde(&step["value"]).unwrap(),
                                kind,
                                reference: step
                                    .get("reference")
                                    .map(|v| IvyJson::from_serde(v).unwrap()),
                            })
                        },
                    )
                    .map(wire)
            }
            "receipt" => store
                .object_write_receipt(
                    &context(&step["context"]),
                    &step["mutationId"].as_str().unwrap().into(),
                    step["hash"].as_str().unwrap(),
                )
                .map(|v| v.map(wire).unwrap_or(Value::Null)),
            "contract" => store
                .contract(
                    &step["key"].as_str().unwrap().into(),
                    &step["version"].as_str().unwrap().into(),
                )
                .map(wire),
            "cleanup" => store
                .transaction(|s| {
                    s.cleanup_mutation_receipts(
                        step["bytes"].as_u64().unwrap_or(0) as usize,
                        step["count"].as_u64().unwrap_or(0) as usize,
                    )
                })
                .map(|_| Value::Null),
            "adjust" => store
                .adjust_retention_usage(
                    &step["key"].as_str().unwrap().into(),
                    step["objects"].as_i64().unwrap(),
                    step["revisions"].as_i64().unwrap(),
                    step["bytes"].as_i64().unwrap(),
                )
                .map(|_| Value::Null),
            "event" => store
                .transaction(|s| {
                    s.append_core_event(
                        &Principal {
                            principal_id: step["principal"]["principalId"].as_str().unwrap().into(),
                            credential_digest: "unused".into(),
                        },
                        &step["objectId"].as_str().unwrap().into(),
                        step["revision"].as_u64().unwrap(),
                        &step["operation"].as_str().unwrap().into(),
                        &step["mutationId"].as_str().unwrap().into(),
                        &step["now"].as_str().unwrap().into(),
                        step["key"].as_str().map(JsString::from).as_ref(),
                    )
                })
                .map(|_| Value::Null),
            "admit" => store
                .transaction(|s| s.admit_event(step["reserve"].as_bool().unwrap_or(false)))
                .map(|_| Value::Null),
            "durableBoundary" => store.durable_event_boundary().map(|v| json!(v)),
            _ => panic!("unknown step"),
        };
        let (value, error) = match outcome {
            Ok(v) => (v, Value::Null),
            Err(e) => (
                Value::Null,
                serde_json::from_str(&stringify(&e.to_wire(), 1 << 20).unwrap()).unwrap(),
            ),
        };
        let mut row = json!({"value":value,"error":error,"state":snapshot(&store,step)});
        if package_trace {
            row["catalogMutations"] =
                rows(&store, "SELECT mutation_id FROM mutations ORDER BY rowid");
        }
        if step["op"] == "uiFiles" || step["op"] == "uis" {
            row["fileTree"] = ui_files::tree(&path.with_extension("files"));
            row["state"]["appAssets"] = rows(
                &store,
                "SELECT * FROM app_assets ORDER BY app_id,release_id,path",
            );
        }
        if inventory_trace {
            row["state"]["inventorySets"] = rows(
                &store,
                "SELECT * FROM inventory_sets ORDER BY node_id,namespace,kind",
            );
            row["state"]["inventory"] = if step["compactInventory"] == true {
                let encoded:String=store.connection().query_row("SELECT json_group_array(json_array(record_id,node_id,namespace,kind,native_id,schema_version,summary,observed_at)) FROM (SELECT rowid AS record_id,* FROM inventory ORDER BY rowid)",[],|r|r.get(0)).unwrap();
                json!(ivy_contracts::hash::digest(encoded))
            } else {
                rows(
                    &store,
                    "SELECT * FROM inventory ORDER BY node_id,namespace,kind,native_id",
                )
            };
        }
        if registry_trace {
            row["registry"] = registry.state();
        }
        if step["op"] == "wiki" {
            row["wikiCalls"] = wire(IvyJson::Array(wiki_calls));
        }
        result.push(row);
    }
    json!({"rows":result,"definitions":rows(&store,"SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name"),"metadata":rows(&store,"SELECT * FROM metadata WHERE key NOT IN ('cursor_secret','runtime_epoch') ORDER BY key"),"topics":rows(&store,"SELECT * FROM topics ORDER BY topic,version")})
}
fn oracle(path: &Path, steps: &[Value]) -> Value {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-store-oracle.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!({"path":path,"steps":steps}).to_string().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

#[test]
fn schema_authentication_credential_rotation_and_rollback_match_original_store() {
    let root = Root::new();
    let a = format!("sha256:{}", "a".repeat(64));
    let b = format!("sha256:{}", "b".repeat(64));
    let c = format!("sha256:{}", "c".repeat(64));
    let mut steps = vec![
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":a},{"principalId":"beta","digest":b}]}),
        json!({"op":"authenticate","context":{"credentialDigest":a}}),
        json!({"op":"sql","sql":format!("INSERT INTO service_nodes VALUES('node','beta','host','fixture',1,'{{\"connected\":true,\"synced\":true,\"ready\":true}}'); INSERT INTO subscriptions VALUES('node','sub','{{}}',0,0,1,'{{}}'); INSERT INTO durable_subscriptions VALUES('node','sub'); INSERT INTO browser_sessions VALUES('session','{a}','2026-01-01T00:00:00Z');")}),
        json!({"op":"authenticate","context":{"credentialDigest":a,"sessionDigest":"session"}}),
        json!({"op":"authenticate","context":{"credentialDigest":b,"sessionDigest":"session"}}),
        json!({"op":"authenticate","context":{"credentialDigest":a,"sessionDigest":""}}),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":c}]}),
        json!({"op":"authenticate","context":{"credentialDigest":a,"sessionDigest":"session"}}),
        json!({"op":"authenticate","context":{"credentialDigest":c}}),
        json!({"op":"configure","credentials":[{"principalId":"different","digest":c}]}),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":a},{"principalId":"","digest":b}]}),
        json!({"op":"configure","credentials":[{"principalId":"🙂".repeat(101),"digest":a}]}),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":"sha256:ABC"}]}),
        json!({"op":"invalidate","scope":"before"}),
        json!({"op":"transaction","scope":"discard","sql":"INSERT INTO principals VALUES('rollback');","fail":true}),
        json!({"op":"transaction","scope":"commit","sql":"INSERT INTO principals VALUES('committed');","fail":false}),
        json!({"op":"reopen"}),
    ];
    for n in 0..66 {
        steps.push(json!({"op":"invalidate","scope":format!("scope/{n}")}));
    }
    steps.extend([
        json!({"op":"configure","credentials":[]}),
        json!({"op":"authenticate","context":{"credentialDigest":c}}),
    ]);
    let expected = oracle(&root.0.join("reference.sqlite"), &steps);
    let actual = native(&root.0.join("native.sqlite"), &steps);
    for (index, (actual, expected)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(actual, expected, "step {index}: {}", steps[index]);
    }
    assert_eq!(actual["definitions"], expected["definitions"]);
    assert_eq!(actual["metadata"], expected["metadata"]);
    assert_eq!(actual["topics"], expected["topics"]);
}

#[test]
fn transaction_commit_failure_restores_invalidations_and_exclusive_writer_blocks_second_owner() {
    let root = Root::new();
    let path = root.0.join("store.sqlite");
    let mut store = Store::open(&path).unwrap();
    store.invalidate("before".into());
    let error=store.transaction(|s|{s.connection().execute_batch("PRAGMA defer_foreign_keys=ON;INSERT INTO credentials VALUES('bad','missing',0);").map_err(|_|IvyError::internal())?;s.invalidate("after".into());Ok(())}).unwrap_err();
    assert_eq!(error.code, "internal_error");
    assert!(store.connection().is_autocommit());
    assert_eq!(store.changes(), &[JsString::from("before")]);
    assert_eq!(rows(&store, "SELECT * FROM credentials"), json!([]));
    assert!(Store::open(&path).is_err());
    let secret = store.metadata("cursor_secret").unwrap().unwrap();
    let epoch = store.metadata("runtime_epoch").unwrap().unwrap();
    assert_eq!(secret.len(), 64);
    assert!(
        secret
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(uuid::Uuid::parse_str(&epoch).unwrap().get_version_num(), 4);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.metadata("cursor_secret").unwrap(), Some(secret));
    assert_eq!(store.metadata("runtime_epoch").unwrap(), Some(epoch));
}

#[test]
fn checked_in_ddl_remains_exactly_the_reference_format_four_schema() {
    let source = include_str!("../../services/hive/src/storage-schema.ts");
    assert_eq!(
        include_str!("../../services/hive/rust/src/schema.sql"),
        source.split('`').nth(1).unwrap()
    );
}

#[test]
fn unsupported_clean_database_refusal_preserves_bytes_and_sidecars() {
    let root = Root::new();
    for format in [-1i64, 1, 3, 5, 99, i32::MAX as i64] {
        for journal in ["DELETE", "WAL"] {
            let path = root
                .0
                .join(format!("unsupported-{format}-{journal}.sqlite"));
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(&format!("PRAGMA journal_mode={journal}; PRAGMA user_version={format}; CREATE TABLE retained(value TEXT); INSERT INTO retained VALUES('unchanged');")).unwrap();
            drop(db);
            let observe = || {
                ["", "-wal", "-shm", "-journal"].map(|suffix| {
                    let p = PathBuf::from(format!("{}{suffix}", path.display()));
                    match fs::read(p) {
                        Ok(bytes) => Some(bytes),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => panic!("{e}"),
                    }
                })
            };
            let before = observe();
            let error = match Store::open(&path) {
                Ok(_) => panic!("Unsupported format admitted"),
                Err(error) => error,
            };
            assert_eq!(
                error.to_wire(),
                IvyError::new(
                    "unsupported_storage_format",
                    "This binary cannot read the database format."
                )
                .to_wire()
            );
            assert_eq!(observe(), before, "format {format}, journal {journal}");
        }
    }
}

#[test]
fn historical_writable_store_matches_original_initialization_and_all_retained_rows() {
    let root = Root::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/hive-current.sqlite.gz");
    for legacy in [false, true] {
        let reference = root.0.join(format!("reference-{legacy}.sqlite"));
        let path = root.0.join(format!("native-{legacy}.sqlite"));
        let status = Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .args(["--input-type=module", "-e", "import {readFileSync,writeFileSync} from 'node:fs';import {gunzipSync} from 'node:zlib';writeFileSync(process.argv[2],gunzipSync(readFileSync(process.argv[1])));"])
            .arg(&fixture).arg(&reference).status().unwrap();
        assert!(status.success());
        if legacy {
            let db = rusqlite::Connection::open(&reference).unwrap();
            db.execute_batch("DROP TABLE IF EXISTS durable_subscriptions; CREATE INDEX IF NOT EXISTS mutations_expiry ON mutations(principal_id);").unwrap();
        }
        fs::copy(&reference, &path).unwrap();
        let steps = [json!({"op":"reopen"})];
        assert_eq!(native(&path, &steps), oracle(&reference, &steps));
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("ATTACH ? AS reference", [reference.to_str().unwrap()])
            .unwrap();
        let tables = db
            .prepare("SELECT name FROM main.sqlite_schema WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(tables.len() > 10);
        for table in tables {
            let quoted = table.replace('"', "\"\"");
            for (left, right) in [("main", "reference"), ("reference", "main")] {
                let count: i64 = db.query_row(&format!("SELECT count(*) FROM (SELECT * FROM {left}.\"{quoted}\" EXCEPT SELECT * FROM {right}.\"{quoted}\")"), [], |r|r.get(0)).unwrap();
                assert_eq!(count, 0, "legacy={legacy}: {left}.{table}");
            }
        }
    }
}
