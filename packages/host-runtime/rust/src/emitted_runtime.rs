//! Emitted JavaScript dependency inventory; syntax belongs to the supplied parser.
use crate::{IvyError, IvyResult, filesystem, node_path};
use ivy_contracts::JsString;
use std::{collections::BTreeSet, fs, path::PathBuf, sync::Arc};

/// Return literal module specifiers in the original depth-first visitor order:
/// import/export declarations, dynamic import and identifier require calls.
/// The production binding must preserve the accepted JavaScript parser semantics.
pub type JavaScriptImports =
    Arc<dyn Fn(&JsString, &JsString) -> IvyResult<Vec<JsString>> + Send + Sync>;

fn package(specifier: &JsString) -> Option<JsString> {
    if specifier.0.first().is_some_and(|c| matches!(*c, 46 | 47))
        || specifier.0.starts_with(&[110, 111, 100, 101, 58])
    {
        return None;
    }
    let mut parts = specifier.0.split(|c| *c == 47);
    let first = parts.next()?;
    let mut name = first.to_vec();
    if first.first() == Some(&64) {
        name.push(47);
        name.extend(parts.next().unwrap_or_default());
    }
    let text = String::from_utf16(&name).ok()?;
    let segment = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'~' | b'-' | b'.'))
            && s.as_bytes()[0] != b'.'
    };
    let accepted = if let Some(scoped) = text.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, name)| segment(scope) && segment(name))
    } else {
        segment(&text)
    };
    accepted.then_some(JsString(name))
}

/// Keep original breadth-first directory order and insertion-ordered package Set.
/// Symlink entries and non-JavaScript files are skipped, as with Node Dirent.
pub async fn emitted_runtime_packages(
    root: PathBuf,
    parse: JavaScriptImports,
) -> IvyResult<Vec<JsString>> {
    tokio::task::spawn_blocking(move || {
        let mut directories = vec![root];
        let mut index = 0;
        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        while index < directories.len() {
            let directory = directories[index].clone();
            index += 1;
            let directory = node_path::filesystem_path(&node_path::from_native(&directory));
            let entries = fs::read_dir(&directory)
                .map_err(|_| IvyError::internal())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| IvyError::internal())?;
            #[cfg(unix)]
            let entries = {
                use std::os::unix::ffi::OsStrExt;
                let mut entries = entries;
                entries.sort_by(|a, b| a.file_name().as_bytes().cmp(b.file_name().as_bytes()));
                entries
            };
            for entry in entries {
                let kind = entry.file_type().map_err(|_| IvyError::internal())?;
                let name = node_path::node_utf8(&entry.file_name());
                let path = directory.join(node_path::to_native(&name));
                if kind.is_dir() {
                    directories.push(path);
                    continue;
                }
                let units = &name.0;
                let script = [".js", ".cjs", ".mjs"]
                    .iter()
                    .any(|suffix| units.ends_with(&suffix.encode_utf16().collect::<Vec<_>>()));
                if !kind.is_file() || !script {
                    continue;
                }
                let bytes = filesystem::read_file_bytes(&path).map_err(|_| IvyError::internal())?;
                let source = JsString::from(String::from_utf8_lossy(&bytes).as_ref());
                for specifier in parse(&node_path::from_native(&path), &source)? {
                    if let Some(name) = package(&specifier)
                        && seen.insert(name.clone())
                    {
                        names.push(name);
                    }
                }
            }
        }
        Ok(names)
    })
    .await
    .map_err(|_| IvyError::internal())?
}
