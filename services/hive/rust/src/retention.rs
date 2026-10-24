//! Policy previews and single-owner retention passes. Each family, the object
//! deletion set and final journal/UI/diagnostic cleanup retain separate commits.
use crate::store::{EVENT_MAX_COUNT, Store};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, compare_versions, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params, types::Value};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

const DAY: f64 = 86_400_000.0;
const REVISION_BATCH_SIZE: usize = 500;
const EVENT_MAX_BYTES: i64 = 64 * 1024 * 1024;

/// Date and contract ordering services used by the retention policy.
pub trait Runtime {
    fn parse_date(&self, value: &JsString) -> f64;
    fn iso_date(&self, milliseconds: f64) -> IvyResult<JsString>;
    fn compare_contract_keys(&self, a: &JsString, b: &JsString) -> Ordering;
}
pub trait DiagnosticCollector {
    fn collect_diagnostics(&mut self, store: &mut Store, now: f64) -> IvyResult<()>;
}
impl<F: FnMut(&mut Store, f64) -> IvyResult<()>> DiagnosticCollector for F {
    fn collect_diagnostics(&mut self, store: &mut Store, now: f64) -> IvyResult<()> {
        self(store, now)
    }
}
fn sql<T>(store: &Store, result: rusqlite::Result<T>) -> IvyResult<T> {
    result.map_err(|e| store.query_error(e))
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn decode(value: &str) -> IvyResult<IvyJson> {
    parse(value, usize::MAX).map_err(|_| IvyError::internal())
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn encoded(value: &IvyJson) -> IvyResult<String> {
    canonical(value, MANAGEMENT_FRAME_BYTES)
}
fn number(value: &IvyJson) -> IvyResult<f64> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| {
            IvyError::new(
                "storage_invalid",
                "Retained retention limits must be nonnegative numbers.",
            )
        })
}
fn iso(runtime: &dyn Runtime, now: f64) -> IvyResult<String> {
    Ok(text(&runtime.iso_date(now)?))
}

struct Contract {
    key: JsString,
    version: IvyJson,
    policy: IvyJson,
}
fn waiting_for_owner(store: &Store, contract: &Contract) -> IvyResult<bool> {
    if contract
        .policy
        .get("awaitOwnerReadiness")
        .and_then(IvyJson::as_bool)
        != Some(true)
    {
        return Ok(false);
    }
    let key = format!(
        "retention_owner_ready:{}:{}",
        text(&contract.key),
        contract.version.as_str()?
    );
    let acknowledgement = hash_json(&IvyJson::object([
        ("version", contract.version.clone()),
        ("policy", contract.policy.clone()),
    ]))?;
    Ok(store.metadata(&key)?.as_deref() != Some(acknowledgement.as_str()))
}
fn contracts(store: &Store, runtime: &dyn Runtime) -> IvyResult<Vec<Contract>> {
    let mut latest: Vec<(JsString, String, IvyJson)> = vec![];
    let mut indices: HashMap<JsString, usize> = HashMap::new();
    let mut statement = sql(
        store,
        store
            .connection()
            .prepare_cached("SELECT definition FROM contracts"),
    )?;
    let rows = sql(store, statement.query_map([], |r| r.get::<_, String>(0)))?;
    for row in rows {
        let value = decode(&sql(store, row)?)?;
        let key = field(&value, "key")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?
            .clone();
        let version = field(&value, "version")?.as_str()?;
        let index = indices.get(&key).copied();
        if let Some(index) = index {
            if compare_versions(&version, &latest[index].1)? == Ordering::Greater {
                latest[index] = (key, version, value);
            }
        } else {
            indices.insert(key.clone(), latest.len());
            latest.push((key, version, value));
        }
    }
    latest.sort_by(|a, b| runtime.compare_contract_keys(&a.0, &b.0));
    latest
        .into_iter()
        .map(|(key, _, value)| {
            Ok(Contract {
                key,
                version: field(&value, "version")?.clone(),
                policy: field(&value, "retention")?.clone(),
            })
        })
        .collect()
}

