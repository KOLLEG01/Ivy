//! Explicit offline import from a selected MCP/OpenAI JSON snapshot on stdin.
//! Stop the old owner first. No service startup, network or provider launch.
use ivy_agent_manager::offline_import::McpImport;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::io::{Read, Write};

fn invalid() -> IvyError {
    IvyError::new(
        "invalid_arguments",
        "MCP import requires home, hostId, hiveUrl, credential, document and openai on stdin.",
    )
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
    let prepared = McpImport::from_snapshot(&request)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| IvyError::internal())?;
    runtime.block_on(prepared.run())
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
