//! The installed client's executable and both sibling DLLs share one read lease.
//! Their release pins are the same resource consumed by the existing setup path.
use sha2::{Digest, Sha256};
use std::{fs::OpenOptions, io, path::Path};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum MicroClientFileError {
    Changed,
    InvalidName,
    Argument(io::Error),
    Cancelled,
    Io(io::Error),
    Bundle,
}
impl std::fmt::Display for MicroClientFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Changed => "Reviewed USBip client files changed.",
            Self::InvalidName => "Client pins require sibling files.",
            Self::Cancelled => "Micro client file acquisition cancelled.",
            Self::Bundle => "Exact bundled USBip release pins required.",
            Self::Io(error) => return error.fmt(f),
            Self::Argument(error) => return error.fmt(f),
        })
    }
}
impl std::error::Error for MicroClientFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(error) | Self::Argument(error) = self {
            Some(error)
        } else {
            None
        }
    }
}
impl From<io::Error> for MicroClientFileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct MicroClientPins(Vec<(String, Option<String>)>);
impl MicroClientPins {
    #[cfg(test)]
    pub(super) fn fixture(rows: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(
            rows.into_iter()
                .map(|(name, hash)| (name, Some(hash)))
                .collect(),
        )
    }
    pub fn release() -> Result<Self, MicroClientFileError> {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../assets/micro-usbip-client.json"
        ))
        .map_err(|_| MicroClientFileError::Bundle)?;
        let pins = value.as_object().ok_or(MicroClientFileError::Bundle)?;
        if pins.len() != 3
            || !["usbip.exe", "libusbip.dll", "resources.dll"]
                .iter()
                .all(|name| pins.contains_key(*name))
        {
            return Err(MicroClientFileError::Bundle);
        }
        let pins = pins
            .iter()
            .map(|(name, value)| {
                let expected = if value.is_null() {
                    None
                } else {
                    Some(
                        value
                            .as_str()
                            .ok_or(MicroClientFileError::Bundle)?
                            .to_owned(),
                    )
                };
                Ok((name.clone(), expected))
            })
            .collect::<Result<_, MicroClientFileError>>()?;
        Ok(Self(pins))
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.as_deref())
    }
}
pub struct MicroClientFiles {
    files: Vec<tokio::fs::File>,
}
impl MicroClientFiles {
    pub async fn acquire(
        directory: &Path,
        pins: &MicroClientPins,
        stop: &CancellationToken,
    ) -> Result<Self, MicroClientFileError> {
        let mut lease = Self { files: Vec::new() };
        for (name, expected) in &pins.0 {
            // Path.GetFileName treats a leading drive designator separately;
            // the independent separator checks also reject rooted/relative paths.
            if name.contains('/')
                || name.contains('\\')
                || name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                    && name.as_bytes().get(1) == Some(&b':')
            {
                return Err(MicroClientFileError::InvalidName);
            }
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
            }
            // Preserve open-before-cancellation ordering. The owning lease is
            // dropped on every error, including a later sibling's failed hash.
            let file = options.open(directory.join(name)).map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidInput && error.raw_os_error().is_none() {
                    MicroClientFileError::Argument(error)
                } else {
                    MicroClientFileError::Io(error)
                }
            })?;
            lease.files.push(tokio::fs::File::from_std(file));
            let file = lease.files.last_mut().unwrap();
            let mut digest = Sha256::new();
            let mut buffer = [0; 8192];
            loop {
                if stop.is_cancelled() {
                    return Err(MicroClientFileError::Cancelled);
                }
                let count = file.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            let mut actual = String::with_capacity(64);
            for byte in digest.finalize() {
                use std::fmt::Write;
                write!(actual, "{byte:02x}").unwrap();
            }
            if expected.as_deref() != Some(actual.as_str()) {
                return Err(MicroClientFileError::Changed);
            }
        }
        Ok(lease)
    }
    pub fn dispose(&mut self) {
        self.files.clear();
    }
}

#[cfg(all(test, windows))]
#[path = "../../../../../tests/rust/phone_micro_client_files.rs"]
mod tests;
