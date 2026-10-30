use super::*;

fn diagnostic_identity(value: &IvyJson) -> IvyResult<String> {
    hash_json(&IvyJson::object([
        ("code", field(value, "code")?.clone()),
        ("resource", field(value, "resource")?.clone()),
        ("source", field(value, "source")?.clone()),
    ]))
}
fn diagnostic_scope(store: &Store, value: &IvyJson) -> IvyResult<JsString> {
    let resource = value.get("resource");
    let id = resource
        .filter(|v| v.as_object().is_some())
        .and_then(|v| v.get("serviceNodeId"))
        .filter(|v| !v.is_null())
        .cloned()
        .or_else(|| {
            value
                .get("source")
                .and_then(IvyJson::as_js_string)
                .filter(|v| {
                    v.0.starts_with(&"service:".encode_utf16().collect::<Vec<_>>())
                })
                .map(|v| IvyJson::String(JsString(v.0[8..].to_vec())))
        });
    let service = if let Some(id) = id.as_ref().and_then(IvyJson::as_js_string) {
        sql(
            store,
            store
                .connection()
                .query_row(
                    "SELECT service_name FROM service_nodes WHERE id=?",
                    [text(id)],
                    |r| r.get::<_, String>(0),
                )
                .optional(),
        )?
    } else {
        None
    };
    Ok(if let Some(service) = service {
        join([
            "services/".into(),
            service.into(),
            "/".into(),
            id.as_ref()
                .and_then(IvyJson::as_js_string)
                .ok_or_else(IvyError::internal)?
                .clone(),
        ])
    } else {
        "services".into()
    })
}

struct DiagnosticChange {
    identity: String,
    scope: JsString,
}

fn read_diagnostic(store: &Store, identity: &str) -> IvyResult<Option<IvyJson>> {
    sql(
        store,
        store
            .connection()
            .query_row(
                "SELECT diagnostic_json FROM diagnostics WHERE identity=?",
                [identity],
                |row| row.get::<_, String>(0),
            )
            .optional(),
    )?
    .map(|value| decode(&value))
    .transpose()
}

fn validate_diagnostic(value: &IvyJson, runtime: &dyn Runtime) -> IvyResult<()> {
    validate_shared("Diagnostic", value)?;
    crate::paging::utc(field(value, "firstObservedAt")?, runtime)?;
    crate::paging::utc(field(value, "lastObservedAt")?, runtime)?;
    Ok(())
}

fn report_diagnostic(
    store: &mut Store,
    identity: String,
    mut value: IvyJson,
    changes: &mut Vec<DiagnosticChange>,
) -> IvyResult<()> {
    let prior = read_diagnostic(store, &identity)?;
    if let Some(first) = prior
        .as_ref()
        .and_then(|v| v.get("firstObservedAt"))
        .filter(|v| !v.is_null())
    {
        value.insert("firstObservedAt", first.clone());
    }
    let notify = prior.as_ref().is_none_or(|prior| {
        ["status", "message", "severity"]
            .iter()
            .any(|key| prior.get(key) != value.get(key))
    });
    let next = encoded(&value)?;
    if prior.as_ref().map(encoded).transpose()?.as_deref() != Some(next.as_str()) {
        sql(store, store.connection().execute(
            "INSERT INTO diagnostics VALUES (?,?) ON CONFLICT(identity) DO UPDATE SET diagnostic_json=excluded.diagnostic_json",
            params![identity, next],
        ))?;
    }
    if notify {
        changes.push(DiagnosticChange {
            identity,
            scope: diagnostic_scope(store, &value)?,
        });
    }
    Ok(())
}

fn current_diagnostic_ids(store: &Store, source: &JsString) -> IvyResult<Vec<String>> {
    // SQLite strings can select ordinary sources directly. Keep the lossless
    // boundary for lone surrogates and NUL by checking the selected JSON below.
    let source = source.to_utf8().ok().filter(|v| !v.contains('\0'));
    let query = if source.is_some() {
        "SELECT identity FROM diagnostics WHERE json_extract(diagnostic_json,'$.source')=? AND json_extract(diagnostic_json,'$.status')='current' ORDER BY rowid"
    } else {
        "SELECT identity FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')='current' ORDER BY rowid"
    };
    let mut statement = sql(store, store.connection().prepare_cached(query))?;
    sql(
        store,
        statement.query_map(rusqlite::params_from_iter(source.iter()), |row| {
            row.get::<_, String>(0)
        }),
    )?
    .map(|row| sql(store, row))
    .collect()
}

fn check_diagnostic_capacity(store: &Store) -> IvyResult<()> {
    let active: i64 = sql(store, store.connection().query_row(
        "SELECT count(*) FROM diagnostics WHERE json_extract(diagnostic_json,'$.status') IS NOT 'resolved'", [],
        |row| row.get(0),
    ))?;
    require(
        active <= ACTIVE_DIAGNOSTIC_LIMIT as i64,
        "limit_exceeded",
        "Active diagnostic capacity is full.",
    )
}

