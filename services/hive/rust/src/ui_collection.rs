//! UI collection records native filesystem failures at the storage owner's boundary.
use super::*;
use ivy_host_runtime::filesystem;

struct Collector<'a> {
    files: &'a UiFiles,
    store: Option<&'a Store>,
}
impl Collector<'_> {
    fn io<T>(&self, path: &Path, work: impl FnOnce(&Path) -> io::Result<T>) -> IvyResult<T> {
        work(path).map_err(|error| self.failure(error))
    }
    fn entries(&self, path: &Path) -> IvyResult<Vec<fs::DirEntry>> {
        let mut values = self.io(path, |path| {
            fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()
        })?;
        values.sort_by(|a, b| {
            text(&os_string(&a.file_name()))
                .as_bytes()
                .cmp(text(&os_string(&b.file_name())).as_bytes())
        });
        Ok(values)
    }
    fn kind(&self, entry: &fs::DirEntry) -> IvyResult<fs::FileType> {
        entry.file_type().map_err(|error| self.failure(error))
    }
    fn modified_ms(&self, path: &Path) -> IvyResult<f64> {
        let time = self.io(path, |path| fs::metadata(path))?;
        let time = time.modified().map_err(|error| self.failure(error))?;
        Ok(match time.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0,
            Err(e) => {
                -(e.duration().as_secs() as f64 * 1000.0
                    + e.duration().subsec_nanos() as f64 / 1_000_000.0)
            }
        })
    }
    fn failure(&self, error: io::Error) -> IvyError {
        if let Some(store) = self.store {
            store.observe_failure(|| Some(error.to_string().into()));
        }
        sql_error(error)
    }
    fn remove(&self, path: &Path) -> IvyResult<()> {
        filesystem::remove_force_sync(path).map_err(|error| self.failure(error))
    }
    fn collect(&self, ui: &JsString, retained: &HashSet<JsString>, now_ms: f64) -> IvyResult<()> {
        let directory = absolute(&self.files.root.join(component_id(ui)?)).map_err(sql_error)?;
        if !directory.exists() {
            return Ok(());
        }
        for entry in self.entries(&directory)? {
            let name = os_string(&entry.file_name());
            if !self.kind(&entry)?.is_dir() || retained.contains(&name) {
                continue;
            }
            release_id(&name)?;
            let target = absolute(&directory.join(entry.file_name())).map_err(sql_error)?;
            require(
                contained(&directory, &target),
                "storage_invalid",
                "UI release directory is invalid.",
            )?;
            if self.modified_ms(&target)? <= now_ms - DAY_MS {
                self.remove(&target)?;
            }
        }
        if retained.is_empty() && self.entries(&directory)?.is_empty() {
            self.io(&directory, |path| fs::remove_dir(path))?;
        }
        Ok(())
    }
    fn collect_all(
        &self,
        retained: &HashMap<JsString, HashSet<JsString>>,
        now_ms: &dyn Fn() -> f64,
    ) -> IvyResult<()> {
        if !self.files.root.exists() {
            return Ok(());
        }
        for entry in self.entries(&self.files.root)? {
            let name = os_string(&entry.file_name());
            if !self.kind(&entry)?.is_dir() || name.0.first() == Some(&(b'.' as u16)) {
                continue;
            }
            component_id(&name)?;
            self.collect(
                &name,
                retained.get(&name).unwrap_or(&HashSet::new()),
                now_ms(),
            )?;
        }
        self.collect_temporary(now_ms())
    }
    fn sweep(&self, directory: &Path, depth: usize, cutoff: f64) -> IvyResult<()> {
        if !directory.exists() {
            return Ok(());
        }
        for entry in self.entries(directory)? {
            if !self.kind(&entry)?.is_dir() {
                continue;
            }
            let target = absolute(&directory.join(entry.file_name())).map_err(sql_error)?;
            require(
                contained(directory, &target),
                "storage_invalid",
                "UI staging path is invalid.",
            )?;
            if depth > 0 {
                self.sweep(&target, depth - 1, cutoff)?;
                if self.entries(&target)?.is_empty() {
                    self.io(&target, |path| fs::remove_dir(path))?;
                }
            } else if self.modified_ms(&target)? < cutoff {
                self.remove(&target)?;
            }
        }
        Ok(())
    }
    fn collect_temporary(&self, now_ms: f64) -> IvyResult<()> {
        let cutoff = now_ms - DAY_MS;
        self.sweep(
            &absolute(&self.files.root.join(".incoming")).map_err(sql_error)?,
            0,
            cutoff,
        )?;
        self.sweep(
            &absolute(&self.files.root.join(".staging")).map_err(sql_error)?,
            1,
            cutoff,
        )
    }
}

impl UiFiles {
    pub fn collect(
        &self,
        ui: &JsString,
        retained: &HashSet<JsString>,
        now_ms: f64,
    ) -> IvyResult<()> {
        Collector {
            files: self,
            store: None,
        }
        .collect(ui, retained, now_ms)
    }
    pub fn collect_all(
        &self,
        retained: &HashMap<JsString, HashSet<JsString>>,
        now_ms: &dyn Fn() -> f64,
    ) -> IvyResult<()> {
        Collector {
            files: self,
            store: None,
        }
        .collect_all(retained, now_ms)
    }
    pub(crate) fn collect_all_for_store(
        &self,
        store: &Store,
        retained: &HashMap<JsString, HashSet<JsString>>,
        now_ms: &dyn Fn() -> f64,
    ) -> IvyResult<()> {
        store.observe_failure(|| None);
        Collector {
            files: self,
            store: Some(store),
        }
        .collect_all(retained, now_ms)
    }
    pub fn collect_temporary(&self, now_ms: f64) -> IvyResult<()> {
        Collector {
            files: self,
            store: None,
        }
        .collect_temporary(now_ms)
    }
}
