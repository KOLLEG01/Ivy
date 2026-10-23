//! Host folder browsing is independent of the native agent filesystem tools.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    filesystem,
    node_path::{self, Style},
    paths,
};
use std::{cmp::Ordering, sync::Arc};

/// Display ordering supplied by the host runtime.
pub type DirectoryComparator = dyn Fn(&JsString, &JsString) -> Ordering + Send + Sync;

pub async fn list_directories(
    path: &JsString,
    compare: Arc<DirectoryComparator>,
) -> IvyResult<IvyJson> {
    if !node_path::is_absolute(Style::native(), path) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Choose an absolute directory on this host.",
        ));
    }
    let path = path.clone();
    tokio::task::spawn_blocking(move || {
        let read = || -> std::io::Result<(JsString, Vec<JsString>)> {
            let actual = paths::realpath(&node_path::to_native(&path))?;
            let mut names = Vec::new();
            for entry in std::fs::read_dir(&actual)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    let name = entry.file_name();
                    names.push((name.clone(), node_path::node_utf8(&name)));
                }
            }
            #[cfg(not(windows))]
            names.sort_by(|left, right| left.0.as_encoded_bytes().cmp(right.0.as_encoded_bytes()));
            Ok((
                node_path::from_native(&actual),
                names.into_iter().map(|(_, name)| name).collect(),
            ))
        };
        let (actual, mut names) = read().map_err(|cause| {
            let Some(code) = filesystem::node_error_code(&cause) else {
                return IvyError::internal();
            };
            let prefix = match code {
                "EACCES" | "EPERM" => "Access to this folder is denied",
                "ENOENT" => "This folder does not exist",
                "ENOTDIR" => "This path is not a folder",
                _ => return IvyError::internal(),
            };
            let mut message = JsString::from(prefix);
            message.0.extend(": ".encode_utf16());
            message.0.extend_from_slice(&path.0);
            message.0.push(46);
            IvyError::new_js(code.into(), message)
        })?;
        let parent = node_path::dirname(Style::native(), &actual);
        names.sort_by(|left, right| compare(left, right));
        let directories = names
            .into_iter()
            .map(|name| {
                IvyJson::object([
                    (
                        "path",
                        IvyJson::String(node_path::join(
                            Style::native(),
                            &[actual.clone(), name.clone()],
                        )),
                    ),
                    ("name", IvyJson::String(name)),
                ])
            })
            .collect();
        Ok(IvyJson::object([
            ("path", IvyJson::String(actual.clone())),
            (
                "parent",
                if parent == actual {
                    IvyJson::Null
                } else {
                    IvyJson::String(parent)
                },
            ),
            ("directories", IvyJson::Array(directories)),
        ]))
    })
    .await
    .map_err(|_| IvyError::internal())?
}