struct CandidatePlan {
    initial: Vec<(String, JsString)>,
    requirements: HashMap<String, HashSet<String>>,
    dependents: HashMap<String, HashSet<String>>,
}
impl CandidatePlan {
    fn new(
        store: &Store,
        contracts: &[Contract],
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<Self> {
        let mut result = Self {
            initial: vec![],
            requirements: HashMap::new(),
            dependents: HashMap::new(),
        };
        let policies: HashMap<_, _> = contracts
            .iter()
            .map(|c| Ok((c.key.clone(), field(&c.policy, "objects")?)))
            .collect::<IvyResult<_>>()?;
        let mut statement=sql(store,store.connection().prepare_cached("SELECT id,parent_id,owner_object_id,contract_key,created_at,updated_at FROM objects ORDER BY id"))?;
        let rows = sql(
            store,
            statement.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            }),
        )?;
        for row in rows {
            let (id, parent, owner, key, created, updated) = sql(store, row)?;
            let key = JsString::from(key);
            if let Some(policy) = policies.get(&key) {
                let mode = policy.get("mode");
                if (mode == Some(&"owned".into())
                    && runtime.parse_date(&created.into()) <= now - DAY)
                    || (mode == Some(&"expire".into())
                        && runtime.parse_date(&updated.into())
                            <= now - number(field(policy, "maximumAgeDays")?)? * DAY)
                {
                    result.initial.push((id.clone(), key));
                }
            }
            for target in [parent, owner]
                .into_iter()
                .flatten()
                .filter(|v| !v.is_empty())
            {
                result.require(target, id.clone());
            }
        }
        let mut statement = sql(
            store,
            store.connection().prepare_cached(
                "SELECT DISTINCT source_object_id,target_object_id FROM revision_references",
            ),
        )?;
        for row in sql(
            store,
            statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))),
        )? {
            let (source, target) = sql(store, row)?;
            result.require(target, source);
        }
        Ok(result)
    }
    fn require(&mut self, target: String, required: String) {
        self.requirements
            .entry(target.clone())
            .or_default()
            .insert(required.clone());
        self.dependents.entry(required).or_default().insert(target);
    }
    fn candidates(&self, enabled: Option<&HashSet<JsString>>) -> Vec<(String, JsString)> {
        let initial: Vec<_> = self
            .initial
            .iter()
            .filter(|(_, key)| enabled.is_none_or(|set| set.contains(key)))
            .collect();
        let mut candidates: HashSet<_> = initial.iter().map(|(id, _)| id.clone()).collect();
        let mut queue: Vec<_> = initial
            .iter()
            .filter(|(id, _)| {
                self.requirements
                    .get(id)
                    .is_some_and(|required| !required.is_subset(&candidates))
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut index = 0;
        while index < queue.len() {
            let id = queue[index].clone();
            index += 1;
            if !candidates.remove(&id) {
                continue;
            }
            for target in self.dependents.get(&id).into_iter().flatten() {
                if candidates.contains(target) {
                    queue.push(target.clone());
                }
            }
        }
        initial
            .into_iter()
            .filter(|(id, _)| candidates.contains(id))
            .cloned()
            .collect()
    }
}
#[derive(Default)]
struct Summary {
    objects: i64,
    revisions: i64,
    bytes: i64,
    eligible_objects: i64,
    eligible_revisions: i64,
    protected: i64,
}
type Saved = (String, String);
fn saved(store: &Store, key: &JsString) -> IvyResult<Option<Saved>> {
    sql(
        store,
        store
            .connection()
            .query_row(
                "SELECT policy_hash,summary_json FROM retention_families WHERE contract_key=?",
                [text(key)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional(),
    )
}
fn summary_value(saved: Option<&Saved>) -> IvyResult<Option<IvyJson>> {
    saved
        .filter(|(_, v)| !v.is_empty())
        .map(|(_, v)| {
            decode(v).and_then(|v| {
                if v.is_null() {
                    Err(IvyError::internal())
                } else {
                    Ok(v)
                }
            })
        })
        .transpose()
}
pub(crate) fn integer(value: &IvyJson) -> Option<i64> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && v.fract() == 0.0 && *v >= 0.0 && *v <= 9_007_199_254_740_991.0)
        .map(|value| value as i64)
}
fn stored_summary(saved: Option<&Saved>) -> IvyResult<Summary> {
    let value = summary_value(saved)?;
    let n = |key| {
        value
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(integer)
            .unwrap_or(0)
    };
    Ok(Summary {
        objects: n("objectCount"),
        revisions: n("revisionCount"),
        bytes: n("byteLength"),
        eligible_objects: n("eligibleObjects"),
        eligible_revisions: n("eligibleRevisions"),
        protected: n("protectedRevisions"),
    })
}
fn has_stored_usage(saved: Option<&Saved>) -> IvyResult<bool> {
    let value = summary_value(saved)?;
    Ok(value.is_some_and(|v| {
        ["objectCount", "revisionCount", "byteLength"]
            .iter()
            .all(|key| v.get(key).and_then(integer).is_some())
    }))
}
fn totals(store: &Store, key: &JsString) -> IvyResult<(i64, i64, i64)> {
    let objects = sql(
        store,
        store.connection().query_row(
            "SELECT COUNT(*) AS objects FROM objects WHERE contract_key=?",
            [text(key)],
            |r| r.get(0),
        ),
    )?;
    let (revisions,bytes)=sql(store,store.connection().query_row("SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE contract_key=?",[text(key)],|r|Ok((r.get(0)?,r.get(1)?))))?;
    Ok((objects, revisions, bytes))
}
fn candidates(
    store: &Store,
    contract: &Contract,
    runtime: &dyn Runtime,
    now: f64,
    objects: &HashSet<String>,
) -> IvyResult<Summary> {
    let mut result = Summary::default();
    let policy = field(&contract.policy, "revisions")?;
    let mode = field(policy, "mode")?;
    let mut statement = sql(
        store,
        store.connection().prepare_cached(
            "SELECT id,current_revision FROM objects WHERE contract_key=? ORDER BY id",
        ),
    )?;
    for object in sql(
        store,
        statement.query_map([text(&contract.key)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        }),
    )? {
        let (id, current) = sql(store, object)?;
        // The original reads history even for an object selected for deletion.
        let mut history=sql(store,store.connection().prepare_cached("SELECT revision,created_at FROM revisions WHERE object_id=? ORDER BY revision DESC"))?;
        let history = sql(
            store,
            sql(
                store,
                history.query_map([&id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))),
            )?
            .collect::<rusqlite::Result<Vec<_>>>(),
        )?;
        if objects.contains(&id) {
            result.eligible_objects += 1;
            continue;
        }
        for (revision, created) in history {
            if revision == current || mode == &"all".into() {
                continue;
            }
            let eligible = mode == &"current".into()
                || (mode == &"bounded".into()
                    && (policy
                        .get("maximumCount")
                        .map(|v| revision_limit(v).map(|n| revision <= current - n))
                        .transpose()?
                        .unwrap_or(false)
                        || policy
                            .get("maximumAgeDays")
                            .map(|v| {
                                number(v)
                                    .map(|n| runtime.parse_date(&created.into()) <= now - n * DAY)
                            })
                            .transpose()?
                            .unwrap_or(false)));
            if !eligible {
                continue;
            }
            let pin=sql(store,store.connection().query_row("SELECT 1 FROM revision_references WHERE target_object_id=? AND target_revision=? LIMIT 1",params![id,revision],|r|r.get::<_,i64>(0)).optional())?;
            if pin.is_some() {
                result.protected += 1;
            } else {
                result.eligible_revisions += 1;
            }
        }
    }
    Ok(result)
}
struct Revision {
    id: String,
    revision: i64,
    bytes: i64,
}
fn revision_limit(value: &IvyJson) -> IvyResult<i64> {
    integer(value).ok_or_else(|| {
        IvyError::new(
            "storage_invalid",
            "Retained revision counts must be nonnegative integers.",
        )
    })
}
fn revision_batch(
    store: &Store,
    contract: &Contract,
    runtime: &dyn Runtime,
    now: f64,
) -> IvyResult<Vec<Revision>> {
    let policy = field(&contract.policy, "revisions")?;
    let mode = field(policy, "mode")?;
    if mode == &"all".into() {
        return Ok(vec![]);
    }
    let mut eligible = vec![];
    let mut values = vec![Value::Text(text(&contract.key))];
    if mode == &"current".into() {
        eligible.push("1");
    } else {
        if let Some(value) = policy.get("maximumCount") {
            eligible.push("r.revision<=o.current_revision-?");
            values.push(Value::Integer(revision_limit(value)?));
        }
        if let Some(value) = policy.get("maximumAgeDays") {
            eligible.push("r.created_at<=?");
            values.push(Value::Text(iso(runtime, now - number(value)? * DAY)?));
        }
    }
    values.push(Value::Integer((REVISION_BATCH_SIZE + 1) as i64));
    let query = format!(
        "SELECT r.object_id,r.revision,r.byte_length FROM revisions r
         JOIN objects o ON o.id=r.object_id
         WHERE r.contract_key=? AND r.revision<>o.current_revision
           AND ({})
           AND NOT EXISTS (
             SELECT 1 FROM revision_references x
             WHERE x.target_object_id=r.object_id AND x.target_revision=r.revision
           )
         LIMIT ?",
        eligible.join(" OR ")
    );
    let mut statement = sql(store, store.connection().prepare_cached(&query))?;
    sql(
        store,
        sql(
            store,
            statement.query_map(rusqlite::params_from_iter(values), |r| {
                Ok(Revision {
                    id: r.get(0)?,
                    revision: r.get(1)?,
                    bytes: r.get(2)?,
                })
            }),
        )?
        .collect(),
    )
}

