//! Pure storage paths, usable without host configuration admission or I/O.
use crate::{
    IvyError, IvyJson, IvyResult,
    node_path::{self, Style},
};
use ivy_contracts::JsString;
#[path = "service_paths.rs"]
mod service_paths;
#[path = "storage.rs"]
mod storage;
pub(crate) use service_paths::{joined, service_base, text};
pub use service_paths::{service_paths, service_paths_for_style};
pub use storage::{StorageArea, StorageContext, host_storage_areas, storage_file, storage_path};
