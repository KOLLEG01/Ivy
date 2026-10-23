//! Transfer a completed staged payload without deleting its cross-volume copy.
use crate::{IvyError, IvyResult, filesystem, preparation_copy};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

fn cross_device(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(17)
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EXDEV)
    }
}
fn copy_payload(source: &Path, destination: &Path, directory: bool) -> io::Result<()> {
    if directory {
        preparation_copy::entry(source, destination, false, true, &|_| Ok(true))
    } else {
        // Follow both paths, but do not truncate a file through its own identity.
        let source_identity = filesystem::stat_identity(source)?;
        match filesystem::stat_identity(destination) {
            Ok(destination_identity)
                if source_identity.dev != "0"
                    && source_identity.ino != "0"
                    && source_identity == destination_identity =>
            {
                return Ok(());
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        fs::copy(source, destination)?;
        Ok(())
    }
}

/// Keep admitted rename/copy work alive after its caller stops waiting.
/// Only the native cross-device error selects copying; that path retains source.
pub async fn transfer_prepared_payload(
    source: PathBuf,
    destination: PathBuf,
    directory: bool,
) -> IvyResult<()> {
    tokio::task::spawn_blocking(
        move || match filesystem::rename_raw(&source, &destination) {
            Ok(()) => Ok(()),
            Err(error) if cross_device(&error) => copy_payload(&source, &destination, directory),
            Err(error) => Err(error),
        },
    )
    .await
    .map_err(|_| IvyError::internal())?
    .map_err(|_| IvyError::internal())
}

/// The actual EXDEV copy phase, exposed for preparation composition/qualification.
/// Calling it directly does not establish that a cross-device rename occurred.
pub async fn copy_prepared_payload(
    source: PathBuf,
    destination: PathBuf,
    directory: bool,
) -> IvyResult<()> {
    tokio::task::spawn_blocking(move || copy_payload(&source, &destination, directory))
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())
}
