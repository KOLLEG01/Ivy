use super::*;

fn diagnostic_identity(value: &IvyJson) -> IvyResult<String> {
    hash_json(&IvyJson::object([
        ("code", field(value, "code")?.clone()),
        ("resource", field(value, "resource")?.clone()),
        ("source", field(value, "source")?.clone()),
    ]))
}
fn invalidate(store: &mut Store, value: &IvyJson) -> IvyResult<()> {
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
    store.invalidate(if let Some(service) = service {
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
    });
    Ok(())
}
impl Registry {
    pub fn diagnostic(
        &mut self,
        store: &mut Store,
        value: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<()> {
        self.commit_diagnostics(store, std::slice::from_ref(value), runtime, now)
    }
    pub fn diagnostics(&self, store: &Store, status: Option<&JsString>) -> IvyResult<Vec<IvyJson>> {
        let mut statement = sql(
            store,
            store
                .connection()
                .prepare_cached("SELECT diagnostic_json FROM diagnostics ORDER BY identity"),
        )?;
        let mut result = vec![];
        for row in sql(store, statement.query_map([], |r| r.get::<_, String>(0)))? {
            let value = decode(&sql(store, row)?)?;
            if status.is_some_and(|v| !v.0.is_empty()) {
                require_object(&value)?;
            }
            if match status {
                Some(s) if s == &JsString::from("unresolved") => {
                    !is_text(value.get("status"), "resolved")
                }
                Some(s) if !s.0.is_empty() => {
                    value.get("status").and_then(IvyJson::as_js_string) == Some(s)
                }
                _ => true,
            } {
                result.push(value);
            }
        }
        Ok(result)
    }
    pub fn collect_diagnostics(
        &mut self,
        store: &mut Store,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<()> {
        self.commit_diagnostics(store, &[], runtime, now)
    }
    fn commit_diagnostics(
        &mut self,
        store: &mut Store,
        values: &[IvyJson],
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<()> {
        let mut updates = vec![];
        let mut identities = HashSet::new();
        for value in values {
            validate_shared("Diagnostic", value)?;
            crate::paging::utc(field(value, "firstObservedAt")?, runtime)?;
            crate::paging::utc(field(value, "lastObservedAt")?, runtime)?;
            let identity = diagnostic_identity(value)?;
            require(
                identities.insert(identity.clone()),
                "invalid_arguments",
                "A diagnostic identity may be reported once per heartbeat.",
            )?;
            updates.push((identity, value.clone()));
        }
        let work = |store: &mut Store| -> IvyResult<()> {
            let mut retained = {
                let mut statement = sql(
                    store,
                    store
                        .connection()
                        .prepare_cached("SELECT identity,diagnostic_json FROM diagnostics"),
                )?;
                sql(
                    store,
                    statement
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))),
                )?
                .map(|r| {
                    let (id, value) = sql(store, r)?;
                    Ok((id, decode(&value)?))
                })
                .collect::<IvyResult<Vec<_>>>()?
            };
            let mut indices: HashMap<String, usize> = retained
                .iter()
                .enumerate()
                .map(|(i, (id, _))| (id.clone(), i))
                .collect();
            let originals: HashMap<_, _> = retained.iter().cloned().collect();
            for (id, value) in &updates {
                let mut value = value.clone();
                if let Some(index) = indices.get(id) {
                    let prior = &retained[*index].1;
                    if let Some(first) = prior.get("firstObservedAt").filter(|v| !v.is_null()) {
                        value.insert("firstObservedAt", first.clone());
                    }
                    retained[*index].1 = value;
                } else {
                    indices.insert(id.clone(), retained.len());
                    retained.push((id.clone(), value));
                }
            }
            let mut active = 0;
            for (_, value) in &retained {
                require_object(value)?;
                if !is_text(value.get("status"), "resolved") {
                    active += 1;
                }
            }
            require(
                active <= ACTIVE_DIAGNOSTIC_LIMIT,
                "limit_exceeded",
                "Active diagnostic capacity is full.",
            )?;
            let mut resolved: Vec<_> = retained
                .iter()
                .filter(|(_, v)| is_text(v.get("status"), "resolved"))
                .collect();
            let mut sort_error = None;
            resolved.sort_by(|(left_id, left), (right_id, right)| {
                let value: IvyResult<Ordering> = (|| {
                    let order = runtime.compare_contract_keys(
                        string(left, "lastObservedAt")?,
                        &string(right, "lastObservedAt")?.clone(),
                    );
                    Ok(if order == Ordering::Equal {
                        runtime.compare_contract_keys(
                            &left_id.as_str().into(),
                            &right_id.as_str().into(),
                        )
                    } else {
                        order
                    })
                })();
                match value {
                    Ok(value) => value,
                    Err(error) => {
                        sort_error = Some(error);
                        Ordering::Equal
                    }
                }
            });
            if let Some(error) = sort_error {
                return Err(error);
            }
            let mut removed = vec![];
            let mut removed_set = HashSet::new();
            for (id, value) in &resolved {
                if runtime.parse_date(&string(value, "lastObservedAt")?.clone())
                    <= now - 7.0 * 86_400_000.0
                {
                    removed_set.insert(id.as_str());
                    removed.push(id.as_str());
                }
            }
            let excess = resolved
                .len()
                .saturating_sub(removed.len() + RESOLVED_DIAGNOSTIC_LIMIT);
            for (id, _) in resolved
                .iter()
                .filter(|(id, _)| !removed_set.contains(id.as_str()))
                .take(excess)
            {
                removed.push(id.as_str());
            }
            for id in &removed {
                invalidate(store, &retained[indices[*id]].1)?;
                sql(
                    store,
                    store
                        .connection()
                        .execute("DELETE FROM diagnostics WHERE identity=?", [id]),
                )?;
            }
            let removed: HashSet<_> = removed.into_iter().collect();
            for (id, _) in &updates {
                if removed.contains(id.as_str()) {
                    continue;
                }
                let next = &retained[indices[id]].1;
                let prior = originals.get(id);
                if prior.is_none_or(|prior| {
                    ["status", "message", "severity"]
                        .iter()
                        .any(|key| prior.get(key) != next.get(key))
                }) {
                    invalidate(store, next)?;
                }
                let encoded = encoded(next)?;
                if prior.map(encoded_original).transpose()?.as_deref() != Some(&encoded) {
                    sql(store,store.connection().execute("INSERT INTO diagnostics VALUES (?,?) ON CONFLICT(identity) DO UPDATE SET diagnostic_json=excluded.diagnostic_json",params![id,encoded]))?;
                }
            }
            Ok(())
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
        self.contact_times.insert(id.clone(), at.clone());
        store.transaction(|store| {
            let mut node = self.node(store, id, runtime, now)?;
            require(
                parameters.get("ready").and_then(IvyJson::as_bool) != Some(true)
                    || node.get("synced").and_then(IvyJson::as_bool) == Some(true),
                "service_not_ready",
                "Full registry sync is required before readiness.",
            )?;
            let source = join(["service:".into(), id.clone()]);
            let mut reported = HashSet::new();
            let mut updates = vec![];
            for value in array(parameters, "diagnostics")? {
                require_object(value)?;
                let mut value = value.clone();
                value.insert("source", IvyJson::String(source.clone()));
                reported.insert(diagnostic_identity(&value)?);
                updates.push(value);
            }
            {
                let mut statement = sql(
                    store,
                    store
                        .connection()
                        .prepare_cached("SELECT identity,diagnostic_json FROM diagnostics"),
                )?;
                for row in sql(
                    store,
                    statement
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))),
                )? {
                    let (identity, value) = sql(store, row)?;
                    let mut value = decode(&value)?;
                    require_object(&value)?;
                    if value.get("source").and_then(IvyJson::as_js_string) == Some(&source)
                        && is_text(value.get("status"), "current")
                        && !reported.contains(&identity)
                    {
                        value.insert("lastObservedAt", IvyJson::String(at.clone()));
                        value.insert("status", "resolved".into());
                        updates.push(value);
                    }
                }
            }
            self.commit_diagnostics(store, &updates, runtime, now)?;
            if node.get("ready") != parameters.get("ready") {
                node.insert("ready", field(parameters, "ready")?.clone());
                node.insert("lastContactAt", IvyJson::String(at.clone()));
                self.save(store, &node, true)?;
            }
            if parameters.get("ready").and_then(IvyJson::as_bool) == Some(true) {
                if let Some(registry) = self.get_registry(store, id)? {
                    for required in array(&registry, "requiredContracts")? {
                        for version in array(required, "writeVersions")? {
                            let contract = store.contract(
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
        })
    }
}
fn encoded_original(value: &IvyJson) -> IvyResult<String> {
    encoded(value)
}
