//! Session-bound push storage. Network delivery has a separate owner.
use crate::store::{AuthContext, Store};
use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::digest,
    json::{js_string, number_from_string, parse, stringify},
    whatwg_url::{parse_url, parse_url_with_base},
};
use rusqlite::{OptionalExtension, params};
use std::{rc::Rc, sync::OnceLock};

/// Required cryptographic primitives; no permissive validator or generated-key
/// fallback is installed by the storage domain.
pub trait Crypto {
    fn generate_vapid_keys(&self) -> IvyResult<IvyJson>;
    fn validate_p256_public_key(&self, bytes: &[u8]) -> IvyResult<()>;
}
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
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn decode(value: &str) -> IvyResult<IvyJson> {
    parse(value, usize::MAX).map_err(|_| IvyError::internal())
}
fn encoded(value: &IvyJson) -> IvyResult<String> {
    stringify(value, usize::MAX)
}
fn head(store: &Store) -> IvyResult<f64> {
    sql(
        store,
        store
            .connection()
            .query_row("SELECT MAX(sequence) AS n FROM events", [], |r| {
                r.get::<_, Option<f64>>(0)
            }),
    )
    .map(|v| v.unwrap_or(0.0))
}
fn truthy(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::Bool(v)) => *v,
        Some(IvyJson::Number(v)) => *v != 0.0 && !v.is_nan(),
        Some(IvyJson::String(v)) => !v.0.is_empty(),
        _ => true,
    }
}
fn notice_schema() -> &'static IvyJson {
    static SCHEMA: OnceLock<IvyJson> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        parse(
            include_str!("../../../../specs/schemas/browser-notice.schema.json"),
            usize::MAX,
        )
        .expect("Embedded browser notice schema")
    })
}
pub fn push_subscription(input: &IvyJson, crypto: &dyn Crypto) -> IvyResult<IvyJson> {
    let endpoint = input.get("endpoint").and_then(IvyJson::as_js_string);
    require(
        endpoint.is_some_and(|v| v.0.len() <= 4096),
        "invalid_arguments",
        "A push subscription is required.",
    )?;
    let url = parse_url(endpoint.unwrap())
        .map_err(|_| IvyError::new("invalid_arguments", "Invalid push endpoint."))?;
    // On the accepted ASCII DNS hosts, a normalized authority equal to the
    // hostname has neither a non-default port nor user information. Default
    // HTTPS ports have already been removed by the shared URL constructor.
    let authority = url
        .href
        .strip_prefix("https://")
        .and_then(|v| v.split(['/', '?', '#']).next());
    require(
        url.protocol == "https:"
            && url.username.is_empty()
            && url.password.is_empty()
            && url.hash.is_empty()
            && authority == Some(url.hostname.as_str())
            && (matches!(
                url.hostname.as_str(),
                "fcm.googleapis.com" | "jmt17.google.com" | "updates.push.services.mozilla.com"
            ) || [
                ".push.services.mozilla.com",
                ".push.apple.com",
                ".notify.windows.com",
            ]
            .iter()
            .any(|v| url.hostname.ends_with(v))),
        "invalid_arguments",
        "Unsupported browser push service.",
    )?;
    let engine = GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let key = |name, length| -> IvyResult<(&JsString, Vec<u8>)> {
        let value = input
            .get("keys")
            .and_then(|v| v.get(name))
            .and_then(IvyJson::as_js_string);
        let decoded = value
            .filter(|v| {
                !v.0.is_empty()
                    && v.0
                        .iter()
                        .all(|c| matches!(*c,65..=90|97..=122|48..=57|45|95))
            })
            .and_then(|v| engine.decode(text(v)).ok());
        require(
            decoded.as_ref().is_some_and(|v| v.len() == length),
            "invalid_arguments",
            "Invalid push encryption keys.",
        )?;
        Ok((value.unwrap(), decoded.unwrap()))
    };
    let (auth, _) = key("auth", 16)?;
    let (p256dh, bytes) = key("p256dh", 65)?;
    crypto
        .validate_p256_public_key(&bytes)
        .map_err(|_| IvyError::new("invalid_arguments", "Invalid push encryption key."))?;
    Ok(IvyJson::object([
        ("endpoint", url.href.into()),
        (
            "keys",
            IvyJson::object([
                ("auth", IvyJson::String(auth.clone())),
                ("p256dh", IvyJson::String(p256dh.clone())),
            ]),
        ),
    ]))
}
pub struct BrowserPushStore {
    root: JsString,
    crypto: Rc<dyn Crypto>,
    clock: Rc<dyn Fn() -> f64>,
}
impl BrowserPushStore {
    pub fn new(
        store: &Store,
        base: &JsString,
        crypto: Rc<dyn Crypto>,
        clock: Rc<dyn Fn() -> f64>,
    ) -> IvyResult<Self> {
        let base = parse_url(base)?.href;
        let root = if base.ends_with('/') {
            base
        } else {
            base + "/"
        };
        sql(store,store.connection().execute_batch(r#"CREATE TABLE IF NOT EXISTS browser_push (
      endpoint TEXT PRIMARY KEY, session_digest TEXT NOT NULL REFERENCES browser_sessions(digest) ON DELETE CASCADE,
      subscription_json TEXT NOT NULL, last_error TEXT, created_sequence INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS browser_push_outbox (
      id INTEGER PRIMARY KEY AUTOINCREMENT, endpoint TEXT NOT NULL REFERENCES browser_push(endpoint) ON DELETE CASCADE,
      tag TEXT NOT NULL, notice_json TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
      next_attempt INTEGER NOT NULL DEFAULT 0, UNIQUE(endpoint,tag)
    ) STRICT;
    CREATE TABLE IF NOT EXISTS browser_push_receipts (tag TEXT PRIMARY KEY, created_at INTEGER NOT NULL) STRICT;"#))?;
        if store
            .metadata("browser_push_cursor")?
            .is_none_or(|v| v.is_empty())
        {
            store.set_metadata(
                "browser_push_cursor",
                &text(&js_string(&IvyJson::Number(head(store)?))),
            )?;
        }
        Ok(Self {
            root: root.into(),
            crypto,
            clock,
        })
    }
    pub fn keys(&self, store: &Store) -> IvyResult<IvyJson> {
        let mut value = store.metadata("browser_push_keys")?;
        if value.as_ref().is_none_or(|v| v.is_empty()) {
            let generated = encoded(&self.crypto.generate_vapid_keys()?)?;
            store.set_metadata("browser_push_keys", &generated)?;
            value = Some(generated);
        }
        decode(&value.unwrap())
    }
    pub fn request(
        &self,
        store: &mut Store,
        context: &IvyJson,
        request: &IvyJson,
    ) -> IvyResult<IvyJson> {
        store.authenticate(&AuthContext {
            credential_digest: string(context, "credentialDigest")?.clone(),
            session_digest: context
                .get("sessionDigest")
                .and_then(IvyJson::as_js_string)
                .cloned(),
        })?;
        require(
            truthy(context.get("sessionDigest")),
            "forbidden",
            "Push settings require a browser session.",
        )?;
        let session = text(string(context, "sessionDigest")?);
        if request.is_null() {
            return Err(IvyError::internal());
        }
        let action = request.get("action");
        if action == Some(&IvyJson::from("status")) {
            let row = if truthy(request.get("endpoint")) {
                sql(store,store.connection().query_row("SELECT last_error FROM browser_push WHERE endpoint=? AND session_digest=?",params![text(string(request,"endpoint")?),session],|r|r.get::<_,Option<String>>(0)).optional())?
            } else {
                None
            };
            let keys = self.keys(store)?;
            if keys.is_null() {
                return Err(IvyError::internal());
            }
            let mut value = IvyJson::object([
                ("subscribed", row.is_some().into()),
                (
                    "lastError",
                    row.flatten().map(IvyJson::from).unwrap_or(IvyJson::Null),
                ),
            ]);
            if let Some(key) = keys.get("publicKey") {
                value.insert("publicKey", key.clone());
            }
            return Ok(value);
        }
        if action == Some(&IvyJson::from("subscribe")) {
            let subscription = push_subscription(
                request.get("subscription").unwrap_or(&IvyJson::Null),
                self.crypto.as_ref(),
            )?;
            let endpoint = text(string(&subscription, "endpoint")?);
            let encoded = encoded(&subscription)?;
            let existing = sql(
                store,
                store
                    .connection()
                    .query_row(
                        "SELECT subscription_json FROM browser_push WHERE endpoint=?",
                        [&endpoint],
                        |r| r.get::<_, String>(0),
                    )
                    .optional(),
            )?;
            require(
                existing.as_ref().is_none_or(|v| v == &encoded),
                "forbidden",
                "This endpoint already has different encryption keys.",
            )?;
            require(
                existing.is_some()
                    || sql(
                        store,
                        store.connection().query_row(
                            "SELECT COUNT(*) AS n FROM browser_push",
                            [],
                            |r| r.get::<_, i64>(0),
                        ),
                    )? < 1024,
                "limit_exceeded",
                "Push subscription capacity reached.",
            )?;
            require(
                existing.is_some()
                    || sql(
                        store,
                        store.connection().query_row(
                            "SELECT COUNT(*) AS n FROM browser_push WHERE session_digest=?",
                            [&session],
                            |r| r.get::<_, i64>(0),
                        ),
                    )? < 16,
                "limit_exceeded",
                "Browser push subscription capacity reached.",
            )?;
            store.transaction(|store| {
                sql(
                    store,
                    store
                        .connection()
                        .execute("DELETE FROM browser_push WHERE endpoint=?", [&endpoint]),
                )?;
                sql(
                    store,
                    store.connection().execute(
                        "INSERT INTO browser_push VALUES (?,?,?,NULL,?)",
                        params![endpoint, session, encoded, head(store)?],
                    ),
                )?;
                Ok(())
            })?;
            return Ok(IvyJson::object([("subscribed", true.into())]));
        }
        let endpoint = request.get("endpoint").and_then(IvyJson::as_js_string);
        require(
            endpoint.is_some_and(|v| v.0.len() <= 4096),
            "invalid_arguments",
            "A push endpoint is required.",
        )?;
        let endpoint = text(endpoint.unwrap());
        if action == Some(&IvyJson::from("unsubscribe")) {
            sql(
                store,
                store.connection().execute(
                    "DELETE FROM browser_push WHERE endpoint=? AND session_digest=?",
                    params![endpoint, session],
                ),
            )?;
            return Ok(IvyJson::object([("subscribed", false.into())]));
        }
        require(
            action == Some(&IvyJson::from("test")),
            "invalid_arguments",
            "Unknown push action.",
        )?;
        let exists = sql(
            store,
            store
                .connection()
                .query_row(
                    "SELECT 1 FROM browser_push WHERE endpoint=? AND session_digest=?",
                    params![endpoint, session],
                    |r| r.get::<_, i64>(0),
                )
                .optional(),
        )?
        .is_some();
        require(exists, "not_found", "Enable notifications first.")?;
        let queued = self.enqueue(
            store,
            IvyJson::object([
                ("title", "Ivy".into()),
                ("body", "Browser notifications are working.".into()),
                ("tag", "ivy-test".into()),
                ("url", IvyJson::String(self.root.clone())),
            ]),
            Some(&endpoint),
            None,
        )?;
        require(
            queued,
            "capacity_exceeded",
            "Browser push capacity is occupied by pending deliveries.",
        )?;
        Ok(IvyJson::object([("queued", true.into())]))
    }
    fn enqueue(
        &self,
        store: &Store,
        mut notice: IvyJson,
        endpoint: Option<&str>,
        sequence: Option<f64>,
    ) -> IvyResult<bool> {
        let filtered = endpoint.filter(|v| !v.is_empty());
        let query = format!(
            "SELECT p.endpoint FROM browser_push p JOIN browser_sessions s ON s.digest=p.session_digest\n      JOIN credentials c ON c.digest=s.credential_digest WHERE c.revoked=0{}{}",
            if filtered.is_some() {
                " AND p.endpoint=?"
            } else {
                ""
            },
            if sequence.is_some() {
                " AND p.created_sequence<?"
            } else {
                ""
            }
        );
        let mut arguments = vec![];
        if let Some(endpoint) = filtered {
            arguments.push(rusqlite::types::Value::Text(endpoint.into()));
        }
        if let Some(sequence) = sequence {
            arguments.push(rusqlite::types::Value::Real(sequence));
        }
        let mut statement = sql(store, store.connection().prepare_cached(&query))?;
        let rows = sql(
            store,
            sql(
                store,
                statement.query_map(rusqlite::params_from_iter(arguments), |r| {
                    r.get::<_, String>(0)
                }),
            )?
            .collect::<rusqlite::Result<Vec<_>>>(),
        )?;
        let tag = text(string(&notice, "tag")?);
        let mut additions = 0;
        for endpoint in &rows {
            if sql(
                store,
                store
                    .connection()
                    .query_row(
                        "SELECT 1 FROM browser_push_outbox WHERE endpoint=? AND tag=?",
                        params![endpoint, tag],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional(),
            )?
            .is_none()
            {
                additions += 1;
            }
        }
        let queued = sql(
            store,
            store.connection().query_row(
                "SELECT COUNT(*) AS count FROM browser_push_outbox",
                [],
                |r| r.get::<_, i64>(0),
            ),
        )?;
        if queued + additions > 2048 {
            return Ok(false);
        }
        for (name, limit) in [("title", 100), ("body", 300)] {
            let value = string(&notice, name)?;
            notice.insert(
                name,
                IvyJson::String(JsString(value.0.iter().take(limit).copied().collect())),
            );
        }
        let encoded = encoded(&notice)?;
        for endpoint in rows {
            sql(store,store.connection().execute("INSERT OR IGNORE INTO browser_push_outbox(endpoint,tag,notice_json) VALUES (?,?,?)",params![endpoint,tag,encoded]))?;
        }
        Ok(true)
    }
    fn event_notice(
        &self,
        store: &Store,
        payload: &IvyJson,
        source: &str,
        sequence: f64,
    ) -> IvyResult<bool> {
        if store
            .validators()
            .validate(notice_schema(), payload, usize::MAX)
            .is_err()
        {
            return Ok(true);
        }
        let mut tag = JsString::from(source);
        tag.0.push(0);
        tag.0.extend_from_slice(&string(payload, "tag")?.0);
        let tag = digest(text(&tag));
        if sql(
            store,
            store
                .connection()
                .query_row(
                    "SELECT 1 FROM browser_push_receipts WHERE tag=?",
                    [&tag],
                    |r| r.get::<_, i64>(0),
                )
                .optional(),
        )?
        .is_some()
        {
            return Ok(true);
        }
        let target = field(payload, "target")?;
        let relative = format!(
            "ui/{}/{}",
            text(string(target, "uiId")?),
            text(string(target, "fragment")?)
        );
        let url = parse_url_with_base(&relative.into(), &self.root)?.href;
        let queued = self.enqueue(
            store,
            IvyJson::object([
                ("title", field(payload, "title")?.clone()),
                ("body", field(payload, "body")?.clone()),
                ("tag", tag.clone().into()),
                ("url", url.into()),
            ]),
            None,
            Some(sequence),
        )?;
        if !queued {
            return Ok(false);
        }
        sql(
            store,
            store.connection().execute(
                "INSERT INTO browser_push_receipts VALUES (?,?)",
                params![tag, (self.clock)()],
            ),
        )?;
        Ok(true)
    }
    pub fn next(&self, store: &mut Store) -> IvyResult<IvyJson> {
        let has_more=store.transaction(|store|{
            sql(store,store.connection().execute("DELETE FROM browser_push WHERE session_digest NOT IN\n        (SELECT s.digest FROM browser_sessions s JOIN credentials c ON c.digest=s.credential_digest WHERE c.revoked=0)",[]))?;
            let cursor=number_from_string(&store.metadata("browser_push_cursor")?.unwrap_or_default().into());
            let rows={let mut statement=sql(store,store.connection().prepare_cached("SELECT sequence,topic,topic_version,source,payload FROM events WHERE sequence>? ORDER BY sequence LIMIT 100"))?;
                sql(store,sql(store,statement.query_map([cursor],|r|Ok((r.get::<_,f64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?))))?.collect::<rusqlite::Result<Vec<_>>>())?};
            let subscribed=sql(store,store.connection().query_row("SELECT 1 FROM browser_push LIMIT 1",[],|r|r.get::<_,i64>(0)).optional())?.is_some();
            let mut through=cursor;let mut blocked=false;
            for (sequence,topic,version,source,payload) in &rows {
                if subscribed&&topic.ends_with(".browser-notification")&&version=="1.0.0"&&source.starts_with("service:")&&!self.event_notice(store,&decode(payload)?,source,*sequence)?{blocked=true;break;}
                through=*sequence;
            }
            if !rows.is_empty(){store.set_metadata("browser_push_cursor",&text(&js_string(&IvyJson::Number(through))))?;}
            sql(store,store.connection().execute("DELETE FROM browser_push_receipts WHERE created_at<?",[(self.clock)()-7.0*86_400_000.0]))?;
            sql(store,store.connection().execute("DELETE FROM browser_push_receipts WHERE tag IN (SELECT tag FROM browser_push_receipts ORDER BY created_at DESC LIMIT -1 OFFSET 10000)",[]))?;
            Ok(!blocked&&rows.len()==100)
        })?;
        let mut statement=sql(store,store.connection().prepare_cached("SELECT q.*,p.subscription_json FROM browser_push_outbox q JOIN browser_push p ON p.endpoint=q.endpoint\n      WHERE q.next_attempt<=? ORDER BY q.id LIMIT 4"))?;
        let rows = sql(
            store,
            sql(
                store,
                statement.query_map([(self.clock)()], |r| {
                    Ok((
                        r.get::<_, f64>(0)?,
                        r.get::<_, f64>(4)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(3)?,
                    ))
                }),
            )?
            .collect::<rusqlite::Result<Vec<_>>>(),
        )?;
        let deliveries = rows
            .into_iter()
            .map(|(id, attempts, subscription, notice)| {
                Ok(IvyJson::object([
                    ("id", IvyJson::Number(id)),
                    ("attempts", IvyJson::Number(attempts)),
                    ("subscription", decode(&subscription)?),
                    ("notice", decode(&notice)?),
                ]))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        Ok(IvyJson::object([
            ("deliveries", IvyJson::Array(deliveries)),
            ("hasMore", has_more.into()),
        ]))
    }
    pub fn complete(&self, store: &Store, id: f64, status: f64) -> IvyResult<()> {
        let row = sql(
            store,
            store
                .connection()
                .query_row(
                    "SELECT endpoint,attempts FROM browser_push_outbox WHERE id=?",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)),
                )
                .optional(),
        )?;
        let Some((endpoint, attempts)) = row else {
            return Ok(());
        };
        if status == 404.0 || status == 410.0 {
            sql(
                store,
                store
                    .connection()
                    .execute("DELETE FROM browser_push WHERE endpoint=?", [endpoint]),
            )?;
            return Ok(());
        }
        let ok = (200.0..300.0).contains(&status);
        let attempts = attempts + 1.0;
        sql(store,store.connection().execute("UPDATE browser_push SET last_error=? WHERE endpoint=?",params![if ok{None}else{Some("Push delivery failed. Try a test notification or enable notifications again.")},endpoint]))?;
        if ok || attempts >= 5.0 {
            sql(
                store,
                store
                    .connection()
                    .execute("DELETE FROM browser_push_outbox WHERE id=?", [id]),
            )?;
        } else {
            sql(
                store,
                store.connection().execute(
                    "UPDATE browser_push_outbox SET attempts=?,next_attempt=? WHERE id=?",
                    params![
                        attempts,
                        (self.clock)() + 30_000.0 * 2.0_f64.powf(attempts - 1.0),
                        id
                    ],
                ),
            )?;
        }
        Ok(())
    }
}