fn remove_diagnostics(store: &mut Store, identities: Vec<String>) -> IvyResult<()> {
    for identity in identities {
        let value = read_diagnostic(store, &identity)?.ok_or_else(IvyError::internal)?;
        store.invalidate(diagnostic_scope(store, &value)?);
        sql(
            store,
            store
                .connection()
                .execute("DELETE FROM diagnostics WHERE identity=?", [identity]),
        )?;
    }
    Ok(())
}

fn prune_diagnostics(store: &mut Store, now: f64) -> IvyResult<()> {
    let expired = {
        let mut statement = sql(store, store.connection().prepare_cached(
            "SELECT identity FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')='resolved'
             AND julianday(json_extract(diagnostic_json,'$.lastObservedAt')) <= julianday(? / 1000.0,'unixepoch')
             ORDER BY json_extract(diagnostic_json,'$.lastObservedAt'),identity",
        ))?;
        sql(
            store,
            statement.query_map([now - 7.0 * 86_400_000.0], |row| row.get::<_, String>(0)),
        )?
        .map(|row| sql(store, row))
        .collect::<IvyResult<Vec<_>>>()?
    };
    remove_diagnostics(store, expired)?;
    let resolved: i64 = sql(store, store.connection().query_row(
        "SELECT count(*) FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')='resolved'", [],
        |row| row.get(0),
    ))?;
    let excess = resolved - RESOLVED_DIAGNOSTIC_LIMIT as i64;
    if excess > 0 {
        // Admitted UTC strings and SHA-256 identities have the same restricted
        // ASCII order here as the registry runtime's contract-key ordering.
        let oldest = {
            let mut statement = sql(store, store.connection().prepare_cached(
                "SELECT identity FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')='resolved'
                 ORDER BY json_extract(diagnostic_json,'$.lastObservedAt'),identity LIMIT ?",
            ))?;
            sql(
                store,
                statement.query_map([excess], |row| row.get::<_, String>(0)),
            )?
            .map(|row| sql(store, row))
            .collect::<IvyResult<Vec<_>>>()?
        };
        remove_diagnostics(store, oldest)?;
    }
    Ok(())
}

