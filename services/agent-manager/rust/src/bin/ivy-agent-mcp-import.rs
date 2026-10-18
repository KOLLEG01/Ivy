//! Explicit offline import from a selected MCP/OpenAI JSON snapshot on stdin.
//! Stop the old owner first. No service startup, network or provider launch.
use ivy_agent_manager::{mcp_import::import_mcp, mcp_prepare::prepare_mcp_import_snapshot};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::node_path::{self, Style};
use std::io::{Read, Write};

fn invalid() -> IvyError {
    IvyError::new(
        "invalid_arguments",
        "MCP import requires home, hostId, hiveUrl, credential, document and openai on stdin.",
    )
}
// These are the existing Host InstanceConfig field bounds; this standalone
// boundary receives the fields directly rather than loading that whole config.
fn string(request: &IvyJson, name: &str, maximum: usize) -> IvyResult<JsString> {
    request
        .get(name)
        .and_then(IvyJson::as_js_string)
        .filter(|value| {
            !value.0.is_empty() && String::from_utf16_lossy(&value.0).chars().count() <= maximum
        })
        .cloned()
        .ok_or_else(invalid)
}
fn run() -> IvyResult<IvyJson> {
    if std::env::args_os().len() != 1 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take((MANAGEMENT_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| IvyError::internal())?;
    if bytes.len() > MANAGEMENT_FRAME_BYTES {
        return Err(IvyError::new(
            "limit_exceeded",
            "MCP import snapshot exceeds the management byte limit.",
        ));
    }
    let request = parse(
        std::str::from_utf8(&bytes).map_err(|_| invalid())?,
        MANAGEMENT_FRAME_BYTES,
    )
    .map_err(|_| invalid())?;
    let fields = request.as_object().ok_or_else(invalid)?;
    if fields.len() != 6
        || fields.iter().any(|(key, _)| {
            ![
                "home",
                "hostId",
                "hiveUrl",
                "credential",
                "document",
                "openai",
            ]
            .iter()
            .any(|name| key == &JsString::from(*name))
        })
    {
        return Err(invalid());
    }
    let home = string(&request, "home", 2048)?;
    if !node_path::is_absolute(Style::native(), &home) {
        return Err(IvyError::new(
            "invalid_arguments",
            "MCP import home must be absolute.",
        ));
    }
    let prepared = prepare_mcp_import_snapshot(
        request.required("document")?,
        request.required("openai")?,
        &string(&request, "hostId", 128)?,
        &string(&request, "hiveUrl", 2048)?,
        &string(&request, "credential", 4096)?,
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| IvyError::internal())?;
    let imported = runtime.block_on(import_mcp(node_path::filesystem_path(&home), prepared))?;
    Ok(IvyJson::object([
        (
            "target",
            IvyJson::String(node_path::from_native(&imported.target)),
        ),
        (
            "statePath",
            IvyJson::String(node_path::from_native(&imported.state_path)),
        ),
        (
            "sourceBackup",
            imported
                .source_backup
                .as_ref()
                .map(|p| IvyJson::String(node_path::from_native(p)))
                .unwrap_or(IvyJson::Null),
        ),
        ("changed", imported.changed.into()),
    ]))
}
fn main() {
    let result = run();
    let (code, value) = match result {
        Ok(value) => (0, IvyJson::object([("result", value)])),
        Err(error) => (
            if error.code == "invalid_arguments" {
                2
            } else {
                1
            },
            IvyJson::object([("error", error.to_wire())]),
        ),
    };
    match stringify(&value, MANAGEMENT_FRAME_BYTES) {
        Ok(text) => {
            if writeln!(std::io::stdout().lock(), "{text}").is_err() {
                std::process::exit(1);
            }
        }
        Err(_) => std::process::exit(1),
    }
    std::process::exit(code);
}
