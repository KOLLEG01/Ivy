//! Authoritative package catalog on the versioned Object store.
use crate::{
    objects::{self, Location},
    store::{AuthenticatedContext, Store},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, compare_versions, parse},
    limits::MANAGEMENT_FRAME_BYTES,
    operation::operation_id,
};
use ivy_host_runtime::package_archive::{validate_package_authorization, validate_package_catalog};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

const CONTRACT_KEY: &str = "ivy/package-catalog";
const CONTRACT_VERSION: &str = "1.0.2";
const CONTRACT: &str = r#"{
  "key":"ivy/package-catalog","version":"1.0.2","owner":{"kind":"hive"},"mediaType":"application/json",
  "retention":{"objects":{"mode":"retain"},"revisions":{"mode":"current"}},
  "specMarkdown":"Validated Ivy package catalog. Retain the latest two archives per component and builds referenced by installation observations or unfinished deployments.",
  "jsonSchema":{"type":"object","properties":{"schemaVersion":{"const":1},"revision":{"type":"integer","minimum":0},"packages":{"type":"array"}},"required":["schemaVersion","revision","packages"],"additionalProperties":false}
}"#;

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
fn entries(value: &IvyJson) -> IvyResult<&[IvyJson]> {
    field(value, "packages")?
        .as_array()
        .ok_or_else(IvyError::internal)
}
fn compare(a: &IvyJson, b: &IvyJson) -> IvyResult<Ordering> {
    compare_versions(
        &field(a, "version")?.as_str()?,
        &field(b, "version")?.as_str()?,
    )
}

pub struct PackageCatalogSnapshot {
    pub object_id: JsString,
    pub object_revision: u64,
    pub catalog: IvyJson,
}
impl PackageCatalogSnapshot {
    pub fn into_json(self) -> IvyJson {
        IvyJson::object([
            ("objectId", IvyJson::String(self.object_id)),
            (
                "objectRevision",
                IvyJson::Number(self.object_revision as f64),
            ),
            ("catalog", self.catalog),
        ])
    }
}

/// The kernel supplies its current build and real Registry/host observations.
/// A fallible callback preserves failure before the catalog write transaction.
pub struct PackageCatalogObject<F> {
    protected_builds: F,
}
impl<F: Fn(&Store) -> IvyResult<HashSet<JsString>>> PackageCatalogObject<F> {
    pub fn new(store: &mut Store, protected_builds: F) -> IvyResult<Self> {
        let contract = parse(CONTRACT, MANAGEMENT_FRAME_BYTES)?;
        objects::register(store, &contract, field(&contract, "owner")?)?;
        Ok(Self { protected_builds })
    }

