//! Retention limits and completion markers. Collection has one owner in LocalHistory.
use crate::local_journal::{LocalJournal, text};
use ivy_contracts::{IvyResult, JsString};
use ivy_store::{rusqlite::params, sqlite::error};

pub struct HistoryLimits {
    pub count: u64,
    pub bytes: u64,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            count: 10_000,
            bytes: 256 * 1024 * 1024,
        }
    }
}

impl LocalJournal<'_> {
    /// Mark the original graph eligible only after its publication dependency closes.
    pub fn complete_workflow(
        &self,
        root: &JsString,
        completed_at: &JsString,
        dependency: Option<&JsString>,
    ) -> IvyResult<()> {
        if let Some(dependency) = dependency {
            self.connection
                .execute(
                    "INSERT OR IGNORE INTO cleanup_dependencies VALUES(?,?)",
                    params![text(root), text(dependency)],
                )
                .map_err(error)?;
        }
        self.connection.execute("INSERT INTO cleanup_roots VALUES(?,?) ON CONFLICT(root_id) DO UPDATE SET delete_after=excluded.delete_after",params![text(root),text(completed_at)]).map_err(error)?;
        Ok(())
    }
}