pub fn preview(store: &Store, runtime: &dyn Runtime, now: f64) -> IvyResult<IvyJson> {
    build_status(store, runtime, now, true)
}
pub fn status(store: &Store, runtime: &dyn Runtime, now: f64) -> IvyResult<IvyJson> {
    build_status(store, runtime, now, false)
}
fn build_status(store: &Store, runtime: &dyn Runtime, now: f64, scan: bool) -> IvyResult<IvyJson> {
    let contracts = contracts(store, runtime)?;
    let objects: HashSet<_> = if scan {
        CandidatePlan::new(store, &contracts, runtime, now)?
            .candidates(None)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    } else {
        HashSet::new()
    };
    let mut families = vec![];
    for contract in contracts {
        let hash = hash_json(&contract.policy)?;
        let saved = saved(store, &contract.key)?;
        let same = saved.as_ref().is_some_and(|(old, _)| old == &hash);
        let selected = if scan {
            Some(candidates(store, &contract, runtime, now, &objects)?)
        } else {
            None
        };
        let mut summary = stored_summary(saved.as_ref())?;
        if scan || !same {
            (summary.objects, summary.revisions, summary.bytes) = totals(store, &contract.key)?;
        }
        let eligibility = selected.as_ref().unwrap_or(&summary);
        let waiting = waiting_for_owner(store, &contract)?;
        families.push(IvyJson::object([
            ("contractKey", IvyJson::String(contract.key)),
            ("policy", contract.policy),
            ("policyHash", hash.into()),
            ("previewRequired", (!same).into()),
            ("waitingForOwner", waiting.into()),
            ("objectCount", IvyJson::Number(summary.objects as f64)),
            ("revisionCount", IvyJson::Number(summary.revisions as f64)),
            ("byteLength", IvyJson::Number(summary.bytes as f64)),
            (
                "eligibleObjectCount",
                IvyJson::Number(
                    (if scan || same {
                        eligibility.eligible_objects
                    } else {
                        0
                    }) as f64,
                ),
            ),
            (
                "eligibleRevisionCount",
                IvyJson::Number(
                    (if scan || same {
                        eligibility.eligible_revisions
                    } else {
                        0
                    }) as f64,
                ),
            ),
            (
                "protectedRevisionCount",
                IvyJson::Number(
                    (if scan || same {
                        eligibility.protected
                    } else {
                        0
                    }) as f64,
                ),
            ),
        ]));
    }
    let (events,bytes)=sql(store,store.connection().query_row("SELECT COUNT(*) AS count,COALESCE(SUM(length(CAST(payload AS BLOB))),0) AS bytes FROM events",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))))?;
    let failure = store
        .metadata("retention_last_failure")?
        .filter(|v| !v.is_empty())
        .map(|v| decode(&v))
        .transpose()?
        .unwrap_or(IvyJson::Null);
    Ok(IvyJson::object([
        ("checkedAt", iso(runtime, now)?.into()),
        ("nextPassAt", iso(runtime, now + 15.0 * 60_000.0)?.into()),
        ("families", IvyJson::Array(families)),
        (
            "lastSuccessAt",
            store
                .metadata("retention_last_success")?
                .filter(|v| !v.is_empty())
                .map(IvyJson::from)
                .unwrap_or(IvyJson::Null),
        ),
        ("lastFailure", failure),
        (
            "events",
            IvyJson::object([
                ("count", IvyJson::Number(events as f64)),
                ("byteLength", IvyJson::Number(bytes as f64)),
                (
                    "prunedThroughSequence",
                    IvyJson::Number(store.metadata_integer("events_pruned_through")? as f64),
                ),
            ]),
        ),
        (
            "mutations",
            IvyJson::object([
                (
                    "count",
                    IvyJson::Number(sql(
                        store,
                        store.connection().query_row(
                            "SELECT COUNT(*) AS count FROM mutations",
                            [],
                            |r| r.get::<_, i64>(0),
                        ),
                    )? as f64),
                ),
                (
                    "byteLength",
                    IvyJson::Number(store.mutation_receipt_bytes()? as f64),
                ),
                (
                    "expiredBefore",
                    IvyJson::Number(store.metadata_integer("mutations_expired_before")? as f64),
                ),
            ]),
        ),
    ]))
}

