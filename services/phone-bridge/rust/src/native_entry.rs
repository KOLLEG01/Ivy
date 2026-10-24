//! Admission for the inherited-stdio native companion. Directory and lease
//! failures belong to runtime failure (1), after argument/platform usage (64).
use crate::{
    native_owner::NativeOwnerLease,
    native_pipe::{
        NativeRuntime, NativeShutdownStatus, NativeStdioError, NativeStopReason,
        run_owned_native_stdio_with,
    },
    native_rpc::guid_d,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const USAGE_EXIT_CODE: i32 = 64;
pub const RUNTIME_EXIT_CODE: i32 = 1;
pub const RUNTIME_FAILURE: &str = "phone_runtime_failed";

/// Compose the dedicated native child's existing owner lease and inherited pipe.
/// The factory supplies the real native runtime inside the owned Tokio context;
/// it must not launch a legacy backend. This process-wide entry is not for a
/// service host. The admitted lease remains held until process termination,
/// including factory failure, malformed input and unconfirmed shutdown.
pub fn run_owned_native_entry<E>(
    args: &[OsString],
    create_runtime: impl FnOnce() -> Result<Arc<dyn NativeRuntime>, E>,
) -> i32 {
    let arguments = match NativeEntryArguments::parse(args) {
        Ok(arguments) => arguments,
        Err(code) => return code,
    };
    // Construction failures carry no payload to stderr. Once construction has
    // succeeded, the pipe installs its established redacted task-panic hook.
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        NativeOwnerLease::acquire(arguments.owner_root(), arguments.epoch()).map_err(|_| ())?;
        let report = run_owned_native_stdio_with(arguments.epoch().into(), move || {
            create_runtime().map_err(|_| NativeStdioError::RuntimeUnavailable)
        })
        .map_err(|_| ())?;
        if report.shutdown != NativeShutdownStatus::Confirmed
            || matches!(
                report.reason,
                NativeStopReason::InvalidFrame
                    | NativeStopReason::InputFailed
                    | NativeStopReason::OwnerFailed
            )
        {
            return Err(());
        }
        Ok(())
    }));
    if matches!(outcome, Ok(Ok(()))) {
        0
    } else {
        eprintln!("{RUNTIME_FAILURE}");
        RUNTIME_EXIT_CODE
    }
}

#[cfg(all(test, windows))]
#[path = "../../../../tests/rust/phone_native_bootstrap.rs"]
mod bootstrap_tests;

#[derive(Debug, PartialEq, Eq)]
pub struct NativeEntryArguments {
    epoch: String,
    owner_root: PathBuf,
}

impl NativeEntryArguments {
    /// Supply arguments after argv[0]. No account credentials or synthetic media
    /// options are accepted. Preserve epoch spelling and the original OS path.
    pub fn parse(args: &[OsString]) -> Result<Self, i32> {
        Self::on_platform(
            args,
            cfg!(all(
                any(windows, target_os = "linux"),
                target_pointer_width = "64"
            )),
        )
    }

    fn on_platform(args: &[OsString], windows_64: bool) -> Result<Self, i32> {
        if !windows_64 || args.len() != 4 || args[0] != "--epoch" || args[2] != "--owner-root" {
            return Err(USAGE_EXIT_CODE);
        }
        let epoch = args[1]
            .to_str()
            .filter(|epoch| guid_d(epoch))
            .ok_or(USAGE_EXIT_CODE)?;
        Ok(Self {
            epoch: epoch.into(),
            owner_root: args[3].clone().into(),
        })
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }
    pub fn owner_root(&self) -> &Path {
        &self.owner_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_exact_and_does_not_admit_owner_paths_early() {
        let epoch = " \t00112233-4455-6677-8899-AABBCCDDEEFF\r\n";
        let valid: Vec<OsString> = ["--epoch", epoch, "--owner-root", "relative-or-absent"]
            .map(Into::into)
            .into();
        let args = NativeEntryArguments::on_platform(&valid, true).unwrap();
        assert_eq!(args.epoch(), epoch);
        assert_eq!(args.owner_root(), Path::new("relative-or-absent"));
        assert_eq!(NativeEntryArguments::on_platform(&valid, false), Err(64));
        for args in [
            vec![],
            valid[..3].to_vec(),
            [valid.clone(), vec!["extra".into()]].concat(),
        ] {
            assert_eq!(NativeEntryArguments::on_platform(&args, true), Err(64));
        }
        for (index, bad) in [
            (0, "--Epoch"),
            (1, "{00112233-4455-6677-8899-aabbccddeeff}"),
            (1, ""),
            (2, "--ownerRoot"),
        ] {
            let mut args = valid.clone();
            args[index] = bad.into();
            assert_eq!(NativeEntryArguments::on_platform(&args, true), Err(64));
        }
    }

    #[cfg(windows)]
    #[test]
    fn owner_path_keeps_os_code_units_but_epoch_must_be_text() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let root = OsString::from_wide(&[b'C' as u16, 58, 92, 0xd800]);
        let mut args = vec![
            "--epoch".into(),
            "00112233-4455-6677-8899-aabbccddeeff".into(),
            "--owner-root".into(),
            root.clone(),
        ];
        let parsed = NativeEntryArguments::parse(&args).unwrap();
        assert_eq!(
            parsed
                .owner_root()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>(),
            root.encode_wide().collect::<Vec<_>>()
        );
        args[1] = OsString::from_wide(&[0xd800]);
        assert_eq!(NativeEntryArguments::parse(&args), Err(64));
    }
}