    pub fn current(&self, store: &Store) -> IvyResult<Option<PackageCatalogSnapshot>> {
        let mut statement = store
            .connection()
            .prepare_cached("SELECT id FROM objects WHERE contract_key=? ORDER BY id LIMIT 2")
            .map_err(|e| store.query_error(e))?;
        let ids = statement
            .query_map([CONTRACT_KEY], |row| row.get::<_, String>(0))
            .map_err(|e| store.query_error(e))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| store.query_error(e))?;
        require(
            ids.len() <= 1,
            "storage_invalid",
            "Hive retained more than one package catalog Object.",
        )?;
        let Some(id) = ids.first().filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let id: JsString = id.as_str().into();
        let read = objects::read(store, Location::Id(&id), None)?;
        let content = field(&read, "content")?;
        require(
            content.get("encoding") == Some(&"json".into()),
            "storage_invalid",
            "Package catalog Object is not JSON.",
        )?;
        Ok(Some(PackageCatalogSnapshot {
            object_id: id,
            object_revision: field(field(&read, "revision")?, "revision")?
                .as_f64()
                .ok_or_else(IvyError::internal)? as u64,
            catalog: validate_package_catalog(field(content, "value")?)?,
        }))
    }

    pub fn seed(
        &self,
        store: &mut Store,
        value: &IvyJson,
        principal: &JsString,
        now_ms: u64,
        now: &JsString,
    ) -> IvyResult<PackageCatalogSnapshot> {
        if let Some(current) = self.current(store)? {
            return Ok(current);
        }
        let catalog = validate_package_catalog(value)?;
        let mut request = IvyJson::object([(
            "create",
            IvyJson::object([
                ("contractKey", CONTRACT_KEY.into()),
                ("parentId", IvyJson::Null),
                ("ownerObjectId", IvyJson::Null),
                ("name", "Ivy package catalog".into()),
            ]),
        )]);
        self.write(
            store,
            principal,
            "internal:package-catalog",
            "packages.catalog.seed",
            &mut request,
            &catalog,
            now_ms,
            now,
        )?;
        self.current(store)?.ok_or_else(IvyError::internal)
    }

    pub fn list(
        &self,
        store: &Store,
        after: &IvyJson,
        include_uis: bool,
    ) -> IvyResult<PackageCatalogSnapshot> {
        let after = after.as_f64();
        require(
            after.is_some_and(|n| {
                n.is_finite() && n.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&n)
            }),
            "invalid_arguments",
            "Package catalog revision is invalid.",
        )?;
        let after = after.unwrap();
        let mut current = self.current(store)?.ok_or_else(|| {
            IvyError::new(
                "service_unavailable",
                "Package catalog Object is not initialized.",
            )
        })?;
        let packages = entries(&current.catalog)?
            .iter()
            .filter(|entry| {
                entry
                    .get("revision")
                    .and_then(IvyJson::as_f64)
                    .is_some_and(|revision| revision > after)
                    && (include_uis
                        || entry
                            .get("manifest")
                            .and_then(|manifest| manifest.get("kind"))
                            != Some(&"app".into()))
            })
            .cloned()
            .collect();
        current.catalog = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("revision", field(&current.catalog, "revision")?.clone()),
            ("packages", IvyJson::Array(packages)),
        ]);
        Ok(current)
    }

    fn retained_indices(&self, store: &Store, packages: &[IvyJson]) -> IvyResult<Vec<bool>> {
        let mut groups: Vec<Vec<usize>> = vec![];
        let mut positions = HashMap::new();
        for (index, entry) in packages.iter().enumerate() {
            let component = string(entry, "componentId")?;
            let position = *positions.entry(component).or_insert_with(|| {
                groups.push(vec![]);
                groups.len() - 1
            });
            groups[position].push(index);
        }
        let mut keep = vec![false; packages.len()];
        for mut indices in groups {
            let mut failure = None;
            // Stable ties preserve entry identity, even with duplicate versions.
            indices.sort_by(|a, b| match compare(&packages[*b], &packages[*a]) {
                Ok(order) => order,
                Err(error) => {
                    failure = Some(error);
                    Ordering::Equal
                }
            });
            if let Some(error) = failure {
                return Err(error);
            }
            for index in indices.into_iter().take(2) {
                keep[index] = true;
            }
        }
        let protected = (self.protected_builds)(store)?;
        for (index, entry) in packages.iter().enumerate() {
            keep[index] |= protected.contains(string(entry, "buildId")?);
        }
        Ok(keep)
    }

    pub fn prune(
        &self,
        store: &mut Store,
        principal: &JsString,
        now_ms: u64,
        now: &JsString,
    ) -> IvyResult<Vec<IvyJson>> {
        let current = self.current(store)?.ok_or_else(|| {
            IvyError::new(
                "service_unavailable",
                "Package catalog Object is not initialized.",
            )
        })?;
        let packages = entries(&current.catalog)?;
        let keep = self.retained_indices(store, packages)?;
        if keep.iter().all(|keep| *keep) {
            return Ok(vec![]);
        }
        let retained = packages
            .iter()
            .zip(&keep)
            .filter(|(_, keep)| **keep)
            .map(|(entry, _)| entry.clone())
            .collect();
        let removed = packages
            .iter()
            .zip(&keep)
            .filter(|(_, keep)| !**keep)
            .map(|(entry, _)| entry.clone())
            .collect();
        let next = validate_package_catalog(&IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("revision", field(&current.catalog, "revision")?.clone()),
            ("packages", IvyJson::Array(retained)),
        ]))?;
        let mut request = current.write_request();
        self.write(
            store,
            principal,
            "internal:package-prune",
            "packages.catalog.prune",
            &mut request,
            &next,
            now_ms,
            now,
        )?;
        Ok(removed)
    }

    pub fn publish(
        &self,
        store: &mut Store,
        value: &IvyJson,
        principal: &JsString,
        now_ms: u64,
        now: &JsString,
    ) -> IvyResult<IvyJson> {
        let mut entry = validate_package_authorization(value)?;
        let current = self.current(store)?.ok_or_else(|| {
            IvyError::new(
                "service_unavailable",
                "Package catalog Object is not initialized.",
            )
        })?;
        let mut latest: Option<&IvyJson> = None;
        for previous in entries(&current.catalog)? {
            if previous.get("componentId") == entry.get("componentId")
                && (latest.is_none() || compare(previous, latest.unwrap())? == Ordering::Greater)
            {
                latest = Some(previous);
            }
        }
        if let Some(latest) = latest {
            if compare(&entry, latest)? != Ordering::Greater {
                require(
                    entry.get("version") == latest.get("version")
                        && entry.get("archiveHash") == latest.get("archiveHash")
                        && entry.get("buildId") == latest.get("buildId")
                        && canonical(field(&entry, "manifest")?, MANAGEMENT_FRAME_BYTES)?
                            == canonical(field(latest, "manifest")?, MANAGEMENT_FRAME_BYTES)?,
                    "version_conflict",
                    "Published package versions are immutable and must increase.",
                )?;
                return Ok(latest.clone());
            }
        }
        entry.insert(
            "revision",
            IvyJson::Number(
                field(&current.catalog, "revision")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)?
                    + 1.0,
            ),
        );
        entry.insert("publishedAt", IvyJson::String(now.clone()));
        entry.insert("publisherPrincipalId", IvyJson::String(principal.clone()));
        let mut packages = entries(&current.catalog)?.to_vec();
        packages.push(entry.clone());
        let keep = self.retained_indices(store, &packages)?;
        let next = validate_package_catalog(&IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("revision", field(&entry, "revision")?.clone()),
            (
                "packages",
                IvyJson::Array(
                    packages
                        .into_iter()
                        .zip(keep)
                        .filter(|(_, keep)| *keep)
                        .map(|(entry, _)| entry)
                        .collect(),
                ),
            ),
        ]))?;
        let mut request = current.write_request();
        self.write(
            store,
            principal,
            "internal:package-upload",
            "packages.catalog.publish",
            &mut request,
            &next,
            now_ms,
            now,
        )?;
        Ok(entry)
    }

    fn write(
        &self,
        store: &mut Store,
        principal: &JsString,
        credential: &str,
        method: &str,
        request: &mut IvyJson,
        catalog: &IvyJson,
        now_ms: u64,
        now: &JsString,
    ) -> IvyResult<()> {
        let epoch = store
            .metadata("runtime_epoch")?
            .ok_or_else(IvyError::internal)?;
        request.insert(
            "mutationId",
            operation_id(&epoch, now_ms, &uuid::Uuid::new_v4().to_string())?.into(),
        );
        request.insert("contractVersion", CONTRACT_VERSION.into());
        request.insert(
            "references",
            IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
        );
        request.insert(
            "content",
            IvyJson::object([
                ("encoding", "json".into()),
                (
                    "value",
                    parse(
                        &canonical(catalog, MANAGEMENT_FRAME_BYTES)?,
                        MANAGEMENT_FRAME_BYTES,
                    )?,
                ),
            ]),
        );
        objects::write_hive(
            store,
            &AuthenticatedContext {
                principal_id: principal.clone(),
                credential_digest: credential.into(),
            },
            &method.into(),
            request,
            now_ms,
            now,
        )?;
        Ok(())
    }
}
impl PackageCatalogSnapshot {
    fn write_request(&self) -> IvyJson {
        IvyJson::object([
            ("objectId", IvyJson::String(self.object_id.clone())),
            (
                "expectedRevision",
                IvyJson::Number(self.object_revision as f64),
            ),
        ])
    }
}
