//! UI publication, retained manifests, requirement inspection and private assets.
use crate::{
    paging::{catalog_boundary, catalog_page},
    registry::Registry,
    retention::Runtime,
    store::{AuthenticatedContext, LedgerResult, Store, sql_error},
    ui_files::UiFiles,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::{is_ui_metadata, is_ui_release},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};

fn sql<T>(store: &Store, result: rusqlite::Result<T>) -> IvyResult<T> {
    result.map_err(|e| store.query_error(e))
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn array<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a [IvyJson]> {
    field(value, key)?.as_array().ok_or_else(IvyError::internal)
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn nullable(value: Option<String>) -> IvyJson {
    value.map(IvyJson::from).unwrap_or(IvyJson::Null)
}
fn empty() -> IvyJson {
    IvyJson::object(std::iter::empty::<(&str, IvyJson)>())
}
fn metadata(ui: &JsString, encoded: &str) -> Option<IvyJson> {
    let value = parse(encoded, usize::MAX).ok()?;
    (is_ui_metadata(&value).ok()? && value.get("uiId").and_then(IvyJson::as_js_string) == Some(ui))
        .then_some(value)
}
fn identity(method: &str, parameters: &IvyJson) -> IvyResult<IvyJson> {
    let mut value = IvyJson::object([("method", method.into())]);
    for (key, item) in parameters.as_object().ok_or_else(IvyError::internal)? {
        if key != &JsString::from("cursor") {
            value.insert(key.clone(), item.clone());
        }
    }
    Ok(value)
}
fn cursor(parameters: &IvyJson) -> Option<&JsString> {
    parameters
        .get("cursor")
        .and_then(IvyJson::as_js_string)
        .filter(|s| !s.0.is_empty())
}
fn limit(parameters: &IvyJson) -> Option<usize> {
    parameters
        .get("limit")
        .and_then(IvyJson::as_f64)
        .map(|v| v as usize)
}
fn after(store: &Store, identity: &IvyJson, parameters: &IvyJson) -> IvyResult<JsString> {
    match cursor(parameters) {
        Some(c) => catalog_boundary(store, identity, c),
        None => Ok(JsString::default()),
    }
}
fn catalog_key(summary: &IvyJson) -> JsString {
    let value = summary.get("metadata").expect("validated UI metadata");
    let priority = value
        .get("priority")
        .and_then(IvyJson::as_f64)
        .unwrap_or(0.0) as i64;
    let mut key: JsString = format!("{:07}:", 1_000_000 - priority).into();
    key.0.extend(
        &value
            .get("uiId")
            .and_then(IvyJson::as_js_string)
            .expect("validated UI id")
            .0,
    );
    key
}
fn mime_type(value: Option<&IvyJson>) -> bool {
    value.and_then(IvyJson::as_js_string).is_some_and(|s| {
        [
            "text/html",
            "text/css",
            "text/javascript",
            "text/plain",
            "application/javascript",
            "application/json",
            "application/wasm",
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "image/svg+xml",
            "image/x-icon",
            "font/woff",
            "font/woff2",
        ]
        .iter()
        .any(|v| s == &JsString::from(*v))
    })
}
fn retained(store: &Store, ui: &JsString, release: &JsString) -> IvyResult<Option<String>> {
    sql(
        store,
        store
            .connection()
            .query_row(
                "SELECT release_json FROM app_releases WHERE app_id=? AND release_id=?",
                params![text(ui), text(release)],
                |r| r.get(0),
            )
            .optional(),
    )
}
fn releases(store: &Store, ui: &JsString) -> IvyResult<Vec<IvyJson>> {
    let mut query = sql(
        store,
        store.connection().prepare_cached(
            "SELECT release_json FROM app_releases WHERE app_id=? ORDER BY release_id",
        ),
    )?;
    let rows = sql(
        store,
        query.query_map([text(ui)], |r| r.get::<_, String>(0)),
    )?;
    let mut values = vec![];
    for row in rows {
        if let Ok(value) = parse(&sql(store, row)?, usize::MAX) {
            if is_ui_release(&value).unwrap_or(false) {
                values.push(value);
            }
        }
    }
    Ok(values)
}
struct Pointer {
    metadata: String,
    current: Option<String>,
}
fn pointer(store: &Store, ui: &JsString) -> IvyResult<Pointer> {
    sql(
        store,
        store
            .connection()
            .query_row(
                "SELECT metadata,current_release_id FROM apps WHERE app_id=?",
                [text(ui)],
                |r| {
                    Ok(Pointer {
                        metadata: r.get(0)?,
                        current: r.get(1)?,
                    })
                },
            )
            .optional(),
    )?
    .ok_or_else(|| IvyError::new("not_found", "UI not found."))
}
fn current(store: &Store, ui: &JsString) -> IvyResult<Option<IvyJson>> {
    match pointer(store, ui) {
        Ok(row) => Ok(metadata(ui, &row.metadata).map(|metadata| {
            IvyJson::object([
                ("metadata", metadata),
                ("currentReleaseId", nullable(row.current)),
            ])
        })),
        Err(error) if error.code == "not_found" => Ok(None),
        Err(error) => Err(error),
    }
}
struct ServiceState {
    has_catalog: bool,
    compatible: bool,
    eligible: bool,
}
const RANKS: [&str; 6] = [
    "unselected",
    "missing",
    "invalid",
    "incompatible",
    "unavailable",
    "ready",
];
struct Inspection {
    value: IvyJson,
    status: usize,
    issues: Vec<IvyJson>,
    truncated: bool,
}
impl Inspection {
    fn issue(&mut self, status: usize, code: &str, message: &str, resource: IvyJson) {
        self.status = self.status.min(status);
        if self.issues.len() < 32 {
            self.issues.push(IvyJson::object([
                ("code", code.into()),
                ("message", message.into()),
                ("resource", resource),
            ]));
        } else {
            self.truncated = true;
        }
    }
    fn finish(mut self) -> IvyJson {
        self.value.insert("status", RANKS[self.status].into());
        self.value.insert("issues", IvyJson::Array(self.issues));
        self.value.insert("issuesTruncated", self.truncated.into());
        self.value
    }
}

pub struct Uis<'a> {
    registry: &'a Registry,
    files: &'a UiFiles,
}
impl<'a> Uis<'a> {
    pub fn new(registry: &'a Registry, files: &'a UiFiles) -> Self {
        Self { registry, files }
    }
    pub fn list(&self, store: &Store) -> IvyResult<Vec<IvyJson>> {
        let mut query = sql(
            store,
            store
                .connection()
                .prepare_cached("SELECT app_id FROM apps ORDER BY app_id"),
        )?;
        let ids = sql(store, query.query_map([], |r| r.get::<_, String>(0)))?;
        let mut result = vec![];
        for id in ids {
            let id: JsString = sql(store, id)?.into();
            if let Some(mut value) = current(store, &id)? {
                value.insert("releases", IvyJson::Array(releases(store, &id)?));
                result.push(value);
            }
        }
        Ok(result)
    }
    pub fn get(&self, store: &Store, ui: &JsString) -> IvyResult<IvyJson> {
        let mut value =
            current(store, ui)?.ok_or_else(|| IvyError::new("not_found", "UI not found."))?;
        value.insert("releases", IvyJson::Array(releases(store, ui)?));
        Ok(value)
    }
    pub fn catalog(&self, store: &Store, parameters: &IvyJson) -> IvyResult<IvyJson> {
        let identity = identity("uis.catalog", parameters)?;
        let mut after = text(&after(store, &identity, parameters)?);
        let maximum = limit(parameters).unwrap_or(50);
        let mut summaries = vec![];
        while summaries.len() <= maximum {
            store.check_budget()?;
            let mut query=sql(store,store.connection().prepare_cached("WITH navigation AS (SELECT a.*, printf('%07d', 1000000 - CASE WHEN json_valid(a.metadata) THEN COALESCE(json_extract(a.metadata,'$.priority'),0) ELSE 0 END) || ':' || a.app_id AS navigation_key FROM apps a) SELECT a.app_id,a.metadata,a.current_release_id,a.navigation_key, (SELECT COUNT(*) FROM app_releases r WHERE r.app_id=a.app_id) AS release_count FROM navigation a WHERE a.navigation_key > ? COLLATE BINARY ORDER BY a.navigation_key LIMIT ?"))?;
            let rows = sql(
                store,
                query.query_map(
                    params![after, (maximum + 1 - summaries.len()) as i64],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                        ))
                    },
                ),
            )?
            .collect::<rusqlite::Result<Vec<_>>>();
            let rows = sql(store, rows)?;
            if rows.is_empty() {
                break;
            }
            for (id, encoded, current, key, count) in rows {
                after = key;
                if let Some(value) = metadata(&id.into(), &encoded) {
                    summaries.push(IvyJson::object([
                        ("metadata", value),
                        ("currentReleaseId", nullable(current)),
                        ("releaseCount", IvyJson::Number(count as f64)),
                    ]));
                }
            }
        }
        catalog_page(
            store,
            &mut summaries,
            &identity,
            catalog_key,
            limit(parameters),
            cursor(parameters),
        )
    }
    pub fn resolve_slug(&self, store: &Store, slug: &JsString) -> IvyResult<Option<JsString>> {
        let row=sql(store,store.connection().query_row("SELECT app_id,metadata FROM apps WHERE CASE WHEN json_valid(metadata) THEN json_extract(metadata,'$.slug') END=?",[text(slug)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional())?;
        Ok(row.and_then(|(id, encoded)| {
            let id: JsString = id.into();
            metadata(&id, &encoded).map(|_| id)
        }))
    }
    pub fn releases(&self, store: &Store, parameters: &IvyJson) -> IvyResult<IvyJson> {
        let ui = string(parameters, "uiId")?;
        current(store, ui)?.ok_or_else(|| IvyError::new("not_found", "UI not found."))?;
        let identity = identity("uis.releases", parameters)?;
        let after = text(&after(store, &identity, parameters)?);
        let mut query=sql(store,store.connection().prepare_cached("SELECT release_id, json_extract(release_json,'$.entryPath'), json_array_length(release_json,'$.assets') FROM app_releases WHERE app_id=? AND release_id > ? COLLATE BINARY ORDER BY release_id LIMIT ?"))?;
        let rows = sql(
            store,
            query.query_map(
                params![
                    text(ui),
                    after,
                    (limit(parameters).unwrap_or(50) + 1) as i64
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, rusqlite::types::Value>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                },
            ),
        )?;
        let mut items = vec![];
        for row in rows {
            let (id, path, count) = sql(store, row)?;
            let path: IvyJson = match path {
                rusqlite::types::Value::Null => "null".into(),
                rusqlite::types::Value::Text(path) => path.into(),
                rusqlite::types::Value::Integer(value) => value.to_string().into(),
                rusqlite::types::Value::Real(value) => ivy_contracts::json::stringify(&IvyJson::Number(value), usize::MAX)?.into(),
                rusqlite::types::Value::Blob(_) => return Err(IvyError::internal()),
            };
            items.push(IvyJson::object([
                ("releaseId", id.into()),
                ("entryPath", path),
                ("assetCount", IvyJson::Number(count.unwrap_or(0) as f64)),
            ]));
        }
        catalog_page(
            store,
            &mut items,
            &identity,
            |v| {
                string(v, "releaseId")
                    .expect("retained release key")
                    .clone()
            },
            limit(parameters),
            cursor(parameters),
        )
    }
    fn service_states(
        &self,
        store: &Store,
        required: &IvyJson,
        runtime: &dyn Runtime,
        clock: &dyn Fn() -> f64,
    ) -> IvyResult<Vec<ServiceState>> {
        let mut result = vec![];
        for node in self.registry.nodes(store, runtime, clock())? {
            if required.is_null() {
                return Err(IvyError::internal());
            }
            if node.get("serviceName") != required.get("serviceName") {
                continue;
            }
            let catalog = self
                .registry
                .get_registry(store, string(&node, "serviceNodeId")?)?;
            let mut namespace = None;
            if let Some(catalog) = catalog.as_ref().filter(|v| !v.is_null()) {
                for value in array(catalog, "namespaces")? {
                    if value.is_null() {
                        return Err(IvyError::internal());
                    }
                    if value.get("namespace") == required.get("namespace") {
                        namespace = Some(value);
                        break;
                    }
                }
            }
            let compatible = if let Some(namespace) = namespace {
                let tools = array(namespace, "tools")?;
                let mut compatible = !tools.is_empty();
                if compatible {
                    for tool in tools {
                        if tool.is_null() {
                            return Err(IvyError::internal());
                        }
                        if tool.get("interfaceVersion") != required.get("interfaceVersion") {
                            compatible = false;
                            break;
                        }
                    }
                }
                compatible
            } else {
                false
            };
            result.push(ServiceState {
                has_catalog: namespace.is_some(),
                compatible,
                eligible: Registry::eligible(&node),
            });
        }
        Ok(result)
    }
    pub fn inspect(
        &self,
        store: &Store,
        parameters: &IvyJson,
        runtime: &dyn Runtime,
        clock: &dyn Fn() -> f64,
    ) -> IvyResult<IvyJson> {
        let ui = string(parameters, "uiId")?;
        let pointer = pointer(store, ui)?;
        let selected = parameters
            .get("releaseId")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| nullable(pointer.current.clone()));
        let mut report = Inspection {
            value: IvyJson::object([
                ("uiId", IvyJson::String(ui.clone())),
                ("currentReleaseId", nullable(pointer.current)),
                ("requestedReleaseId", selected.clone()),
                ("release", IvyJson::Null),
                ("requirements", IvyJson::Null),
                ("checkedAt", IvyJson::String(runtime.iso_date(clock())?)),
            ]),
            status: if Some(&selected)
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty())
            {
                5
            } else {
                0
            },
            issues: vec![],
            truncated: false,
        };
        if Some(&selected)
            .and_then(IvyJson::as_js_string)
            .is_none_or(|v| v.0.is_empty())
        {
            return Ok(report.finish());
        }
        let id = selected.as_js_string().ok_or_else(IvyError::internal)?;
        let Some(encoded) = retained(store, ui, id)? else {
            report.issue(
                1,
                "not_found",
                "The selected release is not retained.",
                empty(),
            );
            return Ok(report.finish());
        };
        let release: IvyResult<IvyJson> = (|| {
            let value = parse(&encoded, usize::MAX)?;
            require(
                is_ui_release(&value)?,
                "ui_release_invalid",
                "Release does not match the current ui contract.",
            )?;
            require(
                value.get("releaseId") == Some(&selected),
                "ui_release_invalid",
                "Release identity differs from its retained key.",
            )?;
            asset_path(string(&value, "entryPath")?)?;
            Ok(value)
        })();
        let release = match release {
            Ok(value) => value,
            Err(_) => {
                report.issue(
                    2,
                    "ui_release_invalid",
                    "The retained release manifest is invalid.",
                    empty(),
                );
                return Ok(report.finish());
            }
        };
        let assets = array(&release, "assets")?;
        let entry = string(&release, "entryPath")?;
        let requirements = field(&release, "requirements")?;
        report.value.insert(
            "release",
            IvyJson::object([
                ("releaseId", selected.clone()),
                ("entryPath", IvyJson::String(entry.clone())),
                ("assetCount", IvyJson::Number(assets.len() as f64)),
            ]),
        );
        report.value.insert("requirements", requirements.clone());
        let mut names = HashSet::new();
        for asset in assets {
            let path = string(asset, "path")?;
            let resource = || IvyJson::object([("assetPath", IvyJson::String(path.clone()))]);
            if asset_path(path).is_err() {
                report.issue(
                    2,
                    "ui_release_invalid",
                    "An asset path is invalid.",
                    resource(),
                );
            }
            if !names.insert(path.clone()) {
                report.issue(
                    2,
                    "ui_release_invalid",
                    "An asset path occurs more than once.",
                    resource(),
                );
            }
            if !mime_type(asset.get("mediaType")) || !self.files.available(ui, id, asset) {
                report.issue(
                    2,
                    "ui_release_invalid",
                    "The private UI file is missing or changed.",
                    resource(),
                );
            }
        }
        if !assets.iter().any(|a| {
            a.get("path").and_then(IvyJson::as_js_string) == Some(entry)
                && a.get("mediaType") == Some(&"text/html".into())
        }) {
            report.issue(
                2,
                "ui_release_invalid",
                "The release has no HTML entry asset.",
                IvyJson::object([("assetPath", IvyJson::String(entry.clone()))]),
            );
        }
        for required in array(requirements, "contracts")? {
            if let Err(error) =
                self.registry
                    .check_requirements(store, std::slice::from_ref(required), false)
            {
                if !["contract_not_found", "contract_version_conflict"]
                    .contains(&error.code.as_str())
                {
                    return Err(error);
                }
                report.issue(
                    3,
                    &error.code,
                    &error.message,
                    IvyJson::object([("contractKey", field(required, "key")?.clone())]),
                );
            }
        }
        for required in array(requirements, "services")? {
            let states = self.service_states(store, required, runtime, clock)?;
            let resource = IvyJson::object([
                ("serviceName", field(required, "serviceName")?.clone()),
                ("namespace", field(required, "namespace")?.clone()),
            ]);
            if states.is_empty() {
                report.issue(
                    4,
                    "service_unavailable",
                    "The required service has not registered.",
                    resource,
                );
            } else if !states.iter().any(|s| s.compatible) && states.iter().any(|s| s.has_catalog) {
                report.issue(
                    3,
                    "service_interface_changed",
                    "No provider exposes the required namespace interface version.",
                    resource,
                );
            } else if !states.iter().any(|s| s.compatible && s.eligible) {
                report.issue(
                    4,
                    "service_not_ready",
                    "No compatible provider currently has an eligible catalog.",
                    resource,
                );
            }
        }
        Ok(report.finish())
    }
    fn check_services(
        &self,
        store: &Store,
        requirements: &IvyJson,
        runtime: &dyn Runtime,
        clock: &dyn Fn() -> f64,
    ) -> IvyResult<()> {
        if requirements.is_null() {
            return Err(IvyError::internal());
        }
        require(
            requirements.get("hiveProtocol") == Some(&IvyJson::Number(1.0)),
            "unsupported_protocol",
            "UI requires an unsupported Hive protocol.",
        )?;
        for required in array(requirements, "services")? {
            let states = self.service_states(store, required, runtime, clock)?;
            require(
                !states.is_empty(),
                "service_unavailable",
                "Required ui service has not registered.",
            )?;
            require(
                states.iter().any(|s| s.compatible),
                "service_interface_changed",
                "Required ui service interface is incompatible.",
            )?;
            require(
                states.iter().any(|s| s.compatible && s.eligible),
                "service_not_ready",
                "Required ui service has no eligible compatible provider.",
            )?;
        }
        Ok(())
    }
    pub fn entry(
        &self,
        store: &Store,
        ui: &JsString,
        runtime: &dyn Runtime,
        clock: &dyn Fn() -> f64,
    ) -> IvyResult<IvyJson> {
        let id = pointer(store, ui)?
            .current
            .filter(|s| !s.is_empty())
            .ok_or_else(|| IvyError::new("not_found", "UI has no active release."))?;
        let encoded = retained(store, ui, &id.clone().into())?
            .ok_or_else(|| IvyError::new("not_found", "Selected UI release is not retained."))?;
        let release = parse(&encoded, usize::MAX).map_err(sql_error)?;
        self.check_services(store, field(&release, "requirements")?, runtime, clock)?;
        let mut entry = IvyJson::object([("releaseId", id.into())]);
        if let Some(path) = release.get("entryPath") {
            entry.insert("entryPath", path.clone());
        }
        Ok(entry)
    }
    pub fn stage_asset(
        &self,
        store: &mut Store,
        context: &AuthenticatedContext,
        parameters: &IvyJson,
        now_ms: u64,
    ) -> IvyResult<IvyJson> {
        store.mutate(
            context,
            &"uis.stageAsset".into(),
            parameters,
            now_ms,
            |_, _| {
                let base64 = string(parameters, "base64")?;
                let bytes = STANDARD
                    .decode(text(base64))
                    .ok()
                    .filter(|b| JsString::from(STANDARD.encode(b)) == *base64)
                    .ok_or_else(|| {
                        IvyError::new("invalid_arguments", "UI asset encoding is invalid.")
                    })?;
                self.files.stage(
                    string(parameters, "uiId")?,
                    string(parameters, "releaseId")?,
                    field(parameters, "asset")?,
                    &bytes,
                )?;
                Ok(LedgerResult::snapshot(IvyJson::object([(
                    "staged",
                    true.into(),
                )])))
            },
        )
    }
    pub fn deploy(
        &self,
        store: &mut Store,
        context: &AuthenticatedContext,
        parameters: &IvyJson,
        now_ms: u64,
    ) -> IvyResult<IvyJson> {
        store.mutate(context,&"uis.deploy".into(),parameters,now_ms,|store,_|{
            let metadata=field(parameters,"metadata")?;let release=field(parameters,"release")?;
            let ui=string(metadata,"uiId")?;let id=string(release,"releaseId")?;
            let existing=sql(store,store.connection().query_row("SELECT current_release_id FROM apps WHERE app_id=?",[text(ui)],|r|r.get::<_,Option<String>>(0)).optional())?;
            let previous=nullable(existing.clone().flatten());
            require(parameters.get("expectedReleaseId")==Some(&previous),"release_conflict","UI release pointer changed.")?;
            if metadata.get("slug").and_then(IvyJson::as_js_string).is_some_and(|v| !v.0.is_empty()) {
                let registered=sql(store,store.connection().query_row("SELECT app_id FROM apps WHERE app_id<>? AND CASE WHEN json_valid(metadata) THEN json_extract(metadata,'$.slug') END=?",params![text(ui),text(string(metadata,"slug")?)],|r|r.get::<_,String>(0)).optional())?;
                require(registered.is_none(),"invalid_arguments","UI path is already registered.")?;
            }
            self.registry.check_requirements(store,array(field(release,"requirements")?,"contracts")?,true)?;
            let assets=array(release,"assets")?;self.files.promote(ui,id,assets)?;
            let entry=asset_path(string(release,"entryPath")?)?;
            let names=assets.iter().map(|a|asset_path(string(a,"path")?).cloned()).collect::<IvyResult<Vec<_>>>()?;
            require(names.iter().collect::<HashSet<_>>().len()==names.len()&&names.contains(entry),"invalid_arguments","UI release needs unique asset paths and an existing entry.")?;
            for asset in assets {
                require(mime_type(asset.get("mediaType")),"invalid_arguments","UI asset media type is unsupported.")?;
                require(self.files.check(ui,id,asset)?,"invalid_arguments","UI release file does not match its manifest.")?;
                if asset.get("path").and_then(IvyJson::as_js_string)==Some(entry){require(asset.get("mediaType")==Some(&"text/html".into()),"invalid_arguments","UI entry must be HTML.")?;}
            }
            let retained=retained(store,ui,id)?;
            if let Some(encoded)=retained.as_ref(){require(*encoded==canonical(release,MANAGEMENT_FRAME_BYTES)?,"release_conflict","Release identity already has different immutable assets.")?;}
            if existing.is_none(){sql(store,store.connection().execute("INSERT INTO apps VALUES (?,?,NULL,NULL)",params![text(ui),canonical(metadata,MANAGEMENT_FRAME_BYTES)?]))?;}
            if retained.is_none(){sql(store,store.connection().execute("INSERT INTO app_releases VALUES (?,?,?)",params![text(ui),text(id),canonical(release,MANAGEMENT_FRAME_BYTES)?]))?;}
            sql(store,store.connection().execute("UPDATE apps SET metadata=?,previous_release_id=NULL,current_release_id=? WHERE app_id=?",params![canonical(metadata,MANAGEMENT_FRAME_BYTES)?,text(id),text(ui)]))?;
            self.prune(store,ui)?;store.invalidate("uis".into());
            Ok(LedgerResult::snapshot(IvyJson::object([("uiId",IvyJson::String(ui.clone())),("releaseId",IvyJson::String(id.clone())),("previousReleaseId",IvyJson::Null)])))
        })
    }
    pub fn rollback(
        &self,
        store: &mut Store,
        context: &AuthenticatedContext,
        parameters: &IvyJson,
        now_ms: u64,
        runtime: &dyn Runtime,
        clock: &dyn Fn() -> f64,
    ) -> IvyResult<IvyJson> {
        store.mutate(context,&"uis.rollback".into(),parameters,now_ms,|store,_|{
            let ui=string(parameters,"uiId")?;let id=string(parameters,"releaseId")?;let current=self.get(store,ui)?;
            require(current.get("currentReleaseId")==parameters.get("expectedReleaseId"),"release_conflict","UI release pointer changed.")?;
            require(array(&current,"releases")?.iter().any(|r|r.get("releaseId")==parameters.get("releaseId")),"not_found","Retained ui release not found.")?;
            let inspection=self.inspect(store,&IvyJson::object([("uiId",IvyJson::String(ui.clone())),("releaseId",IvyJson::String(id.clone()))]),runtime,clock)?;
            if inspection.get("status")!=Some(&"ready".into()) {
                let issue=array(&inspection,"issues")?.first();
                return Err(IvyError::new_js(issue.and_then(|i|i.get("code")).and_then(IvyJson::as_js_string).cloned().unwrap_or_else(||"ui_release_invalid".into()),issue.and_then(|i|i.get("message")).and_then(IvyJson::as_js_string).cloned().unwrap_or_else(||"Retained UI release is unavailable.".into())));
            }
            sql(store,store.connection().execute("UPDATE apps SET previous_release_id=NULL,current_release_id=? WHERE app_id=?",params![text(id),text(ui)]))?;
            self.prune(store,ui)?;store.invalidate("uis".into());
            Ok(LedgerResult::snapshot(IvyJson::object([("uiId",IvyJson::String(ui.clone())),("releaseId",IvyJson::String(id.clone())),("previousReleaseId",IvyJson::Null)])))
        })
    }
    pub fn asset(
        &self,
        store: &Store,
        ui: &JsString,
        id: &JsString,
        path: &JsString,
    ) -> IvyResult<IvyJson> {
        asset_path(path)?;
        let encoded = retained(store, ui, id)?
            .ok_or_else(|| IvyError::new("not_found", "Immutable ui release not found."))?;
        let release = parse(&encoded, usize::MAX).map_err(sql_error)?;
        for asset in array(&release, "assets")? {
            if asset.is_null() {
                return Err(IvyError::internal());
            }
            if asset.get("path").and_then(IvyJson::as_js_string) == Some(path) {
                return Ok(IvyJson::object([
                    ("releaseId", IvyJson::String(id.clone())),
                    ("asset", asset.clone()),
                ]));
            }
        }
        Err(IvyError::new("not_found", "Immutable ui asset not found."))
    }
    pub fn current_asset(
        &self,
        store: &Store,
        ui: &JsString,
        path: Option<&JsString>,
        runtime: &dyn Runtime,
        clock: &dyn Fn() -> f64,
    ) -> IvyResult<IvyJson> {
        if let Some(path) = path.filter(|p| !p.0.is_empty()) {
            let id = pointer(store, ui)?
                .current
                .filter(|s| !s.is_empty())
                .ok_or_else(|| IvyError::new("not_found", "UI has no active release."))?;
            self.asset(store, ui, &id.into(), path)
        } else {
            let entry = self.entry(store, ui, runtime, clock)?;
            self.asset(
                store,
                ui,
                string(&entry, "releaseId")?,
                string(&entry, "entryPath")?,
            )
        }
    }
    fn prune(&self, store: &Store, ui: &JsString) -> IvyResult<()> {
        let current = sql(
            store,
            store.connection().query_row(
                "SELECT current_release_id FROM apps WHERE app_id=?",
                [text(ui)],
                |r| r.get::<_, Option<String>>(0),
            ),
        )?;
        sql(store,store.connection().execute("DELETE FROM app_releases WHERE app_id=? AND release_id IS NOT ?",params![text(ui),current]))?;
        Ok(())
    }
    pub fn collect_files(&self, store: &Store, clock: &dyn Fn() -> f64) -> IvyResult<()> {
        let mut query = sql(
            store,
            store.connection().prepare_cached("SELECT app_id FROM apps"),
        )?;
        let ids = sql(store, query.query_map([], |r| r.get::<_, String>(0)))?;
        let mut retained = HashMap::new();
        for id in ids {
            let id = sql(store, id)?;
            let mut query = sql(
                store,
                store
                    .connection()
                    .prepare_cached("SELECT release_id FROM app_releases WHERE app_id=?"),
            )?;
            let values = sql(store, query.query_map([&id], |r| r.get::<_, String>(0)))?
                .map(|r| sql(store, r).map(JsString::from))
                .collect::<IvyResult<HashSet<_>>>()?;
            retained.insert(id.into(), values);
        }
        self.files.collect_all_for_store(store, &retained, clock)
    }
}

pub fn asset_path(path: &JsString) -> IvyResult<&JsString> {
    let valid = !path.0.is_empty()
        && path.0.len() <= 1024
        && char::decode_utf16(path.0.iter().copied()).all(|c| c.is_ok())
        && *path == path.normalize_nfc()
        && !path.0.iter().any(|c| {
            *c <= 31
                || *c == 127
                || [b'\\', b'%', b'?', b'#', b':'].contains(&(*c as u8)) && *c < 128
        })
        && path.0.split(|c| *c == b'/' as u16).all(|part| {
            !part.is_empty() && part != [b'.' as u16] && part != [b'.' as u16, b'.' as u16]
        });
    if valid {
        Ok(path)
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Asset path must be a normalized relative path.",
        ))
    }
}
