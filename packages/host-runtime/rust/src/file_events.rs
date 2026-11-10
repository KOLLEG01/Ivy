//! Coalesced native filesystem hints. Durable state remains the authority.
//! Subscribe before reading it so a concurrent write cannot be lost before wait.
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

pub struct FileEvents {
    _watcher: Option<RecommendedWatcher>,
    _sender: watch::Sender<()>,
    receiver: watch::Receiver<()>,
    failed: Arc<AtomicBool>,
    paths: Vec<PathBuf>,
    retry: tokio::time::Instant,
}

// FSEvents reports canonical paths (not /var's symlink spelling). Missing files
// retain their suffix, and watching an existing ancestor covers later creation.
fn canonical(path: &Path) -> PathBuf {
    for parent in path.ancestors() {
        if let Ok(root) = std::fs::canonicalize(parent) {
            return root.join(path.strip_prefix(parent).unwrap());
        }
    }
    path.to_owned()
}

impl FileEvents {
    pub fn new(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let paths: Vec<_> = paths.into_iter().map(|p| canonical(&p)).collect();
        let (sender, receiver) = watch::channel(());
        let failed = Arc::new(AtomicBool::new(false));
        let signal = sender.clone();
        let error = failed.clone();
        let selected = paths.clone();
        let watcher = RecommendedWatcher::new(
            move |event: notify::Result<Event>| {
                let changed = match event {
                    Ok(event) => {
                        !event.kind.is_access()
                            && (event.need_rescan()
                                || event.paths.iter().any(|path| {
                                    let structural = matches!(
                                        event.kind,
                                        EventKind::Create(_)
                                            | EventKind::Remove(_)
                                            | EventKind::Modify(notify::event::ModifyKind::Name(_))
                                    );
                                    if matches!(
                                        event.kind,
                                        EventKind::Remove(_)
                                            | EventKind::Modify(notify::event::ModifyKind::Name(_))
                                    ) && selected
                                        .iter()
                                        .any(|wanted| path != wanted && wanted.starts_with(path))
                                    {
                                        error.store(true, Ordering::Release);
                                    }
                                    selected.iter().any(|wanted| {
                                        path == wanted || structural && wanted.starts_with(path)
                                    })
                                }))
                    }
                    Err(_) => {
                        error.store(true, Ordering::Release);
                        true
                    }
                };
                if changed {
                    signal.send_replace(());
                }
            },
            Config::default().with_follow_symlinks(false),
        )
        .ok()
        .and_then(|mut watcher| {
            if paths.is_empty() {
                return None;
            }
            let mut roots: Vec<(PathBuf, RecursiveMode)> = Vec::new();
            for path in &paths {
                let parent = path.parent()?;
                let mut root = parent;
                while !root.is_dir() {
                    root = root.parent()?;
                }
                let mode = if root == parent {
                    RecursiveMode::NonRecursive
                } else {
                    RecursiveMode::Recursive
                };
                if let Some((_, previous)) = roots.iter_mut().find(|(path, _)| path == root) {
                    if mode == RecursiveMode::Recursive {
                        *previous = mode;
                    }
                } else {
                    roots.push((root.to_path_buf(), mode));
                }
            }
            for (root, mode) in roots {
                watcher.watch(&root, mode).ok()?;
            }
            Some(watcher)
        });
        if watcher.is_none() {
            failed.store(true, Ordering::Release);
        }
        Self {
            _watcher: watcher,
            _sender: sender,
            receiver,
            failed,
            paths,
            retry: tokio::time::Instant::now() + Duration::from_secs(30),
        }
    }

    pub fn native(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
    }

    /// Recovery for dropped events, watch invalidation or filesystems without
    /// notifications. Timers never request a system wake or prevent sleep.
    pub fn fallback(&self) -> Duration {
        if self.native() {
            Duration::from_secs(30)
        } else {
            Duration::from_millis(500)
        }
    }

    pub async fn changed(&mut self) {
        if !self.native() {
            // Absolute deadline survives cancellation by a caller's fallback
            // timer; recovery must neither starve nor spin on backend errors.
            tokio::time::sleep_until(self.retry).await;
            *self = Self::new(self.paths.clone());
        } else {
            let _ = self.receiver.changed().await;
        }
    }
}
