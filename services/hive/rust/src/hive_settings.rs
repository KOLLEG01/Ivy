//! Hive-owned settings and OpenAI identity over immutable Object revisions.
use crate::{
    objects::{self, Location},
    store::{AuthenticatedContext, Store},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use rusqlite::OptionalExtension;
use std::sync::OnceLock;

/// Exact success/failure of Intl.DateTimeFormat('en', {timeZone}).format().
/// Startup must bind the canonical language provider; no zone list is assumed.
pub trait TimeZones {
    fn supported(&self, value: &JsString) -> bool;
}
impl<F: Fn(&JsString) -> bool> TimeZones for F {
    fn supported(&self, value: &JsString) -> bool {
        self(value)
    }
}
#[derive(Clone, Copy)]
pub enum Kind {
    Settings,
    Openai,
}
impl Kind {
    fn key(self) -> &'static str {
        match self {
            Self::Settings => "ivy/settings",
            Self::Openai => "ivy/openai-configuration",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Settings => "/ivy-settings",
            Self::Openai => "/ivy-openai-configuration",
        }
    }
    fn field(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Openai => "configuration",
        }
    }
    fn operation(self) -> &'static str {
        match self {
            Self::Settings => "settings.save",
            Self::Openai => "openai.saveConfiguration",
        }
    }
    fn conflict(self) -> &'static str {
        match self {
            Self::Settings => "Hive settings changed. Reload before saving.",
            Self::Openai => "OpenAI configuration changed. Reload before saving.",
        }
    }
    fn invalid_path(self) -> &'static str {
        match self {
            Self::Settings => "The Hive settings path contains a different or archived object.",
            Self::Openai => {
                "The OpenAI configuration path contains a different or archived object."
            }
        }
    }
    fn default(self) -> IvyJson {
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            (
                match self {
                    Self::Settings => "quietHours",
                    Self::Openai => "appId",
                },
                IvyJson::Null,
            ),
        ])
    }
}
fn field<'a>(v: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    v.get(key).ok_or_else(IvyError::internal)
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn contracts() -> &'static [IvyJson; 2] {
    static VALUE: OnceLock<[IvyJson; 2]> = OnceLock::new();
    VALUE.get_or_init(|| {
        let schemas = parse(include_str!("../../../../specs/schemas/hive-operations.schema.json"),usize::MAX).expect("embedded operation schemas");
        [
            (Kind::Openai, "OpenaiConfiguration", "Hive-wide OpenAI connection identity. The Console saves it and AgentManager disables this hosted app while managing direct MCP."),
            (Kind::Settings, "HiveSettings", "Hive-wide settings. Quiet hours govern automatic notifications and calls independently of optional services."),
        ].map(|(kind,name,spec)| IvyJson::object([
            ("key",kind.key().into()),("version","1.0.0".into()),("owner",IvyJson::object([("kind","hive".into())])),
            ("mediaType","application/json".into()),("retention",IvyJson::object([
                ("objects",IvyJson::object([("mode","retain".into())])),
                ("revisions",IvyJson::object([("mode","bounded".into()),("maximumCount",IvyJson::Number(100.0))])),
            ])),("specMarkdown",spec.into()),("jsonSchema",schemas.get("$defs").and_then(|v|v.get(name)).expect("embedded configuration schema").clone()),
        ]))
    })
}
/// Preserve original constructor order, after host configuration registration.
pub fn initialize(store: &mut Store) -> IvyResult<()> {
    for contract in contracts() {
        objects::register(store, contract, field(contract, "owner")?)?;
    }
    Ok(())
}
pub fn get(store: &Store, kind: Kind) -> IvyResult<IvyJson> {
    let id = store
        .connection()
        .query_row(
            "SELECT id FROM objects WHERE path=?",
            [kind.path()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| store.query_error(e))?;
    let Some(id) = id else {
        return Ok(IvyJson::object([
            ("revision", IvyJson::Number(0.0)),
            (kind.field(), kind.default()),
        ]));
    };
    let read = objects::read(store, Location::Id(&id.into()), None)?;
    let object = field(&read, "object")?;
    let content = field(&read, "content")?;
    require(
        object.get("contractKey") == Some(&kind.key().into())
            && content.get("encoding") == Some(&"json".into())
            && object.get("effectivelyArchived") == Some(&false.into()),
        "target_conflict",
        kind.invalid_path(),
    )?;
    Ok(IvyJson::object([
        (
            "revision",
            field(field(&read, "revision")?, "revision")?.clone(),
        ),
        (kind.field(), field(content, "value")?.clone()),
    ]))
}
pub fn save(
    store: &mut Store,
    context: &AuthenticatedContext,
    kind: Kind,
    params: &IvyJson,
    now_ms: u64,
    now: &JsString,
    time_zones: Option<&dyn TimeZones>,
) -> IvyResult<IvyJson> {
    let saved = objects::write_hive_prepared(
        store,
        context,
        &kind.operation().into(),
        params,
        now_ms,
        now,
        |store| {
            let value = field(params, kind.field())?;
            if matches!(kind, Kind::Settings) {
                if let Some(quiet) = value.get("quietHours").filter(|v| !v.is_null()) {
                    require(
                        quiet.get("start") != quiet.get("end"),
                        "invalid_arguments",
                        "Quiet hours need distinct start and end times.",
                    )?;
                    let zone = field(quiet, "timeZone")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?;
                    require(
                        time_zones.ok_or_else(IvyError::internal)?.supported(zone),
                        "invalid_arguments",
                        "Quiet hours need a supported IANA time zone.",
                    )?;
                }
            }
            let current = get(store, kind)?;
            let revision = field(&current, "revision")?;
            require(
                Some(revision) == params.get("expectedRevision"),
                "revision_conflict",
                kind.conflict(),
            )?;
            let mut write = IvyJson::object([
                ("mutationId", field(params, "mutationId")?.clone()),
                ("contractVersion", "1.0.0".into()),
                ("references", IvyJson::object([] as [(&str, IvyJson); 0])),
                (
                    "content",
                    IvyJson::object([("encoding", "json".into()), ("value", value.clone())]),
                ),
            ]);
            if revision.as_f64() != Some(0.0) {
                write.insert(
                    "objectId",
                    field(
                        &objects::stat(store, Location::Path(&kind.path().into()))?,
                        "id",
                    )?
                    .clone(),
                );
                write.insert("expectedRevision", revision.clone());
            } else {
                write.insert(
                    "create",
                    IvyJson::object([
                        ("contractKey", kind.key().into()),
                        ("parentId", IvyJson::Null),
                        ("ownerObjectId", IvyJson::Null),
                        ("name", kind.path()[1..].into()),
                    ]),
                );
            }
            Ok(write)
        },
    )?;
    Ok(IvyJson::object([
        (
            "revision",
            field(field(&saved, "revision")?, "revision")?.clone(),
        ),
        (kind.field(), field(params, kind.field())?.clone()),
    ]))
}
