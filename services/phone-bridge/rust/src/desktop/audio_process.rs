//! Fresh pinned ancestry evidence. It does not identify an earlier audio stream.
use super::{ActionResult, DesktopActionError};
use crate::media::AudioDesktopIdentity;
use ivy_contracts::IvyJson;
use std::{
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
};

pub type ProcessImageEqual<'a> = dyn Fn(&Path, &Path) -> bool + 'a;
#[derive(Clone, Debug)]
pub struct AudioProcessRecord {
    pub pid: i32,
    pub parent_pid: i32,
    pub session_id: i32,
    pub start_ticks: i64,
    pub image_path: Option<PathBuf>,
}
impl AudioProcessRecord {
    pub fn same(&self, other: Option<&Self>, equal: &ProcessImageEqual<'_>) -> bool {
        other.is_some_and(|other| {
            self.pid == other.pid
                && self.parent_pid == other.parent_pid
                && self.session_id == other.session_id
                && self.start_ticks == other.start_ticks
                && match (&self.image_path, &other.image_path) {
                    (Some(a), Some(b)) => equal(a, b),
                    (None, None) => true,
                    _ => false,
                }
        })
    }
}
pub trait AudioProcessSnapshot {
    fn read(&mut self, pid: i32) -> ActionResult<Option<AudioProcessRecord>>;
    fn close(&mut self) -> ActionResult<()> {
        Ok(())
    }
}
impl<T: AudioProcessSnapshot + ?Sized> AudioProcessSnapshot for &mut T {
    fn read(&mut self, pid: i32) -> ActionResult<Option<AudioProcessRecord>> {
        (**self).read(pid)
    }
    fn close(&mut self) -> ActionResult<()> {
        (**self).close()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioProcessState {
    Owned,
    Foreign,
    Unavailable,
}
impl AudioProcessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Foreign => "foreign",
            Self::Unavailable => "unavailable",
        }
    }
}
#[derive(Clone, Debug)]
pub struct AudioProcessOwnership {
    pub pid: i32,
    pub state: AudioProcessState,
}
pub(super) fn guarded<T>(f: impl FnOnce() -> ActionResult<T>) -> ActionResult<T> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| DesktopActionError::Unavailable)?
}
impl AudioProcessOwnership {
    pub fn read<S: AudioProcessSnapshot>(
        root: &AudioDesktopIdentity,
        pid: i32,
        current: &dyn Fn() -> ActionResult<bool>,
        open: impl FnOnce() -> ActionResult<Option<S>>,
        equal: &ProcessImageEqual<'_>,
    ) -> ActionResult<Self> {
        if pid <= 0 {
            return Err(DesktopActionError::Argument(
                "Pinned Desktop and positive audio PID required.",
            ));
        }
        let state = guarded(|| {
            if !current()? {
                return Err(DesktopActionError::Unavailable);
            }
            let mut snapshot = open()?.ok_or(DesktopActionError::Unavailable)?;
            let result = guarded(|| {
                let pinned = snapshot
                    .read(root.pid())?
                    .ok_or(DesktopActionError::Unavailable)?;
                if pinned.pid != root.pid()
                    || pinned.start_ticks != root.creation_ticks()
                    || pinned
                        .image_path
                        .as_deref()
                        .is_none_or(|p| !equal(p, root.image()))
                {
                    return Err(DesktopActionError::Unavailable);
                }
                let mut chain: Vec<AudioProcessRecord> = vec![];
                let mut seen = HashSet::new();
                let mut next = pid;
                let mut state = AudioProcessState::Unavailable;
                while chain.len() < 64 {
                    if !seen.insert(next) {
                        return Err(DesktopActionError::Unavailable);
                    }
                    let process = snapshot
                        .read(next)?
                        .ok_or(DesktopActionError::Unavailable)?;
                    if process.pid != next
                        || process.start_ticks <= 0
                        || process
                            .image_path
                            .as_ref()
                            .is_none_or(|p| p.to_string_lossy().chars().all(char::is_whitespace))
                    {
                        return Err(DesktopActionError::Unavailable);
                    }
                    if chain
                        .last()
                        .is_some_and(|child| process.start_ticks > child.start_ticks)
                    {
                        return Err(DesktopActionError::Unavailable);
                    }
                    chain.push(process);
                    let process = chain.last().unwrap();
                    if process.session_id != pinned.session_id
                        || !equal(
                            process.image_path.as_deref().unwrap(),
                            pinned.image_path.as_deref().unwrap(),
                        )
                    {
                        state = AudioProcessState::Foreign;
                        break;
                    }
                    if process.pid == root.pid() {
                        if !pinned.same(Some(process), equal) {
                            return Err(DesktopActionError::Unavailable);
                        }
                        state = AudioProcessState::Owned;
                        break;
                    }
                    if process.parent_pid <= 0 {
                        state = AudioProcessState::Foreign;
                        break;
                    }
                    next = process.parent_pid;
                }
                // Even exhaustion renews every reached record and the root.
                for process in &chain {
                    if !process.same(snapshot.read(process.pid)?.as_ref(), equal) {
                        return Err(DesktopActionError::Unavailable);
                    }
                }
                if !pinned.same(snapshot.read(root.pid())?.as_ref(), equal) || !current()? {
                    return Err(DesktopActionError::Unavailable);
                }
                Ok(state)
            });
            guarded(|| snapshot.close())?;
            result
        })
        .unwrap_or(AudioProcessState::Unavailable);
        Ok(Self { pid, state })
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("pid", IvyJson::Number(self.pid.into())),
            ("state", self.state.as_str().into()),
        ])
    }
}
