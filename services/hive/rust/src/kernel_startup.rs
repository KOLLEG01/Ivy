//! Original Kernel constructor order, with runtime primitives supplied by its owner.
use super::*;
use crate::{hive_settings, package_kernel, store::Credential};
use ivy_host_runtime::native_path;
use std::{path::Path, sync::Arc};

pub struct StartupOptions {
    pub filename: JsString,
    pub ui_root: Option<JsString>,
    pub credentials: Vec<Credential>,
    pub kernel: Options,
}

/// No locale, ICU admission or Node os.tmpdir policy is substituted here.
/// The directory provider is evaluated only for a default in-memory UI root.
pub struct StartupPorts {
    pub runtime: Rc<dyn Runtime>,
    pub query: Arc<dyn crate::store::QueryRuntime>,
    pub clock: Rc<dyn Fn() -> f64>,
    pub time_zones: Rc<dyn hive_settings::TimeZones>,
    pub temporary_directory: Box<dyn FnOnce() -> IvyResult<JsString>>,
}

/// The worker keeps this error alive while it publishes Fatal, then exits.
pub struct StartupFailure {
    error: IvyError,
    _owner: Option<crate::store::StartupHold>,
}
impl StartupFailure {
    pub(crate) fn unowned(error: IvyError) -> Self {
        Self {
            error,
            _owner: None,
        }
    }
    /// Return the diagnostic, releasing this failure's startup-connection hold.
    /// Retain the typed failure through the worker diagnostic/Fatal/exit boundary.
    pub fn into_error(self) -> IvyError {
        self.error
    }
    pub(crate) fn retain(error: IvyError, store: &Store) -> Self {
        Self {
            error,
            _owner: Some(store.hold_startup_owner()),
        }
    }
}
impl std::borrow::Borrow<IvyError> for StartupFailure {
    fn borrow(&self) -> &IvyError {
        &self.error
    }
}
impl std::fmt::Debug for StartupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl Kernel {
    /// Open with a plain error; failure releases its startup-connection hold.
    /// Use `open_retained` to keep the connection alive with its typed failure.
    pub fn open(options: StartupOptions, ports: StartupPorts) -> IvyResult<Self> {
        Self::open_retained(options, ports).map_err(StartupFailure::into_error)
    }
    pub fn open_retained(
        options: StartupOptions,
        ports: StartupPorts,
    ) -> Result<Self, StartupFailure> {
        let mut owner = None;
        let result = (|| {
            let filename = native_path::to_native(&options.filename);
            let mut store = Store::open(&filename)?;
            owner = Some(store.hold_startup_owner());
            let temporary_ui_root = options.filename == JsString::from(":memory:")
                && options
                    .ui_root
                    .as_ref()
                    .is_none_or(|root| root.0.is_empty());
            let root = match options.ui_root {
                Some(root) => native_path::to_native(&root),
                None => {
                    let path = if temporary_ui_root {
                        native_path::to_native(&(ports.temporary_directory)()?)
                            .join(format!("ivy-ui-{}", uuid::Uuid::new_v4()))
                    } else {
                        filename
                            .parent()
                            .unwrap_or(if filename.has_root() {
                                &filename
                            } else {
                                Path::new(".")
                            })
                            .join("ui-releases")
                    };
                    native_path::lexical(&path)
                }
            };
            let ui_files = UiFiles::new(root);
            if let Err(error) = store.configure_credentials(&options.credentials) {
                // The explicit close can itself supersede the configuration error.
                drop(owner.take());
                store.close()?;
                return Err(error);
            }
            store.bind_query_runtime(ports.query);
            ui_files.migrate_legacy(&mut store)?;
            let registry = Rc::new(RefCell::new(Registry::new(
                &mut store,
                ports.runtime.as_ref(),
                (ports.clock)(),
            )?));
            let discovery = Discovery::new(ports.runtime.as_ref())?;
            let configurations = Configurations::new(&mut store)?;
            hive_settings::initialize(&mut store)?;
            let catalog = package_kernel::catalog(
                &mut store,
                options.kernel.build_id.clone(),
            )?;
            let mut kernel = Self::compose(
                Parts {
                    store,
                    registry,
                    discovery,
                    configurations,
                    ui_files,
                },
                options.kernel,
                ports.runtime,
                ports.clock,
            )?;
            kernel.catalog = Some(catalog);
            kernel.temporary_ui_root = temporary_ui_root;
            kernel.bind_settings_time_zones(ports.time_zones);
            Ok(kernel)
        })();
        result.map_err(|error| StartupFailure {
            error,
            _owner: owner,
        })
    }
}
