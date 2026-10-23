//! One operator-selected offline MCP or Skills snapshot on stdin. Source owners
//! must be stopped before import; no service startup or provider is launched.
use ivy_agent_manager::{offline_import::OfflineImport, skills_files::SkillsComparator};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::{
    io::{Read, Write},
    sync::Arc,
};

fn invalid() -> IvyError {
    IvyError::new(
        "invalid_arguments",
        "Offline import requires one mcp or skills snapshot on stdin.",
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
            "Offline import snapshot exceeds the management byte limit.",
        ));
    }
    let request = parse(
        std::str::from_utf8(&bytes).map_err(|_| invalid())?,
        MANAGEMENT_FRAME_BYTES,
    )
    .map_err(|_| invalid())?;
    // Validate the selected snapshot before runtime loading or path mutations.
    let prepared = OfflineImport::from_snapshot(&request)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| IvyError::internal())?;
    match prepared {
        OfflineImport::Mcp(import) => runtime.block_on(import.run()),
        OfflineImport::Skills(import) => {
            let compare: Arc<SkillsComparator> = Arc::new(|left, right| {
                ivy_agent_manager::runtime::compare_names(left, right, false)
            });
            runtime.block_on(import.run(compare))
        }
    }
}
fn main() {
    let (code, value) = match run() {
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
