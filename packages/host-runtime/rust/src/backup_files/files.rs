use super::require;
use crate::{IvyError, IvyResult, node_path, paths, streaming_hash};
use ivy_contracts::JsString;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub use crate::artifact::ArtifactFile as BackupFile;
/// Called sequentially in UTF-16 path order, before observing each entry.
pub type BackupExclusion = Arc<dyn Fn(&JsString) -> IvyResult<bool> + Send + Sync>;
const MAXIMUM_BYTES: u64 = 64 * 1024 * 1024 * 1024;

fn internal<T, E>(result: Result<T, E>) -> IvyResult<T> {
    result.map_err(|_| IvyError::internal())
}
fn fs_path(path: &Path) -> IvyResult<PathBuf> {
    internal(node_path::node_filesystem_path(&node_path::from_native(
        path,
    )))
}

/// Resolve a safe slash-separated backup member using Node's lexical path policy.
pub fn relative_file(root: &JsString, member: &JsString) -> IvyResult<JsString> {
    relative_file_with_context(root, member, &internal(node_path::Context::current())?)
}
pub fn relative_file_with_context(
    root: &JsString,
    member: &JsString,
    context: &node_path::Context,
) -> IvyResult<JsString> {
    require(
        !node_path::is_absolute(context.style, member)
            && member.0.split(|c| *c == 47).all(|part| {
                !part.is_empty()
                    && part != [46]
                    && part != [46, 46]
                    && !part.iter().any(|c| *c < 32 || *c == 92 || *c == 58)
            }),
        "backup_invalid",
        "Backup members require safe relative file paths.",
    )?;
    let result = context.resolve(&[root.clone(), member.clone()]);
    require(
        context.inside(root, &result) && result != context.resolve(&[root.clone()]),
        "backup_invalid",
        "Backup member leaves its root.",
    )?;
    Ok(result)
}

/// Flush directories after their children. Windows deliberately performs no I/O.
pub async fn sync_directories(root: PathBuf) -> IvyResult<()> {
    #[cfg(windows)]
    {
        let _ = root;
        Ok(())
    }
    #[cfg(unix)]
    {
        fn visit(directory: &Path) -> IvyResult<()> {
            use std::os::unix::ffi::OsStrExt;
            let mut entries = internal(
                internal(fs::read_dir(fs_path(directory)?))?.collect::<io::Result<Vec<_>>>(),
            )?;
            // libuv scandir sorts native names before returning Dirents.
            entries.sort_by(|a, b| a.file_name().as_bytes().cmp(b.file_name().as_bytes()));
            for entry in entries {
                let kind = internal(entry.file_type())?;
                require(
                    !kind.is_symlink(),
                    "target_conflict",
                    "Durable backup publication cannot follow directory links.",
                )?;
                if kind.is_dir() {
                    let path = node_path::join(
                        node_path::Style::native(),
                        &[
                            node_path::from_native(directory),
                            node_path::node_utf8(&entry.file_name()),
                        ],
                    );
                    visit(&node_path::to_native(&path))?;
                }
            }
            sync_close(internal(File::open(fs_path(directory)?))?)
        }
        internal(tokio::task::spawn_blocking(move || visit(&root)).await)?
    }
}