fn finish_diagnostics(
    store: &mut Store,
    changes: Vec<DiagnosticChange>,
    now: f64,
) -> IvyResult<()> {
    check_diagnostic_capacity(store)?;
    prune_diagnostics(store, now)?;
    for change in changes {
        let retained: bool = sql(
            store,
            store.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM diagnostics WHERE identity=?)",
                [change.identity],
                |row| row.get(0),
            ),
        )?;
        if retained {
            store.invalidate(change.scope);
        }
    }
    Ok(())
}
fn readiness_contract(store: &Store, key: &JsString, version: &JsString) -> IvyResult<IvyJson> {
    let value = sql(store, store.connection().query_row(
        "SELECT json_object('key',definition -> '$.key','version',definition -> '$.version',
             'owner',definition -> '$.owner','retention',definition -> '$.retention')
         FROM contracts WHERE key=? AND version=?", params![text(key), text(version)],
        |row| row.get::<_, String>(0),
    ).optional())?.ok_or_else(|| IvyError::new("contract_not_found", "The exact Data Contract is not registered."))?;
    decode(&value)
}
impl Registry {
    pub fn diagnostic(
        &mut self,
        store: &mut Store,
        value: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<()> {
        validate_diagnostic(value, runtime)?;
        let identity = diagnostic_identity(value)?;
        let work = |store: &mut Store| {
            let mut changes = Vec::new();
            report_diagnostic(store, identity, value.clone(), &mut changes)?;
            finish_diagnostics(store, changes, now)
        };
        if store.connection().is_autocommit() {
            store.transaction(work)
        } else {
            work(store)
        }
    }
    pub fn diagnostics(&self, store: &Store, status: Option<&JsString>) -> IvyResult<Vec<IvyJson>> {
        let status = status.filter(|value| !value.0.is_empty());
        let query = match status {
            Some(value) if value == &JsString::from("unresolved") => {
                "SELECT diagnostic_json FROM diagnostics WHERE json_extract(diagnostic_json,'$.status') IS NOT 'resolved' ORDER BY identity"
            }
            Some(_) => {
                "SELECT diagnostic_json FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')=? ORDER BY identity"
            }
            None => "SELECT diagnostic_json FROM diagnostics ORDER BY identity",
        };
        let selected = status
            .filter(|value| *value != &JsString::from("unresolved"))
            .map(text);
        let mut statement = sql(store, store.connection().prepare_cached(query))?;
        let mut result = vec![];
        for row in sql(
            store,
            statement.query_map(rusqlite::params_from_iter(selected.iter()), |r| {
                r.get::<_, String>(0)
            }),
        )? {
            let value = decode(&sql(store, row)?)?;
            result.push(value);
        }
        Ok(result)
    }
    pub fn collect_diagnostics(
        &mut self,
        store: &mut Store,
        _runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<()> {
        let work = |store: &mut Store| -> IvyResult<()> {
            check_diagnostic_capacity(store)?;
            prune_diagnostics(store, now)
        };
        if store.connection().is_autocommit() {
            store.transaction(work)
        } else {
            work(store)
        }
    }
    pub fn heartbeat(
        &mut self,
        store: &mut Store,
        id: &JsString,
        generation: Option<f64>,
        parameters: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let at = iso(runtime, now)?;
        self.record_contact(id.clone(), at.clone());
        let ready = parameters.get("ready").and_then(IvyJson::as_bool) == Some(true);
        let acknowledge_contracts = ready && !self.contacts[id].contracts_acknowledged;
        let result = store.transaction(|store| {
            let mut node = self.node(store, id, runtime, now)?;
            require(
                parameters.get("ready").and_then(IvyJson::as_bool) != Some(true)
                    || node.get("synced").and_then(IvyJson::as_bool) == Some(true),
                "service_not_ready",
                "Full registry sync is required before readiness.",
            )?;
            let source = join(["service:".into(), id.clone()]);
            let mut reported = HashSet::new();
            let mut changes = vec![];
            for value in array(parameters, "diagnostics")? {
                require_object(value)?;
                let mut value = value.clone();
                value.insert("source", IvyJson::String(source.clone()));
                validate_diagnostic(&value, runtime)?;
                let identity = diagnostic_identity(&value)?;
                require(
                    reported.insert(identity.clone()),
                    "invalid_arguments",
                    "A diagnostic identity may be reported once per heartbeat.",
                )?;
                report_diagnostic(store, identity, value, &mut changes)?;
            }
            for identity in current_diagnostic_ids(store, &source)? {
                if reported.contains(&identity) {
                    continue;
                }
                let mut value =
                    read_diagnostic(store, &identity)?.ok_or_else(IvyError::internal)?;
                if value.get("source").and_then(IvyJson::as_js_string) != Some(&source) {
                    continue;
                }
                value.insert("lastObservedAt", IvyJson::String(at.clone()));
                value.insert("status", "resolved".into());
                validate_diagnostic(&value, runtime)?;
                sql(
                    store,
                    store.connection().execute(
                        "UPDATE diagnostics SET diagnostic_json=? WHERE identity=?",
                        params![encoded(&value)?, identity],
                    ),
                )?;
                changes.push(DiagnosticChange {
                    identity,
                    scope: diagnostic_scope(store, &value)?,
                });
            }
            // Quiet heartbeats leave age collection to periodic retention.
            if !reported.is_empty() || !changes.is_empty() {
                finish_diagnostics(store, changes, now)?;
            }
            if node.get("ready") != parameters.get("ready") {
                node.insert("ready", field(parameters, "ready")?.clone());
                node.insert("lastContactAt", IvyJson::String(at.clone()));
                self.save(store, &node, true)?;
            }
            if acknowledge_contracts {
                if let Some(requirements) = self.required_contracts(store, id)? {
                    for required in requirements.as_array().ok_or_else(IvyError::internal)? {
                        for version in array(required, "writeVersions")? {
                            let contract = readiness_contract(
                                store,
                                string(required, "key")?,
                                version.as_js_string().ok_or_else(IvyError::internal)?,
                            )?;
                            if contract.get("owner").and_then(|v| v.get("kind"))
                                != Some(&"service".into())
                                || contract.get("owner").and_then(|v| v.get("serviceName"))
                                    != node.get("serviceName")
                                || contract
                                    .get("retention")
                                    .and_then(|v| v.get("awaitOwnerReadiness"))
                                    .and_then(IvyJson::as_bool)
                                    != Some(true)
                            {
                                continue;
                            }
                            let key = format!(
                                "retention_owner_ready:{}:{}",
                                string(&contract, "key")?.to_utf8()?,
                                string(&contract, "version")?.to_utf8()?
                            );
                            let acknowledgement = hash_json(&IvyJson::object([
                                ("version", field(&contract, "version")?.clone()),
                                ("policy", field(&contract, "retention")?.clone()),
                            ]))?;
                            if store.metadata(&key)?.as_deref() != Some(acknowledgement.as_str()) {
                                store.set_metadata(&key, &acknowledgement)?;
                            }
                        }
                    }
                }
            }
            Ok(generation_result(
                generation,
                [
                    ("ready", field(parameters, "ready")?.clone()),
                    ("observedAt", IvyJson::String(at.clone())),
                ],
            ))
        });
        // A failed commit must leave the current catalog unacknowledged.
        if result.is_ok() {
            self.contacts.get_mut(id).unwrap().contracts_acknowledged = ready;
        }
        result
    }
}
