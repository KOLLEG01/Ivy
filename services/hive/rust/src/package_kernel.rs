//! Package catalog composition with the kernel's live Registry and options.
use crate::{
    package_catalog_object::PackageCatalogObject, registry::Registry, retention::Runtime,
    store::Store,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{cell::RefCell, collections::HashSet, rc::Rc};

pub type KernelCatalog = PackageCatalogObject<Box<dyn Fn(&Store) -> IvyResult<HashSet<JsString>>>>;

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn array<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a [IvyJson]> {
    match field(value, key)? {
        IvyJson::Array(values) => Ok(values),
        IvyJson::String(_) => Ok(&[]),
        _ => Err(IvyError::internal()),
    }
}
fn optional_build(builds: &mut HashSet<JsString>, value: Option<&IvyJson>) {
    if let Some(id) = value
        .and_then(IvyJson::as_js_string)
        .filter(|id| !id.0.is_empty())
    {
        builds.insert(id.clone());
    }
}
/// Retained host observations protect offline installations and unfinished work.
/// Registry readiness/staleness does not remove a build from this protection set.
pub fn protected_builds(
    store: &Store,
    registry: &Registry,
    build_id: &JsString,
    runtime: &dyn Runtime,
    clock: &dyn Fn() -> f64,
) -> IvyResult<HashSet<JsString>> {
    let mut builds = HashSet::from([build_id.clone()]);
    for node in registry.nodes(store, runtime, clock())? {
        if let Some(id) = node.get("buildId").and_then(IvyJson::as_js_string) {
            builds.insert(id.clone());
        }
    }
    for observation in registry.host_observations(store, None, runtime, clock())? {
        let status = field(field(&observation, "snapshot")?, "status")?;
        optional_build(
            &mut builds,
            status.get("executor").and_then(|v| v.get("buildId")),
        );
        for instance in array(status, "instances")? {
            if instance.is_null() {
                return Err(IvyError::internal());
            }
            optional_build(&mut builds, instance.get("installedBuild"));
            optional_build(&mut builds, instance.get("observedBuild"));
        }
        for operation in array(status, "unfinished")? {
            if operation.is_null() {
                return Err(IvyError::internal());
            }
            for key in ["previousBuild", "targetBuild", "observedBuild"] {
                optional_build(&mut builds, operation.get(key));
            }
        }
    }
    Ok(builds)
}
/// Share the existing single-owner Registry, rather than constructing a second
/// Registry or retaining a stale snapshot when catalog publication is initialized.
pub fn catalog(
    store: &mut Store,
    registry: Rc<RefCell<Registry>>,
    build_id: JsString,
    runtime: Rc<dyn Runtime>,
    clock: Rc<dyn Fn() -> f64>,
) -> IvyResult<KernelCatalog> {
    PackageCatalogObject::new(
        store,
        Box::new(move |store: &Store| {
            protected_builds(
                store,
                &registry.borrow(),
                &build_id,
                runtime.as_ref(),
                clock.as_ref(),
            )
        }) as Box<dyn Fn(&Store) -> IvyResult<HashSet<JsString>>>,
    )
}

/// Typed internal requests retain the existing package.* worker wire shapes.
pub enum CatalogRequest<'a> {
    Catalog {
        after: &'a IvyJson,
        include_uis: bool,
    },
    Seed {
        catalog: &'a IvyJson,
    },
    Prune,
    Publish {
        input: &'a IvyJson,
        publisher_principal_id: &'a JsString,
    },
}
/// The caller supplies the current configured credential order for every request.
/// Only seed/prune require its first principal; an empty first entry is not skipped.
pub fn dispatch(
    catalog: &KernelCatalog,
    store: &mut Store,
    request: CatalogRequest<'_>,
    configured_principals: &[JsString],
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    let principal = || {
        configured_principals
            .first()
            .filter(|id| !id.0.is_empty())
            .ok_or_else(|| {
                IvyError::new(
                    "service_unavailable",
                    "Hive package catalog requires one configured principal.",
                )
            })
    };
    match request {
        CatalogRequest::Catalog { after, include_uis } => catalog
            .list(store, after, include_uis)
            .map(|v| v.into_json()),
        CatalogRequest::Seed { catalog: value } => catalog
            .seed(store, value, principal()?, now_ms, now)
            .map(|v| v.into_json()),
        CatalogRequest::Prune => catalog
            .prune(store, principal()?, now_ms, now)
            .map(IvyJson::Array),
        CatalogRequest::Publish {
            input,
            publisher_principal_id,
        } => catalog.publish(store, input, publisher_principal_id, now_ms, now),
    }
}