pub fn collect(
    store: &mut Store,
    runtime: &dyn Runtime,
    now: f64,
    mut diagnostics: Option<&mut dyn DiagnosticCollector>,
) -> IvyResult<IvyJson> {
    let contracts = contracts(store, runtime)?;
    let plan = CandidatePlan::new(store, &contracts, runtime, now)?;
    let preview_objects: HashSet<_> = plan
        .candidates(None)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let mut enabled = HashSet::new();
    for contract in &contracts {
        if waiting_for_owner(store, contract)? {
            continue;
        }
        let active=store.transaction(|store|{
            let hash=hash_json(&contract.policy)?;
            let saved=saved(store,&contract.key)?;
            let active=saved.as_ref().is_some_and(|(old,_)|old==&hash);
            let summary=if active{
                let mut prior=stored_summary(saved.as_ref())?;
                if !has_stored_usage(saved.as_ref())?{(prior.objects,prior.revisions,prior.bytes)=totals(store,&contract.key)?;}
                let batch=revision_batch(store,contract,runtime,now)?;
                let revisions=&batch[..batch.len().min(REVISION_BATCH_SIZE)];
                for revision in revisions {sql(store,store.connection().execute("DELETE FROM revisions WHERE object_id=? AND revision=?",params![revision.id,revision.revision]))?;}
                if !revisions.is_empty(){store.invalidate(format!("objects/{}",text(&contract.key)).into());store.invalidate("system".into());}
                Summary{objects:prior.objects,revisions:(prior.revisions-revisions.len() as i64).max(0),bytes:(prior.bytes-revisions.iter().map(|r|r.bytes).sum::<i64>()).max(0),eligible_objects:0,
                    eligible_revisions:(prior.eligible_revisions-revisions.len() as i64).max(if batch.len()>REVISION_BATCH_SIZE{1}else{0}),protected:prior.protected}
            }else{
                let mut preview=candidates(store,contract,runtime,now,&preview_objects)?;
                (preview.objects,preview.revisions,preview.bytes)=totals(store,&contract.key)?;preview
            };
            let encoded=encoded(&IvyJson::object([
                ("checkedAt",iso(runtime,now)?.into()),("objectCount",IvyJson::Number(summary.objects as f64)),("revisionCount",IvyJson::Number(summary.revisions as f64)),("byteLength",IvyJson::Number(summary.bytes as f64)),
                ("eligibleObjects",IvyJson::Number(summary.eligible_objects as f64)),("eligibleRevisions",IvyJson::Number(summary.eligible_revisions as f64)),("protectedRevisions",IvyJson::Number(summary.protected as f64)),
            ]))?;
            sql(store,store.connection().execute("INSERT INTO retention_families VALUES (?,?,1,?) ON CONFLICT(contract_key) DO UPDATE SET policy_hash=excluded.policy_hash,previewed=1,summary_json=excluded.summary_json",params![text(&contract.key),hash,encoded]))?;
            Ok(active)
        })?;
        if active {
            enabled.insert(contract.key.clone());
        }
    }
    let objects = plan.candidates(Some(&enabled));
    if !objects.is_empty() {
        store.transaction(|store|{
            let mut rows=vec![];
            for (id,_) in &objects {
                rows.push(sql(store,store.connection().query_row("SELECT id,depth,owner_object_id,contract_key FROM objects WHERE id=?",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?))))?);
            }
            rows.sort_by(|a,b|b.2.as_ref().is_some_and(|v|!v.is_empty()).cmp(&a.2.as_ref().is_some_and(|v|!v.is_empty())).then_with(||b.1.cmp(&a.1)));
            for (id,_,_,key) in &rows {
                let (revisions,bytes)=sql(store,store.connection().query_row("SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE object_id=?",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))))?;
                store.adjust_retention_usage(&key.as_str().into(),-1,-revisions,-bytes)?;
            }
            for (id,_) in &objects {sql(store,store.connection().execute("DELETE FROM revision_references WHERE source_object_id=?",[id]))?;}
            for (id,_) in &objects {
                sql(store,store.connection().execute("DELETE FROM object_fts WHERE object_id=?",[id]))?;
                sql(store,store.connection().execute("DELETE FROM revisions WHERE object_id=?",[id]))?;
            }
            for (id,_,_,_) in &rows {sql(store,store.connection().execute("DELETE FROM objects WHERE id=?",[id]))?;}
            store.invalidate("objects".into());store.invalidate("system".into());
            Ok(())
        })?;
    }
    store.transaction(|store|{
        collect_events(store,runtime,now)?;
        store.cleanup_mutation_receipts(0,0)?;
        let apps={
            let mut statement=sql(store,store.connection().prepare_cached("SELECT app_id,current_release_id FROM apps"))?;
            sql(store,sql(store,statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?))))?.collect::<rusqlite::Result<Vec<_>>>())?
        };
        for (id,current) in apps {
            sql(store,store.connection().execute("DELETE FROM app_releases WHERE app_id=? AND release_id IS NOT ?",params![id,current]))?;
            sql(store,store.connection().execute("UPDATE apps SET previous_release_id=NULL WHERE app_id=?",params![id]))?;
        }
        if let Some(diagnostics)=diagnostics.as_mut(){diagnostics.collect_diagnostics(store,now)?;}
        store.set_metadata("retention_last_success",&iso(runtime,now)?)?;
        store.set_metadata("retention_last_failure","")?;
        Ok(())
    })?;
    status(store, runtime, now)
}
pub fn record_failure(
    store: &Store,
    runtime: &dyn Runtime,
    message: &JsString,
    now: f64,
) -> IvyResult<()> {
    let message = JsString(message.0[..message.0.len().min(512)].to_vec());
    store.set_metadata(
        "retention_last_failure",
        &encoded(&IvyJson::object([
            ("at", iso(runtime, now)?.into()),
            ("message", IvyJson::String(message)),
        ]))?,
    )
}
fn collect_events(store: &Store, runtime: &dyn Runtime, now: f64) -> IvyResult<()> {
    let pruned = store.metadata_integer("events_pruned_through")?;
    let age = sql(
        store,
        store.connection().query_row(
            "SELECT MAX(sequence) AS sequence FROM events WHERE occurred_at<?",
            [iso(runtime, now - 7.0 * DAY)?],
            |r| r.get::<_, Option<i64>>(0),
        ),
    )?
    .unwrap_or(0);
    let mut boundary = pruned.max(age);
    let count = sql(
        store,
        store
            .connection()
            .query_row("SELECT COUNT(*) AS count FROM events", [], |r| {
                r.get::<_, i64>(0)
            }),
    )?;
    if count > EVENT_MAX_COUNT {
        let candidate = sql(
            store,
            store
                .connection()
                .query_row(
                    "SELECT sequence FROM events ORDER BY sequence LIMIT 1 OFFSET ?",
                    [count - EVENT_MAX_COUNT - 1],
                    |r| r.get::<_, i64>(0),
                )
                .optional(),
        )?
        .unwrap_or(0);
        boundary = boundary.max(candidate);
    }
    let mut bytes = 0_i64;
    {
        let mut statement=sql(store,store.connection().prepare_cached("SELECT sequence,length(CAST(payload AS BLOB)) AS bytes FROM events ORDER BY sequence DESC"))?;
        for row in sql(
            store,
            statement.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))),
        )? {
            let (sequence, size) = sql(store, row)?;
            bytes += size;
            if bytes > EVENT_MAX_BYTES {
                boundary = boundary.max(sequence);
                break;
            }
        }
    }
    boundary = boundary.min(
        store
            .durable_event_boundary()?
            .unwrap_or(9_007_199_254_740_991),
    );
    if boundary <= pruned {
        return Ok(());
    }
    sql(
        store,
        store
            .connection()
            .execute("DELETE FROM events WHERE sequence<=?", [boundary]),
    )?;
    store.set_metadata("events_pruned_through", &boundary.to_string())?;
    sql(
        store,
        store.connection().execute(
            "UPDATE subscriptions SET batch_json=NULL WHERE acknowledged_sequence<?",
            [boundary],
        ),
    )?;
    Ok(())
}
