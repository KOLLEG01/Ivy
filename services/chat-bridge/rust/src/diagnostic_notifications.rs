//! Reconcile complete diagnostic snapshots with the original notice receipts.
//! Main supplies its existing notice/conversation/native-readiness operations;
//! the monitor owns neither native execution nor transport confirmation.
use crate::{
    diagnostics::{diagnostic_notice, grouped_notice_state, notice_key, retain_receipts},
    operations::ChatOperations,
    store::Document,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{
    clock::{Clock, SystemClock, automatic_notifications_allowed},
    operation_ids::new_operation_id,
    rpc::RequestOptions,
};
use std::{future::Future, pin::Pin, sync::Arc};

pub const DIAGNOSTIC_NOTIFICATION_DELAY_MS: i64 = 5 * 60_000;
pub const DIAGNOSTIC_NOTIFICATION_POLL_MS: i64 = 30_000;

pub type MainFuture<'a, T> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
/// The four existing Main boundaries used by operational notices. Implementations
/// must retain ChatNotices' original journal and native readiness semantics.
pub trait DiagnosticMain: Send + Sync {
    fn find(&self) -> MainFuture<'_, Option<Document>>;
    fn notify<'a>(&'a self, caller: &'a JsString, request: &'a Json) -> MainFuture<'a, Json>;
    fn notice<'a>(&'a self, caller: &'a JsString, query: &'a Json) -> MainFuture<'a, Json>;
    fn ready(&self) -> MainFuture<'_, ()>;
}