/// Inventory regular files without following links, with the original depth,
/// count and total-byte bounds. A dropped observer does not cancel admitted I/O.
pub async fn inventory_files(
    root: PathBuf,
    exclude: Option<BackupExclusion>,
) -> IvyResult<Vec<BackupFile>> {
    internal(
        tokio::task::spawn_blocking(move || {
            let actual = internal(paths::realpath(&root))?;
            require(
                !observe(&root)?.is_symlink,
                "target_conflict",
                "Backup roots cannot be filesystem links.",
            )?;
            let context = internal(node_path::Context::current())?;
            let mut inventory = Inventory {
                root: node_path::from_native(&root),
                actual: node_path::from_native(&actual),
                context,
                exclude,
                entries: Vec::new(),
                total: 0,
            };
            inventory.visit(&actual, &JsString::default(), 0)?;
            inventory.entries.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(inventory.entries)
        })
        .await,
    )?
}
struct Inventory {
    root: JsString,
    actual: JsString,
    context: node_path::Context,
    exclude: Option<BackupExclusion>,
    entries: Vec<BackupFile>,
    total: u64,
}
impl Inventory {
    fn visit(&mut self, directory: &Path, prefix: &JsString, depth: usize) -> IvyResult<()> {
        require(
            depth <= 64,
            "limit_exceeded",
            "Backup directory nesting is too deep.",
        )?;
        let mut names = internal(
            internal(fs::read_dir(fs_path(directory)?))?
                .map(|entry| entry.map(|entry| node_path::node_utf8(&entry.file_name())))
                .collect::<io::Result<Vec<_>>>(),
        )?;
        names.sort();
        for name in names {
            let local = JsString(prefix.0.iter().chain(&name.0).copied().collect());
            if self
                .exclude
                .as_ref()
                .map(|exclude| exclude(&local))
                .transpose()?
                .unwrap_or(false)
            {
                continue;
            }
            let path = node_path::to_native(&relative_file_with_context(
                &self.root,
                &local,
                &self.context,
            )?);
            let metadata = observe(&path)?;
            // Preserve the short circuit: a link is rejected without realpath.
            require(
                !metadata.is_symlink
                    && self.context.inside(
                        &self.actual,
                        &node_path::from_native(&internal(paths::realpath(&path))?),
                    ),
                "target_conflict",
                "Backup cannot follow filesystem links.",
            )?;
            if metadata.is_directory {
                let mut prefix = local.clone();
                prefix.0.push(47);
                self.visit(&path, &prefix, depth + 1)?;
            } else {
                require(
                    metadata.is_file,
                    "backup_unsupported_entry",
                    "Backup contains an unsupported filesystem entry.",
                )?;
                self.total = self.total.saturating_add(metadata.bytes);
                require(
                    self.entries.len() < 50_000 && self.total <= MAXIMUM_BYTES,
                    "limit_exceeded",
                    "Private backup exceeds its bounded file/byte workload.",
                )?;
                self.entries.push(BackupFile {
                    path: local,
                    hash: streaming_hash::hash_file(&fs_path(&path)?)?,
                    bytes: metadata.bytes,
                    mode: metadata.mode,
                });
            }
        }
        Ok(())
    }
}

