//! Select the real host files before a source build plan is hashed or retained.
use crate::{IvyError, IvyJson, IvyResult};
use std::path::{Path, PathBuf};

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}

/// Source plans name platform-neutral service/health binaries under dist/native.
/// Other executable and app plans retain their declared commands and requirements.
pub fn is_native_plan(plan: &IvyJson) -> bool {
    plan.get("kind") != Some(&"app".into())
        && plan
            .get("entrypoint")
            .and_then(|v| v.get("executable"))
            .and_then(IvyJson::as_js_string)
            .is_some_and(|value| {
                value
                    .0
                    .starts_with(&"dist/native/".encode_utf16().collect::<Vec<_>>())
            })
}
pub fn materialize_native_plan(mut plan: IvyJson, os: &str, arch: &str) -> IvyResult<IvyJson> {
    if !is_native_plan(&plan) {
        return Ok(plan);
    }
    if !["win32", "linux"].contains(&os) || !["x64", "arm64"].contains(&arch) {
        return Err(invalid(
            "Native preparation requires a supported host platform.",
        ));
    }
    let mut requirements = plan.required("requirements")?.clone();
    for (key, selected) in [("os", os), ("arch", arch)] {
        if let Some(advertised) = requirements.get(key) {
            if !advertised
                .as_array()
                .is_some_and(|values| values.contains(&selected.into()))
            {
                return Err(invalid(
                    "The native source plan does not support the selected host.",
                ));
            }
        }
        requirements.insert(key, IvyJson::Array(vec![selected.into()]));
    }
    let select = |command: &IvyJson, name: &str| -> IvyResult<IvyJson> {
        let executable = format!(
            "dist/native/{os}-{arch}/{name}{}",
            if os == "win32" { ".exe" } else { "" }
        );
        let source = format!("dist/native/{name}");
        if ![
            Some(&IvyJson::from(source)),
            Some(&IvyJson::from(executable.clone())),
        ]
        .contains(&command.get("executable"))
        {
            return Err(invalid(
                "Native plans must name their service and health binaries directly.",
            ));
        }
        let mut selected = command.clone();
        selected.insert("executable", executable.into());
        Ok(selected)
    };
    let name = format!("ivy-{}", plan.required("componentId")?.as_str()?);
    let entry = select(plan.required("entrypoint")?, &name)?;
    let mut readiness = plan.required("readiness")?.clone();
    readiness.insert(
        "command",
        select(readiness.required("command")?, "ivy-health")?,
    );
    plan.insert("requirements", requirements);
    plan.insert("entrypoint", entry);
    plan.insert("readiness", readiness);
    Ok(plan)
}

pub fn materialize_native_plan_for_host(plan: IvyJson) -> IvyResult<IvyJson> {
    materialize_native_plan(
        plan,
        if cfg!(windows) {
            "win32"
        } else {
            std::env::consts::OS
        },
        match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => other,
        },
    )
}

/// Existing artifacts keep their flat helper paths; prepared native artifacts
/// colocate helpers with the service under the selected platform directory.
pub fn native_binary_path(artifact: &Path, name: &str) -> PathBuf {
    let os = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let name = format!("{name}{}", if cfg!(windows) { ".exe" } else { "" });
    let root = artifact.join("dist/native");
    let selected = root.join(format!("{os}-{arch}")).join(&name);
    if selected.is_file() {
        selected
    } else {
        root.join(name)
    }
}
