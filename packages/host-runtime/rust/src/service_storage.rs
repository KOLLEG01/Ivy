//! Read-only compatibility admission for Ivy-owned service journals.
use crate::{IvyError, IvyJson, IvyResult, layout, node_path};
use ivy_contracts::{JsString, json};
use ivy_store::{rusqlite, sqlite};
use std::time::Duration;

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(v) => *v,
        IvyJson::Number(v) => *v != 0.0 && !v.is_nan(),
        IvyJson::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn number(value: Option<&IvyJson>) -> IvyResult<f64> {
    Ok(match value {
        None => f64::NAN,
        Some(IvyJson::Null) => 0.0,
        Some(IvyJson::Bool(v)) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        Some(IvyJson::Number(v)) => *v,
        Some(v) => json::number_from_string(&json::try_js_string(v)?),
    })
}
fn busy(error: &rusqlite::Error) -> bool {
    matches!(error,rusqlite::Error::SqliteFailure(error,_) if matches!(error.extended_code&255,5|6))
}

/// Only an actual SQLITE_BUSY/LOCKED read may use the optional running format.
/// The executor supplies it only after fresh generation-bound ready evidence.
/// Opening, corruption and other errors never authorize that fallback.
pub async fn verify_service_storage(
    config: IvyJson,
    instance_id: JsString,
    plan: IvyJson,
    verified_running_format: Option<f64>,
) -> IvyResult<()> {
    internal(tokio::task::spawn_blocking(move||{
        if plan.is_null(){return Err(IvyError::internal())}
        let component=match plan.get("componentId"){Some(value)=>json::try_js_string(value)?,None=>"undefined".into()};
        let inherited=["constructor","__defineGetter__","__defineSetter__","hasOwnProperty","__lookupGetter__","__lookupSetter__","isPrototypeOf","propertyIsEnumerable","toString","valueOf","__proto__","toLocaleString"].iter().any(|name|component==JsString::from(*name));
        if component!=JsString::from("phone-bridge")&&!inherited{return Ok(())}
        let storage=plan.get("storage").filter(|value|truthy(value)).ok_or_else(||IvyError::new("incompatible_storage","Service journal requires an explicit storage contract."))?;
        let paths=layout::service_paths(&config,&IvyJson::String(instance_id))?;
        // The original static journal table is an ordinary JS object: inherited
        // names reach path.join with a non-string value after the same gates.
        if inherited{return Err(IvyError::internal())}
        let path=node_path::filesystem_path(&node_path::join(node_path::Style::native(),&[paths.required("data")?.as_js_string().cloned().ok_or_else(IvyError::internal)?,"phone-commands.sqlite".into()]));
        if !path.exists(){return Ok(())}
        let db=internal(sqlite::open(&path,sqlite::OpenOptions{read_only:true,busy_timeout:Duration::from_millis(100)}))?;
        let format=match db.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)){
            Ok(value)=>value as f64,
            Err(error) if busy(&error)&&verified_running_format.is_some()=>verified_running_format.unwrap(),
            Err(_)=>return Err(IvyError::internal()),
        };
        if format==0.0||(format>=number(storage.get("minReadableFormat"))?&&format<=number(storage.get("maxReadableFormat"))?&&storage.get("writeFormat").and_then(IvyJson::as_f64)==Some(format)){return Ok(())}
        let mut message=component.0;
        message.extend(JsString::from(" storage format ").0);message.extend(json::try_js_string(&IvyJson::Number(format))?.0);
        message.extend(JsString::from(" cannot be used by candidate format ").0);message.extend(match storage.get("writeFormat"){Some(value)=>json::try_js_string(value)?.0,None=>JsString::from("undefined").0});
        message.extend(JsString::from(". Update refused before activation; retained data requires an explicit development decision.").0);
        Err(IvyError::new_js("incompatible_storage".into(),JsString(message)))
    }).await)?
}
