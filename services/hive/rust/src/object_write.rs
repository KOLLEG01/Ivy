//! Immutable Object creation, content updates and renaming.
use super::{
    ContentData, Location, complete_object, complete_revision, decode, raw_object, read,
    revision_row, stat,
};
use crate::store::{AuthenticatedContext, LedgerResult, Principal, Store};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, compare_versions, object_entries},
};
use rusqlite::{OptionalExtension, params, types::Value as SqlValue};
use std::cmp::Ordering;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Select an existing Object with an unchanged name for callers with that scope.
pub struct ExistingObjectWrite<'a> {
    parameters: &'a IvyJson,
}
impl<'a> ExistingObjectWrite<'a> {
    pub fn from_parameters(parameters: &'a IvyJson) -> Option<Self> {
        (parameters.as_object().is_some()
            && parameters.get("create").is_none()
            && parameters.get("name").is_none())
        .then_some(Self { parameters })
    }
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string<'a>(value: &'a IvyJson, field: &str) -> IvyResult<&'a JsString> {
    value
        .get(field)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn reference_name(name: &JsString) -> bool {
    (1..=128).contains(&name.0.len())
        && matches!(name.0[0], 65..=90 | 97..=122)
        && name
            .0
            .iter()
            .all(|unit| matches!(unit, 65..=90 | 97..=122 | 48..=57 | 95 | 46 | 45))
}
fn references(store: &Store, source: &JsString, value: &IvyJson) -> IvyResult<String> {
    // Object.entries(null) fails before the original prototype/count guard.
    if value.is_null() {
        return Err(IvyError::internal());
    }
    let entries = object_entries(value);
    require(
        value.as_object().is_some() && entries.len() <= 256,
        "limit_exceeded",
        "A revision supports at most 256 exact references.",
    )?;
    for (name, target) in entries {
        let id = target.get("objectId").and_then(IvyJson::as_js_string);
        let revision = target.get("revision").and_then(IvyJson::as_f64);
        require(
            reference_name(name)
                && target.as_object().is_some_and(|pairs| pairs.len() == 2)
                && id.is_some_and(|id| (1..=256).contains(&id.0.len()))
                && revision.is_some_and(|r| r > 0.0 && r <= MAX_SAFE_INTEGER && r.fract() == 0.0),
            "invalid_arguments",
            "Invalid exact revision reference.",
        )?;
        let id = id.unwrap();
        require(
            id != source,
            "invalid_arguments",
            "A revision cannot reference its own Object.",
        )?;
        let committed = store
            .connection()
            .query_row(
                "SELECT 1 FROM revisions WHERE object_id=? AND revision=?",
                params![String::from_utf16_lossy(&id.0), revision.unwrap() as i64],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| store.query_error(e))?
            .is_some();
        require(
            committed,
            "not_found",
            "Referenced Object revision is not committed.",
        )?;
    }
    canonical(value, 128 * 1024)
}
fn update_search(store: &Store, id: &str, name: &str, text: &str) -> IvyResult<()> {
    let search_id: i64 = store
        .connection()
        .query_row("SELECT search_id FROM objects WHERE id=?", [id], |row| {
            row.get(0)
        })
        .map_err(|e| store.query_error(e))?;
    let previous = store
        .connection()
        .query_row(
            "SELECT name,text FROM object_fts WHERE rowid=?",
            [search_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| store.query_error(e))?;
    if previous
        .as_ref()
        .is_some_and(|(old_name, old_text)| old_name == name && old_text == text)
    {
        return Ok(());
    }
    store
        .connection()
        .execute("DELETE FROM object_fts WHERE rowid=?", [search_id])
        .map_err(|e| store.query_error(e))?;
    store
        .connection()
        .execute(
            "INSERT INTO object_fts(rowid,object_id,name,text) VALUES (?,?,?,?)",
            params![search_id, id, name, text],
        )
        .map_err(|e| store.query_error(e))?;
    Ok(())
}

/// Run inside Store::mutate's work callback. Receipt replay must precede all
/// mutable reads here, including retained references and prepared Hive inputs.
pub fn write_existing_revision(
    store: &mut Store,
    principal: &Principal,
    request: ExistingObjectWrite<'_>,
    allow_hive: bool,
    preserve_references: bool,
    now: &JsString,
) -> IvyResult<LedgerResult> {
    write_revision(
        store,
        principal,
        request.parameters,
        allow_hive,
        preserve_references,
        now,
    )
}

/// The request's clocks are supplied by admitted dispatch, never its parameters.
pub fn write(
    store: &mut Store,
    context: &AuthenticatedContext,
    parameters: &IvyJson,
    preserve_references: bool,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    let method = if preserve_references {
        "objects.updateContent"
    } else {
        "objects.write"
    };
    write_as(
        store,
        context,
        &method.into(),
        parameters,
        false,
        preserve_references,
        now_ms,
        now,
    )
}
pub fn write_hive(
    store: &mut Store,
    context: &AuthenticatedContext,
    operation: &JsString,
    parameters: &IvyJson,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    write_as(
        store, context, operation, parameters, true, false, now_ms, now,
    )
}
fn write_as(
    store: &mut Store,
    context: &AuthenticatedContext,
    operation: &JsString,
    parameters: &IvyJson,
    allow_hive: bool,
    preserve_references: bool,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    store.mutate(
        context,
        operation,
        parameters,
        now_ms,
        |store, principal| {
            write_revision(
                store,
                principal,
                parameters,
                allow_hive,
                preserve_references,
                now,
            )
        },
    )
}
pub fn write_hive_prepared(
    store: &mut Store,
    context: &AuthenticatedContext,
    operation: &JsString,
    request: &IvyJson,
    now_ms: u64,
    now: &JsString,
    prepare: impl FnOnce(&mut Store) -> IvyResult<IvyJson>,
) -> IvyResult<IvyJson> {
    store.mutate(context, operation, request, now_ms, |store, principal| {
        let parameters = prepare(store)?;
        write_revision(store, principal, &parameters, true, false, now)
    })
}
fn optional_id(value: &IvyJson) -> IvyResult<Option<&JsString>> {
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_js_string()
            .map(Some)
            .ok_or_else(IvyError::internal)
    }
}
fn write_revision(
    store: &mut Store,
    principal: &Principal,
    parameters: &IvyJson,
    allow_hive: bool,
    preserve_references: bool,
    now: &JsString,
) -> IvyResult<LedgerResult> {
    let create = parameters.get("create").filter(|value| match value {
        IvyJson::Null => false,
        IvyJson::Bool(value) => *value,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    });
    let existing = if create.is_some() {
        None
    } else {
        Some(raw_object(
            store,
            Location::Id(string(parameters, "objectId")?),
        )?)
    };
    if let Some(existing) = &existing {
        require(
            parameters.get("expectedRevision") == existing.get("currentRevision"),
            "revision_conflict",
            "Object content changed; read the current revision.",
        )?;
        require(
            parameters
                .get("expectedArchived")
                .is_none_or(|expected| existing.get("effectivelyArchived") == Some(expected)),
            "revision_conflict",
            "Object archive state changed; read the current state.",
        )?;
    }
    let retained;
    let revision_references = if preserve_references && existing.is_some() {
        let existing = existing.as_ref().unwrap();
        retained = read(
            store,
            Location::Id(string(existing, "id")?),
            Some(
                existing
                    .get("currentRevision")
                    .and_then(IvyJson::as_f64)
                    .unwrap() as u64,
            ),
        )?;
        retained
            .get("revision")
            .and_then(|revision| revision.get("references"))
    } else {
        parameters.get("references")
    };
    let key = string(
        create
            .or(existing.as_ref())
            .ok_or_else(IvyError::internal)?,
        "contractKey",
    )?;
    let version = string(parameters, "contractVersion")?;
    let contract = store.contract(key, version)?;
    let hive_owned =
        contract.get("owner").and_then(|owner| owner.get("kind")) == Some(&"hive".into());
    require(
        allow_hive || !hive_owned,
        "forbidden",
        "Hive-owned Objects can only be changed through their dedicated operation.",
    )?;
    require(
        !allow_hive || hive_owned,
        "contract_owner_mismatch",
        "Internal Hive writes require a Hive-owned contract.",
    )?;
    if let Some(existing) = &existing {
        let metadata = complete_object(store, existing.clone())?;
        require(
            compare_versions(
                &String::from_utf16_lossy(&version.0),
                &string(&metadata, "contractVersion")?.to_utf8()?,
            )? != Ordering::Less,
            "contract_version_conflict",
            "Object contract version cannot be decreased.",
        )?;
    }
    let content = parameters.get("content").ok_or_else(IvyError::internal)?;
    let decoded = decode(store, &contract, content)?;
    let id = existing
        .as_ref()
        .map(|existing| string(existing, "id").cloned())
        .transpose()?
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());
    let revision = existing.as_ref().map_or(1.0, |existing| {
        existing
            .get("currentRevision")
            .and_then(IvyJson::as_f64)
            .unwrap()
            + 1.0
    });
    require(
        revision.is_finite() && revision.fract() == 0.0 && revision <= MAX_SAFE_INTEGER,
        "limit_exceeded",
        "Revision limit reached.",
    )?;
    let revision = revision as i64;
    let id_text = id.to_utf8()?;
    let key_text = String::from_utf16_lossy(&key.0);
    let now_text = String::from_utf16_lossy(&now.0);
    let name;
    if let Some(create) = create {
        name = super::navigation::name(string(create, "name")?)?;
        let parent_id = optional_id(create.get("parentId").ok_or_else(IvyError::internal)?)?;
        let parent = super::navigation::parent(store, parent_id)?;
        if let Some(parent) = &parent {
            require(
                super::navigation::object_mode(store, &parent.contract_key)?.as_deref()
                    == Some("retain"),
                "invalid_arguments",
                "Owned artifacts cannot be navigation parents.",
            )?;
        }
        let (path, depth) = super::navigation::path(parent.as_ref(), &name)?;
        super::navigation::ensure_name(store, parent_id, &name, None)?;
        let owner = create.get("ownerObjectId").ok_or_else(IvyError::internal)?;
        let owned = contract
            .get("retention")
            .and_then(|r| r.get("objects"))
            .and_then(|r| r.get("mode"))
            == Some(&"owned".into());
        if !owned {
            require(
                owner.is_null(),
                "invalid_arguments",
                "Domain Objects cannot have a lifecycle owner.",
            )?;
        } else {
            require(
                !owner.is_null() && owner.as_js_string() != Some(&id),
                "invalid_arguments",
                "Owned Objects require an existing lifecycle owner.",
            )?;
            let owner = raw_object(
                store,
                Location::Id(owner.as_js_string().ok_or_else(IvyError::internal)?),
            )?;
            require(
                super::navigation::object_mode(store, string(&owner, "contractKey")?)?.as_deref()
                    == Some("retain"),
                "invalid_arguments",
                "The lifecycle owner must be a retained Object.",
            )?;
        }
        let position = super::navigation::next_position(store, parent_id)?;
        let icon = super::navigation::icon(
            create
                .get("icon")
                .filter(|value| !value.is_null())
                .map(|value| value.as_js_string().ok_or_else(IvyError::internal))
                .transpose()?,
        )?;
        store.connection().execute(
            "INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,1,NULL,?,?,?)",
            params![id_text, parent_id.map(|id| String::from_utf16_lossy(&id.0)), optional_id(owner)?.map(|id| String::from_utf16_lossy(&id.0)), name, path, position, icon, depth, key_text, parent.as_ref().map_or(0, |parent| parent.archived), now_text, now_text],
        ).map_err(|e| store.query_error(e))?;
    } else {
        let existing = existing.as_ref().unwrap();
        name = match parameters.get("name") {
            Some(value) => {
                super::navigation::name(value.as_js_string().ok_or_else(IvyError::internal)?)?
            }
            None => string(existing, "name")?.to_utf8()?,
        };
        let renamed = &JsString::from(name.as_str()) != string(existing, "name")?;
        if renamed {
            super::navigation::ensure_name(
                store,
                optional_id(existing.get("parentId").unwrap())?,
                &name,
                Some(&id),
            )?;
        }
        store
            .connection()
            .execute(
                "UPDATE objects SET name=?,current_revision=?,updated_at=? WHERE id=?",
                params![name, revision, now_text, id_text],
            )
            .map_err(|e| store.query_error(e))?;
        if renamed {
            super::mutations::refresh_navigation(store, &id)?;
            store.invalidate("objects".into());
        }
    }
    let revision_references = revision_references.ok_or_else(IvyError::internal)?;
    let encoded_references = references(store, &id, revision_references)?;
    let data = match decoded.data {
        ContentData::Text(value) => SqlValue::Text(value),
        ContentData::Bytes(value) => SqlValue::Blob(value),
    };
    let encoding = string(content, "encoding")?.to_utf8()?;
    store
        .connection()
        .execute(
            "INSERT INTO revisions VALUES (?,?,?,?,?,?,?,?,?,?)",
            params![
                id_text,
                revision,
                key_text,
                String::from_utf16_lossy(&version.0),
                encoding,
                data,
                decoded.content_hash,
                decoded.byte_length as i64,
                now_text,
                encoded_references
            ],
        )
        .map_err(|e| store.query_error(e))?;
    store.adjust_retention_usage(
        key,
        i64::from(create.is_some()),
        1,
        decoded.byte_length as i64,
    )?;
    for (name, target) in object_entries(revision_references) {
        store
            .connection()
            .execute(
                "INSERT INTO revision_references VALUES (?,?,?,?,?)",
                params![
                    id_text,
                    revision,
                    name.to_utf8()?,
                    String::from_utf16_lossy(&string(target, "objectId")?.0),
                    target.get("revision").and_then(IvyJson::as_f64).unwrap() as i64
                ],
            )
            .map_err(|e| store.query_error(e))?;
    }
    let text = match &data {
        SqlValue::Text(value) if encoding == "text" => value.as_str(),
        _ => "",
    };
    update_search(store, &id_text, &name, text)?;
    store.append_core_event(
        principal,
        &id,
        revision as u64,
        &"write".into(),
        string(parameters, "mutationId")?,
        now,
        None,
    )?;
    let (saved_key, saved_revision) = store
        .connection()
        .query_row(
            "SELECT * FROM revisions WHERE object_id=? AND revision=?",
            params![id_text, revision],
            revision_row,
        )
        .map_err(|e| store.query_error(e))?;
    let object = stat(store, Location::Id(&id))?;
    let saved_revision = complete_revision(store, &saved_key, saved_revision)?;
    Ok(LedgerResult::snapshot(IvyJson::object([
        ("object", object),
        ("revision", saved_revision),
    ])))
}