fn optional<'a>(value: &'a Json, key: &str) -> &'a Json {
    static MISSING: Json = Json::Null;
    value.get(key).unwrap_or(&MISSING)
}
fn flag(value: &Json, key: &str) -> bool {
    value.get(key).and_then(Json::as_bool) == Some(true)
}
fn number(value: &Json, key: &str) -> IvyResult<f64> {
    value.required(key)?.as_f64().ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn pending(request: Json) -> Json {
    Json::object([
        ("request", request),
        ("queued", false.into()),
        ("confirmed", false.into()),
    ])
}
// Join typed wire strings without coercing or reinterpreting their content.
fn concat(parts: &[&JsString]) -> JsString {
    JsString(parts.iter().flat_map(|p| p.0.iter().copied()).collect())
}

pub struct DiagnosticNotifications<'a> {
    operations: &'a ChatOperations<'a>,
    main: &'a dyn DiagnosticMain,
    language: JsString,
    clock: Arc<dyn Clock>,
}
impl<'a> DiagnosticNotifications<'a> {
    pub fn new(operations: &'a ChatOperations<'a>, main: &'a dyn DiagnosticMain) -> Self {
        Self::with_clock(operations, main, "en".into(), Arc::new(SystemClock))
    }
    pub fn with_clock(
        operations: &'a ChatOperations<'a>,
        main: &'a dyn DiagnosticMain,
        language: JsString,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            operations,
            main,
            language,
            clock,
        }
    }
    pub fn scope(&self) -> IvyResult<String> {
        let definition = self.operations.admission.definition();
        hash_json(&Json::Array(vec![
            definition.required("principalId")?.clone(),
            definition.required("workspaceId")?.clone(),
        ]))
    }
    fn check(&self, options: &RequestOptions) -> IvyResult<()> {
        // The live monitor belongs to its store's provider generation. A token
        // alone cannot retain the original connection close error.
        if let Some(connection) = self.operations.store.client.service_connection() {
            connection.throw_if_aborted()?;
        }
        if options.cancellation.is_cancelled() {
            Err(options.cancel_error.clone())
        } else {
            Ok(())
        }
    }
    async fn save(&self, scope: &str, state: &mut Json) -> IvyResult<()> {
        self.operations
            .store
            .save_diagnostic_notice_state(scope, state)
            .await
            .map_err(|e| e.into_ivy())
    }
    // Keep the current snapshot when a bounded save evicts a completed row.
    async fn save_entry(
        &self,
        scope: &str,
        state: &mut Json,
        key: &str,
        entry: &Json,
    ) -> IvyResult<()> {
        if state.get(key).is_some() {
            state.insert(key, entry.clone());
        }
        self.save(scope, state).await
    }
    /// Use the store's provider generation signal in production. Other callers
    /// supply the cancellation reason explicitly through `cancel_error`.
    pub async fn tick(&self, options: RequestOptions) -> IvyResult<()> {
        let store = self.operations.store;
        let mut current: Vec<(String, Json)> = Vec::new();
        let mut cursor = Json::Null;
        loop {
            let mut params = Json::object([
                ("status", "unresolved".into()),
                ("limit", Json::Number(100.0)),
            ]);
            if !cursor.is_null() {
                params.insert("cursor", cursor.clone());
            }
            let page = store
                .client
                .request("system.diagnostics", params, options.clone())
                .await?;
            for value in page
                .required("items")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                if value.get("severity") == Some(&Json::from("info"))
                    || value.get("code") == Some(&Json::from("chat_diagnostics_unavailable"))
                {
                    continue;
                }
                let notice = diagnostic_notice(value)?;
                let key = notice_key(&notice)?;
                if let Some((_, existing)) = current.iter_mut().find(|(k, _)| k == &key) {
                    *existing = notice;
                } else {
                    current.push((key, notice));
                }
            }
            cursor = optional(&page, "nextCursor").clone();
            if cursor.is_null() {
                break;
            }
        }
        // A failed/incomplete snapshot cannot discard a pending notice or imply recovery.
        self.check(&options)?;
        let scope = self.scope()?;
        let mut state = grouped_notice_state(
            &store
                .diagnostic_notice_state(&scope)
                .await
                .map_err(|e| e.into_ivy())?,
        )?;
        let now = self.clock.utc_millis() as f64;
        for (key, value) in &current {
            let prior = state.get(key).cloned();
            if prior
                .as_ref()
                .is_none_or(|p| !flag(p, "active") && flag(optional(p, "recovery"), "queued"))
            {
                if let Some(prior) = &prior {
                    retain_receipts(&mut state, prior, &[])?;
                }
                let mut entry = value.clone();
                entry.insert("firstSeen", Json::Number(now));
                entry.insert("active", true.into());
                entry.insert("resolvedAt", Json::Null);
                entry.insert("alert", Json::Null);
                entry.insert("recovery", Json::Null);
                state.insert(key.as_str(), entry);
            } else if let Some(mut prior) = prior.filter(|p| optional(p, "recovery").is_null()) {
                prior.insert("active", true.into());
                prior.insert("resolvedAt", Json::Null);
                state.insert(key.as_str(), prior);
            }
        }
        for (key, mut entry) in state.clone().into_object().ok_or_else(IvyError::internal)? {
            if flag(&entry, "receiptOnly") {
                continue;
            }
            let key = key.to_utf8()?;
            if !current.iter().any(|(k, _)| k == &key) {
                if optional(&entry, "alert").is_null() {
                    state.remove(&key);
                    continue;
                }
                entry.insert("active", false.into());
                if optional(&entry, "resolvedAt").is_null() {
                    entry.insert("resolvedAt", Json::Number(now));
                }
            }
            if flag(optional(&entry, "alert"), "confirmed")
                && flag(optional(&entry, "recovery"), "confirmed")
                && !optional(&entry, "resolvedAt").is_null()
                && now - number(&entry, "resolvedAt")? > 7.0 * 86_400_000.0
            {
                state.remove(&key);
            } else {
                state.insert(key, entry);
            }
        }
        self.save(&scope, &mut state).await?;
        if !automatic_notifications_allowed(store.client.as_ref(), now as i64, options.clone())
            .await?
        {
            if let Json::Object(entries) = &mut state {
                entries.retain(|(_, e)| {
                    flag(e, "receiptOnly")
                        || flag(e, "active")
                        || flag(optional(e, "recovery"), "queued")
                });
            }
            return self.save(&scope, &mut state).await;
        }
        for (key, mut entry) in state.clone().into_object().ok_or_else(IvyError::internal)? {
            self.check(&options)?;
            let key = key.to_utf8()?;
            if flag(&entry, "receiptOnly") {
                let kind = if !optional(&entry, "alert").is_null() {
                    "alert"
                } else {
                    "recovery"
                };
                let mut original = entry.required(kind)?.clone();
                if flag(&original, "queued") && !flag(&original, "confirmed") {
                    self.review(&mut original, &options).await?;
                }
                entry.insert(kind, original.clone());
                self.save_entry(&scope, &mut state, &key, &entry).await?;
                if !flag(&original, "queued") {
                    self.queue(&mut original, &options).await?;
                }
                entry.insert(kind, original.clone());
                if flag(&original, "confirmed") {
                    state.remove(&key);
                }
                self.save_entry(&scope, &mut state, &key, &entry).await?;
                continue;
            }
            if optional(&entry, "alert").is_null()
                && flag(&entry, "active")
                && now - number(&entry, "firstSeen")? >= DIAGNOSTIC_NOTIFICATION_DELAY_MS as f64
            {
                entry.insert("alert", pending(self.request(&entry, false).await?));
            }
            self.deliver(&scope, &mut state, &key, &mut entry, "alert", &options)
                .await?;
            if !flag(&entry, "active")
                && flag(optional(&entry, "alert"), "queued")
                && optional(&entry, "recovery").is_null()
                && !optional(&entry, "resolvedAt").is_null()
                && now - number(&entry, "resolvedAt")? >= DIAGNOSTIC_NOTIFICATION_DELAY_MS as f64
            {
                entry.insert("recovery", pending(self.request(&entry, true).await?));
            }
            self.deliver(&scope, &mut state, &key, &mut entry, "recovery", &options)
                .await?;
        }
        Ok(())
    }
    async fn deliver(
        &self,
        scope: &str,
        state: &mut Json,
        key: &str,
        entry: &mut Json,
        kind: &str,
        options: &RequestOptions,
    ) -> IvyResult<()> {
        let mut original = optional(entry, kind).clone();
        if original.is_null() {
            return Ok(());
        }
        if flag(&original, "queued") && !flag(&original, "confirmed") {
            self.review(&mut original, options).await?;
            entry.insert(kind, original.clone());
            self.save_entry(scope, state, key, entry).await?;
        }
        if !flag(&original, "queued") {
            entry.insert(kind, original.clone());
            self.save_entry(scope, state, key, entry).await?;
            self.queue(&mut original, options).await?;
            entry.insert(kind, original);
            self.save_entry(scope, state, key, entry).await?;
        }
        Ok(())
    }
    async fn request(&self, entry: &Json, recovery: bool) -> IvyResult<Json> {
        let main = self.main.find().await?;
        require(
            main.is_some(),
            "chat_main_missing",
            "Operational notifications need the configured Main conversation.",
        )?;
        let label = string(entry, "label")?;
        let code = string(entry, "code")?;
        let storage_warning = code == &JsString::from("storage_pressure") && label == &JsString::from("hive");
        let text = if self.language == JsString::from("de") {
            if recovery {
                concat(&[
                    &"[Ivy] Erholung festgestellt: ".into(),
                    label,
                    &" meldet den Fehler ".into(),
                    code,
                    &" nicht mehr.".into(),
                ])
            } else if storage_warning {
                "[Ivy] Speicherwarnung: Der freie Speicher von Hive liegt unter der Betriebsreserve. Hive arbeitet weiter.".into()
            } else {
                concat(&[&"[Ivy] Anhaltender Fehler bei ".into(), label, &": ".into(), code, &". Der Dienst oder ein abhängiger Abruf kann blockiert sein; gespeicherte Daten können veraltet sein.".into()])
            }
        } else if recovery {
            concat(&[
                &"[Ivy] Recovery observed: ".into(),
                label,
                &" no longer reports ".into(),
                code,
                &".".into(),
            ])
        } else if storage_warning {
            "[Ivy] Storage warning: Hive has less free disk space than its operating reserve. Hive continues processing requests.".into()
        } else {
            concat(&[
                &"[Ivy] Persistent failure at ".into(),
                label,
                &": ".into(),
                code,
                &". The service or a dependent read may be blocked; retained data may be outdated."
                    .into(),
            ])
        };
        Ok(Json::object([
            ("action", "notify".into()),
            (
                "operationId",
                Json::String(new_operation_id(self.operations.store.client.clone(), None).await?),
            ),
            (
                "source",
                main.unwrap().value.required("conversation")?.clone(),
            ),
            ("text", Json::String(text)),
        ]))
    }
    async fn queue(&self, original: &mut Json, options: &RequestOptions) -> IvyResult<()> {
        self.check(options)?;
        let caller = string(self.operations.admission.definition(), "principalId")?;
        let operation = self
            .main
            .notify(caller, original.required("request")?)
            .await?;
        require(
            operation.get("phase") != Some(&Json::from("failed")),
            "chat_operational_notice_failed",
            "The original operational notice failed before delivery.",
        )?;
        original.insert("queued", true.into());
        Ok(())
    }
    async fn review(&self, original: &mut Json, options: &RequestOptions) -> IvyResult<()> {
        self.check(options)?;
        let caller = string(self.operations.admission.definition(), "principalId")?;
        let request = original.required("request")?;
        let Some(operation) = self
            .operations
            .find(caller, string(request, "operationId")?)
            .await?
        else {
            return Ok(());
        };
        if operation.value.get("phase") == Some(&Json::from("failed")) {
            require(
                optional(&operation.value, "error").get("outcome")
                    == Some(&Json::from("not_executed")),
                "chat_operational_notice_failed",
                "Reconcile the original operational notice before retrying delivery.",
            )?;
            self.main.ready().await?;
            let mut request = request.clone();
            request.insert(
                "operationId",
                Json::String(new_operation_id(self.operations.store.client.clone(), None).await?),
            );
            original.insert("request", request);
            original.insert("queued", false.into());
        } else if operation.value.get("phase") == Some(&Json::from("succeeded")) {
            let query = Json::object([("operationId", request.required("operationId")?.clone())]);
            let reply = self.main.notice(caller, &query).await?;
            original.insert(
                "confirmed",
                (reply.required("data")?.get("state") == Some(&Json::from("confirmed"))).into(),
            );
        }
        Ok(())
    }
}
