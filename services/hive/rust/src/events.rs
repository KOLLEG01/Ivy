//! Durable event delivery on the storage owner's connection. Service identities
//! are supplied by bound dispatch; public parameters do not select a publisher.
use crate::store::{AuthenticatedContext, LedgerResult, Store};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, Row, params};

pub const MAXIMUM_SUBSCRIPTIONS_PER_NODE: i64 = 64;

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value.get(name).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a JsString> {
    field(value, name)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn number(value: &IvyJson, name: &str) -> IvyResult<f64> {
    field(value, name)?.as_f64().ok_or_else(IvyError::internal)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn decode(value: &str) -> IvyResult<IvyJson> {
    parse(value, usize::MAX).map_err(|_| IvyError::internal())
}

pub fn register_topic(store: &Store, definition: &IvyJson, owner: &JsString) -> IvyResult<()> {
    let schema = field(definition, "payloadSchema")?;
    store.validators().compile(schema)?;
    let topic = string(definition, "topic")?;
    let version = string(definition, "version")?;
    let encoded = canonical(
        &IvyJson::object([
            ("topic", IvyJson::String(topic.clone())),
            ("version", IvyJson::String(version.clone())),
            ("payloadSchema", schema.clone()),
        ]),
        MANAGEMENT_FRAME_BYTES,
    )?;
    let previous = store
        .connection()
        .query_row(
            "SELECT owner,definition FROM topics WHERE topic=? AND version=?",
            params![text(topic), text(version)],
            |row| {
                Ok((
                    JsString::from(row.get::<_, String>(0)?),
                    row.get::<_, String>(1)?,
                ))
            },
        )
        .optional()
        .map_err(|e| store.query_error(e))?;
    require(
        previous.is_none_or(|(old_owner, old)| old_owner == *owner && old == encoded),
        "registry_invalid",
        "Topic definitions are immutable and owner-bound.",
    )?;
    store
        .connection()
        .execute(
            "INSERT OR IGNORE INTO topics VALUES (?,?,?,?)",
            params![text(topic), text(version), text(owner), encoded],
        )
        .map_err(|e| store.query_error(e))?;
    Ok(())
}
fn declares_topic(registry: &IvyJson, topic: &JsString, version: &JsString) -> IvyResult<bool> {
    if registry.is_null() {
        return Ok(false);
    }
    for namespace in field(registry, "namespaces")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        for candidate in field(namespace, "topics")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            if candidate.is_null() {
                return Err(IvyError::internal());
            }
            if candidate.get("topic").and_then(IvyJson::as_js_string) == Some(topic)
                && candidate.get("version").and_then(IvyJson::as_js_string) == Some(version)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn publish(
    store: &mut Store,
    context: &AuthenticatedContext,
    service_node_id: &JsString,
    parameters: &IvyJson,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    // The original resolves the publisher before looking up a mutation receipt.
    let service = store
        .connection()
        .query_row(
            "SELECT service_name FROM service_nodes WHERE id=?",
            [text(service_node_id)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| store.query_error(e))?
        .ok_or_else(|| IvyError::new("not_found", "Service Node not found."))?;
    store.mutate(context,&"events.publish".into(),parameters,now_ms,|store,_| {
        let topic=string(parameters,"topic")?;
        let version=string(parameters,"topicVersion")?;
        let definition=store.connection().query_row("SELECT owner,definition FROM topics WHERE topic=? AND version=?",
            params![text(topic),text(version)],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))
            .optional().map_err(|e|store.query_error(e))?;
        require(definition.as_ref().is_some_and(|(owner,_)|*owner==format!("service:{service}")),
            "registry_invalid","The service does not own this topic version.")?;
        let registry=store.connection().query_row("SELECT registry_json FROM registries WHERE node_id=?",
            [text(service_node_id)],|row|row.get::<_,String>(0)).optional().map_err(|e|store.query_error(e))?;
        let declared=match registry {Some(encoded)=>declares_topic(&decode(&encoded)?,topic,version)?,None=>false};
        require(declared,"registry_invalid","Topic is not in this node's current registry.")?;
        let definition=decode(&definition.unwrap().1)?;
        let payload=field(parameters,"payload")?;
        store.validators().validate(field(&definition,"payloadSchema")?,payload,1024*1024)?;
        let payload=canonical(payload,4096)?;
        store.admit_event(false)?;
        store.connection().execute(
            "INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES (?,?,?,?,?,?)",
            params![text(topic),text(version),format!("service:{}",text(service_node_id)),text(now),text(string(parameters,"mutationId")?),payload],
        ).map_err(|e|store.query_error(e))?;
        Ok(LedgerResult::snapshot(IvyJson::object([
            ("sequence",IvyJson::Number(store.connection().last_insert_rowid() as f64)),
            ("topic",IvyJson::String(topic.clone())),("publishedAt",IvyJson::String(now.clone())),
        ])))
    })
}

struct Event {
    sequence: i64,
    topic: JsString,
    version: JsString,
    source: JsString,
    occurred_at: JsString,
    mutation_id: JsString,
    payload: String,
}
impl Event {
    fn row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            sequence: row.get("sequence")?,
            topic: row.get::<_, String>("topic")?.into(),
            version: row.get::<_, String>("topic_version")?.into(),
            source: row.get::<_, String>("source")?.into(),
            occurred_at: row.get::<_, String>("occurred_at")?.into(),
            mutation_id: row.get::<_, String>("mutation_id")?.into(),
            payload: row.get("payload")?,
        })
    }
    fn value(self) -> IvyResult<IvyJson> {
        Ok(IvyJson::object([
            ("sequence", IvyJson::Number(self.sequence as f64)),
            ("topic", IvyJson::String(self.topic)),
            ("topicVersion", IvyJson::String(self.version)),
            ("source", IvyJson::String(self.source)),
            ("occurredAt", IvyJson::String(self.occurred_at)),
            ("mutationId", IvyJson::String(self.mutation_id)),
            ("payload", decode(&self.payload)?),
        ]))
    }
}
fn has_more(store: &Store, through: f64) -> IvyResult<bool> {
    Ok(store
        .connection()
        .query_row(
            "SELECT 1 FROM events WHERE sequence>? LIMIT 1",
            [through],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| store.query_error(e))?
        .is_some())
}
pub fn head(store: &Store) -> IvyResult<IvyJson> {
    let pruned = store.metadata_number("events_pruned_through")?;
    let retained = store
        .connection()
        .query_row("SELECT MAX(sequence) FROM events", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .map_err(|e| store.query_error(e))?
        .unwrap_or(0) as f64;
    let through = pruned.max(retained);
    Ok(IvyJson::object([(
        "throughSequence",
        IvyJson::Number(through),
    )]))
}
fn batch(items: Vec<IvyJson>, through: f64, has_more: bool, gap: IvyJson) -> IvyJson {
    IvyJson::object([
        ("items", IvyJson::Array(items)),
        ("throughSequence", IvyJson::Number(through)),
        ("hasMore", has_more.into()),
        ("gap", gap),
    ])
}
fn includes(filter: &IvyJson, name: &str, value: &IvyJson) -> IvyResult<bool> {
    if filter.get(name).is_none_or(IvyJson::is_null) {
        return Ok(true);
    }
    Ok(field(filter, name)?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .contains(value))
}
fn matches(filter: &IvyJson, event: &IvyJson) -> IvyResult<bool> {
    if !includes(filter, "topics", field(event, "topic")?)?
        || !includes(filter, "sources", field(event, "source")?)?
    {
        return Ok(false);
    }
    if filter.get("objectIds").is_none_or(IvyJson::is_null) {
        return Ok(true);
    }
    if event.get("topic") != Some(&"hive.object.changed".into()) {
        return Ok(false);
    }
    let payload = field(event, "payload")?;
    if payload.is_null() {
        return Err(IvyError::internal());
    }
    match payload
        .get("objectId")
        .filter(|v| v.as_js_string().is_some())
    {
        Some(id) => includes(filter, "objectIds", id),
        None => Ok(false),
    }
}
pub fn read(store: &Store, parameters: &IvyJson) -> IvyResult<IvyJson> {
    let after = number(parameters, "afterSequence")?;
    let filter = field(parameters, "filter")?;
    let limit = parameters
        .get("limit")
        .filter(|v| !v.is_null())
        .and_then(IvyJson::as_f64)
        .unwrap_or(100.0);
    let pruned = store.metadata_number("events_pruned_through")?;
    if after < pruned {
        let gap = IvyJson::object([
            ("prunedThroughSequence", IvyJson::Number(pruned)),
            ("resumeAfterSequence", IvyJson::Number(pruned)),
        ]);
        return Ok(batch(vec![], pruned, has_more(store, pruned)?, gap));
    }
    let mut statement = store
        .connection()
        .prepare_cached("SELECT * FROM events WHERE sequence>? ORDER BY sequence LIMIT 100")
        .map_err(|e| store.query_error(e))?;
    let events = statement
        .query_map([after], Event::row)
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))?;
    let (mut through, mut bytes, mut items) = (after, 128, Vec::new());
    for event in events {
        store.check_budget()?;
        let event = event.value()?;
        let matched = matches(filter, &event)?;
        let sequence = number(&event, "sequence")?;
        if matched {
            let size = canonical(&event, MANAGEMENT_FRAME_BYTES)?.len();
            if bytes + size > 1024 * 1024 - 1024 {
                require(
                    !items.is_empty(),
                    "result_too_large",
                    "Journal event cannot fit a deliverable batch.",
                )?;
                break;
            }
            bytes += size;
            items.push(event);
        }
        through = sequence;
        if items.len() as f64 >= limit {
            break;
        }
    }
    Ok(batch(
        items,
        through,
        has_more(store, through)?,
        IvyJson::Null,
    ))
}