/// Bounded, flushed copy of an immutable observation; create exclusively and
/// retain a failed copy for its caller's diagnosis. Expected.path is not used.
pub async fn copy_verified(
    source: PathBuf,
    destination: PathBuf,
    expected: Option<BackupFile>,
    preserve_mode: bool,
) -> IvyResult<BackupFile> {
    internal(
        tokio::task::spawn_blocking(move || {
            let before = observe(&source)?;
            require(
                before.is_file && !before.is_symlink,
                "target_conflict",
                "Backup copies only regular files.",
            )?;
            require(
                before.bytes <= MAXIMUM_BYTES
                    && expected
                        .as_ref()
                        .is_none_or(|file| file.bytes == before.bytes),
                "backup_changed",
                "Backup source size changed.",
            )?;
            let context = internal(node_path::Context::current())?;
            let parent = node_path::dirname(context.style, &node_path::from_native(&destination));
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            internal(builder.create(internal(node_path::node_filesystem_path(&parent))?))?;
            // Both stream opens are admitted by pipeline before either open error
            // is observed. In particular a failed read can leave an empty target.
            let reader = File::open(fs_path(&source)?);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let writer = options.open(fs_path(&destination)?);
            let mut writer = match writer {
                Ok(writer) => writer,
                Err(_) => {
                    if let Ok(reader) = reader {
                        let _ = close_file(reader);
                    }
                    return Err(IvyError::internal());
                }
            };
            let mut hash = Sha256::new();
            let mut bytes = 0u64;
            let copied = match reader {
                Err(_) => Err(IvyError::internal()),
                Ok(mut reader) => {
                    let result = (|| {
                        let mut buffer = vec![0; 64 * 1024];
                        loop {
                            let count = internal(reader.read(&mut buffer))?;
                            if count == 0 {
                                break;
                            }
                            bytes += count as u64;
                            // The original Transform raises a plain Error here.
                            if bytes > before.bytes {
                                return Err(IvyError::internal());
                            }
                            hash.update(&buffer[..count]);
                            internal(writer.write_all(&buffer[..count]))?;
                        }
                        Ok(())
                    })();
                    let closed = internal(close_file(reader));
                    result.and(closed)
                }
            };
            // flush:true also syncs and closes the destination on stream failure.
            let flushed = sync_close(writer);
            copied.and(flushed)?;
            let after = observe(&source)?;
            let digest = format!(
                "sha256:{}",
                hash.finalize()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            require(
                bytes == before.bytes
                    && after.bytes == before.bytes
                    && after.mtime_ms == before.mtime_ms
                    && after.inode == before.inode
                    && expected.as_ref().is_none_or(|file| file.hash == digest),
                "backup_changed",
                "Backup source changed during its verified copy.",
            )?;
            let mode = expected
                .as_ref()
                .map(|file| file.mode)
                .unwrap_or(before.mode);
            #[cfg(unix)]
            if preserve_mode {
                use std::os::unix::fs::PermissionsExt;
                internal(fs::set_permissions(
                    fs_path(&destination)?,
                    fs::Permissions::from_mode(mode),
                ))?;
                sync_close(internal(File::open(fs_path(&destination)?))?)?;
            }
            #[cfg(windows)]
            let _ = preserve_mode;
            Ok(BackupFile {
                path: node_path::basename(context.style, &node_path::from_native(&destination)),
                hash: digest,
                bytes,
                mode,
            })
        })
        .await,
    )?
}

fn sync_close(file: File) -> IvyResult<()> {
    let synced = internal(file.sync_all());
    // The source's finally-close error wins over a preceding sync error.
    internal(close_file(file))?;
    synced
}
fn close_file(file: File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        if unsafe { libc::close(file.into_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        if unsafe { windows_sys::Win32::Foundation::CloseHandle(file.into_raw_handle()) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
struct Observation {
    is_file: bool,
    is_directory: bool,
    is_symlink: bool,
    bytes: u64,
    mode: u32,
    mtime_ms: f64,
    inode: f64,
}
fn observe(path: &Path) -> IvyResult<Observation> {
    let path = fs_path(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = internal(fs::symlink_metadata(path))?;
        Ok(Observation {
            is_file: metadata.is_file(),
            is_directory: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            bytes: metadata.len(),
            mode: metadata.mode() & 0o777,
            mtime_ms: metadata.mtime() as f64 * 1000.0 + metadata.mtime_nsec() as f64 / 1_000_000.0,
            inode: metadata.ino() as f64,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_READONLY, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GetFileInformationByHandle,
        };
        // Obtain type, identity and timestamps from one no-follow handle rather
        // than combining observations of two possibly different generations.
        let file = internal(
            OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(path),
        )?;
        let metadata = internal(file.metadata())?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(IvyError::internal());
        }
        let ticks = ((info.ftLastWriteTime.dwHighDateTime as u64) << 32)
            | info.ftLastWriteTime.dwLowDateTime as u64;
        Ok(Observation {
            is_file: metadata.is_file(),
            is_directory: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            bytes: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            mode: if info.dwFileAttributes & FILE_ATTRIBUTE_READONLY != 0 {
                0o444
            } else {
                0o666
            },
            mtime_ms: ((ticks / 10_000_000) as i64 - 11_644_473_600) as f64 * 1000.0
                + (ticks % 10_000_000) as f64 / 10_000.0,
            inode: (((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64) as f64,
        })
    }
}
