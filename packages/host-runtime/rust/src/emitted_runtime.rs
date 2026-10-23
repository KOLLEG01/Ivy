//! Intentional JavaScript builds use their installed compiler, not a Rust JS emulator.
use crate::{IvyError, IvyResult, node_runtime::NodeRuntime, process};
use ivy_contracts::{JsString, json};
use std::{collections::BTreeMap, path::Path};

const MAXIMUM_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const INVENTORY: &str = concat!(
    include_str!("../../src/emitted-runtime.ts"),
    r#"
import {createRequire} from 'node:module';
const sourceRoot = process.argv[1];
const typescript = createRequire(join(sourceRoot, 'package.json'))(join(sourceRoot, 'node_modules/typescript'));
process.stdout.write(JSON.stringify([...await emittedRuntimePackages(join(sourceRoot, 'dist'), typescript)]));
"#
);

/// Run once after the declared prepare commands have installed the source's tools.
/// The normal process owner supplies the Job/process group, deadline and output bound.
pub async fn emitted_runtime_packages(
    source_root: &Path,
    node: &NodeRuntime,
    mut context: process::LaunchContext,
) -> IvyResult<Vec<JsString>> {
    context.maximum_output_bytes = Some(MAXIMUM_OUTPUT_BYTES);
    let result = process::run_command(
        &process::Command {
            executable: "$node".into(),
            args: vec![
                "--input-type=module-typescript".into(),
                "--eval".into(),
                INVENTORY.into(),
                source_root.as_os_str().into(),
            ],
            cwd: None,
            timeout_ms: 300_000,
        },
        source_root,
        &BTreeMap::from([("$node".into(), node.executable.clone())]),
        context,
        None,
    )
    .await?;
    if result.truncated {
        return Err(IvyError::new(
            "limit_exceeded",
            "JavaScript runtime dependency inventory exceeds its output limit.",
        ));
    }
    let value = json::parse(&result.stdout, MAXIMUM_OUTPUT_BYTES)?;
    value
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|name| name.as_js_string().cloned().ok_or_else(IvyError::internal))
        .collect()
}