struct Subscription {
    filter: String,
    initial: f64,
    acknowledged: f64,
    issued: Option<String>,
}
fn subscription(
    store: &Store,
    node: &JsString,
    name: &JsString,
) -> IvyResult<Option<Subscription>> {
    store
        .connection()
        .query_row(
            "SELECT * FROM subscriptions WHERE node_id=? AND name=?",
            params![text(node), text(name)],
            |row| {
                Ok(Subscription {
                    filter: row.get("filter_json")?,
                    initial: row.get("initial_sequence")?,
                    acknowledged: row.get("acknowledged_sequence")?,
                    issued: row.get("batch_json")?,
                })
            },
        )
        .optional()
        .map_err(|e| store.query_error(e))
}
pub fn subscribe(
    store: &mut Store,
    service_node_id: &JsString,
    parameters: &IvyJson,
) -> IvyResult<IvyJson> {
    store.transaction(|store| {
        if parameters.get("durable").and_then(IvyJson::as_bool) == Some(true) {
            let service=store.connection().query_row("SELECT service_name FROM service_nodes WHERE id=?",[text(service_node_id)],|row|row.get::<_,String>(0))
                .optional().map_err(|e|store.query_error(e))?;
            require(service.as_deref()==Some("secretary"),"forbidden","Durable event subscriptions belong to Secretary.")?;
        }
        let filter=field(parameters,"filter")?;
        let encoded=canonical(filter,MANAGEMENT_FRAME_BYTES)?;
        let name=string(parameters,"name")?;
        let mut saved=subscription(store,service_node_id,name)?;
        if saved.is_none() {
            let count=store.connection().query_row("SELECT COUNT(*) FROM subscriptions WHERE node_id=?",[text(service_node_id)],|row|row.get::<_,i64>(0))
                .map_err(|e|store.query_error(e))?;
            require(count<MAXIMUM_SUBSCRIPTIONS_PER_NODE,"capacity_exceeded","Service Node subscription limit reached.")?;
            let initial=parameters.get("initialSequence").filter(|v|!v.is_null()).and_then(IvyJson::as_f64).unwrap_or(0.0);
            store.connection().execute("INSERT INTO subscriptions(node_id,name,filter_json,initial_sequence,acknowledged_sequence,batch_json) VALUES (?,?,?,?,?,NULL)",
                params![text(service_node_id),text(name),encoded,initial,initial]).map_err(|e|store.query_error(e))?;
            saved=subscription(store,service_node_id,name)?;
        }
        let saved=saved.ok_or_else(IvyError::internal)?;
        require(saved.filter==encoded&&parameters.get("initialSequence").is_none_or(|v|v.as_f64()==Some(saved.initial)),
            "subscription_filter_conflict","Subscription filter and initial cursor are immutable.")?;
        if parameters.get("durable").and_then(IvyJson::as_bool) == Some(true) {
            store.connection().execute("INSERT OR IGNORE INTO durable_subscriptions(node_id,name) VALUES (?,?)",params![text(service_node_id),text(name)])
                .map_err(|e|store.query_error(e))?;
        }
        if let Some(issued)=saved.issued.filter(|v|!v.is_empty()) {
            let issued=decode(&issued)?;
            let through=number(&issued,"throughSequence")?;
            let more=field(&issued,"hasMore")?.clone();
            if issued.get("gap").is_some_and(|v| v.as_object().is_some()) {
                return Ok(IvyJson::object([("items",IvyJson::Array(vec![])),("throughSequence",IvyJson::Number(through)),("hasMore",more),("gap",field(&issued,"gap")?.clone())]));
            }
            let mut records=Vec::new();
            for sequence in field(&issued,"sequences")?.as_array().ok_or_else(IvyError::internal)? {
                records.push(store.connection().query_row("SELECT * FROM events WHERE sequence=?",[sequence.as_f64().ok_or_else(IvyError::internal)?],Event::row)
                    .optional().map_err(|e|store.query_error(e))?);
            }
            if records.iter().all(Option::is_some) {
                let items=records.into_iter().map(|v|v.unwrap().value()).collect::<IvyResult<Vec<_>>>()?;
                return Ok(IvyJson::object([("items",IvyJson::Array(items)),("throughSequence",IvyJson::Number(through)),("hasMore",more),("gap",IvyJson::Null)]));
            }
            store.connection().execute("UPDATE subscriptions SET batch_json=NULL WHERE node_id=? AND name=?",params![text(service_node_id),text(name)])
                .map_err(|e|store.query_error(e))?;
        }
        let mut request=IvyJson::object([("afterSequence",IvyJson::Number(saved.acknowledged)),("filter",filter.clone())]);
        if let Some(limit)=parameters.get("limit") {request.insert("limit",limit.clone());}
        let delivered=read(store,&request)?;
        let sequences=field(&delivered,"items")?.as_array().unwrap().iter().map(|v|field(v,"sequence").cloned()).collect::<IvyResult<Vec<_>>>()?;
        let mut reference=IvyJson::object([("sequences",IvyJson::Array(sequences)),("throughSequence",field(&delivered,"throughSequence")?.clone()),("hasMore",field(&delivered,"hasMore")?.clone())]);
        if delivered.get("gap").is_some_and(|v| v.as_object().is_some()) {reference.insert("gap",field(&delivered,"gap")?.clone());}
        store.connection().execute("UPDATE subscriptions SET batch_json=? WHERE node_id=? AND name=?",
            params![canonical(&reference,MANAGEMENT_FRAME_BYTES)?,text(service_node_id),text(name)]).map_err(|e|store.query_error(e))?;
        Ok(delivered)
    })
}
pub fn ack(
    store: &mut Store,
    service_node_id: &JsString,
    parameters: &IvyJson,
) -> IvyResult<IvyJson> {
    store.transaction(|store| {
        let name=string(parameters,"name")?;
        let saved=subscription(store,service_node_id,name)?.ok_or_else(||IvyError::new("not_found","Subscription not found."))?;
        let issued=saved.issued.filter(|v|!v.is_empty()).map(|v|decode(&v)).transpose()?.unwrap_or(IvyJson::Null);
        let through=field(parameters,"throughSequence")?;
        let result=IvyJson::object([("acknowledgedSequence",through.clone())]);
        if through.as_f64()==Some(saved.acknowledged)&&issued.get("throughSequence")!=Some(through) {return Ok(result);}
        require(issued.get("throughSequence")==Some(through),"invalid_ack","Acknowledgement must equal the outstanding delivered boundary.")?;
        let gap=issued.get("gap");
        require(if gap.is_some_and(|v| v.as_object().is_some()) { parameters.get("gapThroughSequence")==gap.and_then(|v|v.get("prunedThroughSequence")) }else { parameters.get("gapThroughSequence").is_none() },
            "invalid_ack","A reported journal gap must be acknowledged explicitly.")?;
        store.connection().execute("UPDATE subscriptions SET acknowledged_sequence=?,batch_json=NULL WHERE node_id=? AND name=?",
            params![through.as_f64().ok_or_else(IvyError::internal)?,text(service_node_id),text(name)]).map_err(|e|store.query_error(e))?;
        store.prune_event_journal_for_capacity()?;
        Ok(result)
    })
}
pub fn unsubscribe(
    store: &mut Store,
    context: &AuthenticatedContext,
    service_node_id: &JsString,
    parameters: &IvyJson,
    now_ms: u64,
) -> IvyResult<IvyJson> {
    store.mutate(
        context,
        &"events.unsubscribe".into(),
        parameters,
        now_ms,
        |store, _| {
            let name = string(parameters, "name")?;
            store
                .connection()
                .execute(
                    "DELETE FROM durable_subscriptions WHERE node_id=? AND name=?",
                    params![text(service_node_id), text(name)],
                )
                .map_err(|e| store.query_error(e))?;
            let removed = store
                .connection()
                .execute(
                    "DELETE FROM subscriptions WHERE node_id=? AND name=?",
                    params![text(service_node_id), text(name)],
                )
                .map_err(|e| store.query_error(e))?
                > 0;
            if removed {
                store.prune_event_journal_for_capacity()?;
            }
            Ok(LedgerResult::snapshot(IvyJson::object([(
                "removed",
                removed.into(),
            )])))
        },
    )
}
