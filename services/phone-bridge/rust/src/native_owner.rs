//! Domain-owned native identity record. Replacement requires positive process
//! exit evidence as well as an exclusive, process-lifetime directory lease.
use crate::native_rpc::{guid_d, root_values};
use ivy_contracts::{
    IvyJson,
    json::{ParseOptions, parse_with_options},
};
use std::io;

const RECORD_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeOwner {
    epoch: String,
    pid: i32,
    creation_filetime: i64,
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Native owner record unavailable.",
    )
}

impl NativeOwner {
    /// Match JsonDocument's bounded UTF-8 stream admission, including its optional
    /// UTF-8 BOM. The two Int32 fields use their exact integer token lexemes.
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if !(2..=RECORD_BYTES).contains(&bytes.len()) {
            return Err(unavailable());
        }
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        let text = std::str::from_utf8(bytes).map_err(|_| unavailable())?;
        let value = parse_with_options(
            text,
            RECORD_BYTES,
            ParseOptions {
                reject_duplicate_keys: true,
                maximum_container_depth: Some(4),
            },
        )
        .map_err(|_| unavailable())?;
        let IvyJson::Object(fields) = &value else {
            return Err(unavailable());
        };
        let names = ["schemaVersion", "epoch", "pid", "creationFileTime"];
        if fields.len() != names.len() || names.iter().any(|name| value.get(name).is_none()) {
            return Err(unavailable());
        }
        let spans = root_values(text);
        let raw = |name: &str| {
            fields
                .iter()
                .position(|(key, _)| key == &name.into())
                .map(|index| spans[index])
        };
        if raw("schemaVersion") != Some("1") {
            return Err(unavailable());
        }
        let pid = raw("pid")
            .and_then(|raw| raw.parse::<i32>().ok())
            .filter(|pid| *pid > 0)
            .ok_or_else(unavailable)?;
        let string = |name| value.get(name).unwrap().as_str().map_err(|_| unavailable());
        let epoch = string("epoch")?;
        if !guid_d(&epoch) {
            return Err(unavailable());
        }
        let created = string("creationFileTime")?;
        let creation_filetime = created
            .parse::<i64>()
            .ok()
            .filter(|value| *value > 0 && value.to_string() == created)
            .ok_or_else(unavailable)?;
        Ok(Self {
            epoch,
            pid,
            creation_filetime,
        })
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }
    pub fn pid(&self) -> u32 {
        self.pid as u32
    }
    pub fn creation_filetime(&self) -> u64 {
        self.creation_filetime as u64
    }

    /// Errors and a live matching identity both refuse replacement. A released
    /// directory lock alone never proves that the recorded process has exited.
    #[cfg(windows)]
    pub fn ensure_predecessor_exited(&self) -> io::Result<()> {
        if ivy_host_runtime::platform::windows_process::process_exited(
            self.pid(),
            self.creation_filetime(),
        )? {
            Ok(())
        } else {
            Err(io::Error::other("Original native process has not exited."))
        }
    }

    #[cfg(test)]
    fn encode(&self) -> io::Result<Vec<u8>> {
        ivy_contracts::json::stringify(
            &IvyJson::Object(vec![
                ("schemaVersion".into(), IvyJson::Number(1.0)),
                ("epoch".into(), IvyJson::from(self.epoch.clone())),
                ("pid".into(), IvyJson::Number(f64::from(self.pid))),
                (
                    "creationFileTime".into(),
                    IvyJson::from(self.creation_filetime.to_string()),
                ),
            ]),
            RECORD_BYTES,
        )
        .map(String::into_bytes)
        .map_err(|_| unavailable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ivy_contracts::json::stringify;
    const RECORD: &str = r#"{"schemaVersion":1,"epoch":"00112233-4455-6677-8899-aabbccddeeff","pid":42,"creationFileTime":"9223372036854775807"}"#;

    #[cfg(windows)]
    #[test]
    fn predecessor_fixture() {
        if std::env::var_os("IVY_PHONE_PREDECESSOR_FIXTURE").is_some() {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn predecessor_record_requires_real_exit_or_identity_retirement() {
        use ivy_host_runtime::platform::windows_process::process_identity;
        use std::{
            os::windows::process::CommandExt,
            process::{Child, Command, Stdio},
            time::{Duration, Instant},
        };
        let current = process_identity(std::process::id()).unwrap().unwrap();
        let make = |pid: u32, time: u64| {
            let record = NativeOwner {
                epoch: "00112233-4455-6677-8899-aabbccddeeff".into(),
                pid: i32::try_from(pid).unwrap(),
                creation_filetime: i64::try_from(time).unwrap(),
            };
            NativeOwner::parse(&record.encode().unwrap()).unwrap()
        };
        assert!(
            make(current.pid, current.created_filetime)
                .ensure_predecessor_exited()
                .is_err()
        );
        assert!(
            make(current.pid, current.created_filetime ^ 1)
                .ensure_predecessor_exited()
                .is_ok()
        );
        assert!(make(i32::MAX as u32, 1).ensure_predecessor_exited().is_ok());
        assert_eq!(
            make(4, 1)
                .ensure_predecessor_exited()
                .unwrap_err()
                .raw_os_error(),
            Some(5)
        );
        struct Owned(Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Owned(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_owner::tests::predecessor_fixture",
                    "--nocapture",
                ])
                .env("IVY_PHONE_PREDECESSOR_FIXTURE", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
                .spawn()
                .unwrap(),
        );
        let identity = process_identity(child.0.id()).unwrap().unwrap();
        let record = make(identity.pid, identity.created_filetime);
        assert!(record.ensure_predecessor_exited().is_err());
        drop(child.0.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Owned predecessor fixture did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Keep the original Child handle: the exact signaled process still exists.
        assert!(record.ensure_predecessor_exited().is_ok());
    }

    #[test]
    fn record_keeps_exact_fields_integer_ranges_and_epoch_spelling() {
        let owner = NativeOwner::parse(RECORD.as_bytes()).unwrap();
        assert_eq!(
            (owner.pid(), owner.creation_filetime()),
            (42, i64::MAX as u64)
        );
        assert_eq!(NativeOwner::parse(&owner.encode().unwrap()).unwrap(), owner);
        let epoch = " \t00112233-4455-6677-8899-AABBCCDDEEFF\r\n";
        let mut owner = owner;
        owner.epoch = epoch.into();
        assert_eq!(
            NativeOwner::parse(&owner.encode().unwrap())
                .unwrap()
                .epoch(),
            epoch
        );
        let bom = [vec![0xef, 0xbb, 0xbf], RECORD.as_bytes().to_vec()].concat();
        assert_eq!(NativeOwner::parse(&bom).unwrap().pid(), 42);
        assert!(
            NativeOwner::parse(
                RECORD
                    .replace("schemaVersion", "schemaVer\\u0073ion")
                    .as_bytes()
            )
            .is_ok()
        );
        for record in [
            RECORD.replace("\"schemaVersion\":1", "\"schemaVersion\":1.0"),
            RECORD.replace("\"schemaVersion\":1", "\"schemaVersion\":1e0"),
            RECORD.replace(
                "\"schemaVersion\":1",
                "\"schemaVersion\":1,\"schemaVer\\u0073ion\":1",
            ),
            RECORD.replace("\"schemaVersion\":1", "\"schemaVersion\":1,\"extra\":null"),
            RECORD.replace("\"pid\":42", "\"pid\":2147483648"),
            RECORD.replace("\"pid\":42", "\"pid\":42.0"),
            RECORD.replace("\"pid\":42", "\"pid\":4.2e1"),
            RECORD.replace("\"pid\":42", "\"pid\":0"),
            RECORD.replace("\"pid\":42", "\"pid\":-1"),
            RECORD.replace("\"pid\":42", "\"pid\":\"42\""),
            RECORD.replace("9223372036854775807", "9223372036854775808"),
            RECORD.replace("9223372036854775807", "01"),
            RECORD.replace("9223372036854775807", "+1"),
            RECORD.replace("9223372036854775807", "0"),
            RECORD.replace("9223372036854775807", " 1"),
            RECORD.replace(
                "00112233-4455-6677-8899-aabbccddeeff",
                "00112233445566778899aabbccddeeff",
            ),
            format!("[{RECORD}]"),
            format!("{RECORD}{{}}"),
            format!("{} ", " ".repeat(4096)),
        ] {
            assert!(NativeOwner::parse(record.as_bytes()).is_err(), "{record}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn original_owner_stream_and_private_reader_agree_on_record_admission() {
        use ivy_contracts::json::parse;
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        let mut records = vec![
            RECORD.as_bytes().to_vec(),
            b"{}".to_vec(),
            b"null".to_vec(),
            vec![],
            vec![0xff, 0xff],
        ];
        for field in ["schemaVersion", "pid", "epoch", "creationFileTime"] {
            records.push(
                RECORD
                    .replace(
                        &format!("\"{field}\":"),
                        &format!("\"extra\":null,\"{field}\":"),
                    )
                    .into_bytes(),
            );
            records.push(
                RECORD
                    .replace(
                        &format!("\"{field}\":"),
                        &format!("\"{field}\":null,\"{field}\":"),
                    )
                    .into_bytes(),
            );
            records.push(
                RECORD
                    .replace(
                        &format!("\"{field}\":"),
                        &format!(
                            "\"{field}\":null,\"{}\\u{:04x}\":",
                            &field[..field.len() - 1],
                            field.as_bytes()[field.len() - 1]
                        ),
                    )
                    .into_bytes(),
            );
        }
        for number in [
            "1",
            "0",
            "-1",
            "1.0",
            "1e0",
            "1E+0",
            "2147483647",
            "2147483648",
            "-2147483649",
            "9007199254740993",
            "null",
            "true",
            "\"1\"",
            "[]",
            "{}",
        ] {
            for (field, before) in [("schemaVersion", "1"), ("pid", "42")] {
                records.push(
                    RECORD
                        .replace(
                            &format!("\"{field}\":{before}"),
                            &format!("\"{field}\":{number}"),
                        )
                        .into_bytes(),
                );
            }
        }
        for created in [
            "1",
            "0",
            "-1",
            "+1",
            "01",
            "1.0",
            "1e0",
            " 1",
            "1 ",
            "",
            "9223372036854775806",
            "9223372036854775807",
            "9223372036854775808",
            "18446744073709551615",
            "١",
            "１",
        ] {
            records.push(RECORD.replace("9223372036854775807", created).into_bytes());
        }
        for epoch in [
            "00112233-4455-6677-8899-AABBCCDDEEFF",
            " 00112233-4455-6677-8899-aabbccddeeff ",
            "\\t00112233-4455-6677-8899-aabbccddeeff\\r\\n",
            "\\u008500112233-4455-6677-8899-aabbccddeeff\\u00a0",
            "\\ud800",
            "00112233445566778899aabbccddeeff",
            "{00112233-4455-6677-8899-aabbccddeeff}",
            "",
        ] {
            records.push(
                RECORD
                    .replace("00112233-4455-6677-8899-aabbccddeeff", epoch)
                    .into_bytes(),
            );
        }
        records.extend([
            RECORD
                .replace("schemaVersion", "schemaVer\\u0073ion")
                .into_bytes(),
            format!(" \t{RECORD}\r\n").into_bytes(),
            format!("[{RECORD}]").into_bytes(),
            format!("{RECORD}{{}}").into_bytes(),
            RECORD
                .replace("\"pid\":42", "\"pid\":[[[[42]]]]")
                .into_bytes(),
            RECORD
                .replace("\"pid\":42", "\"pid\":[[[42]]]")
                .into_bytes(),
            [vec![0xef, 0xbb, 0xbf], RECORD.as_bytes().to_vec()].concat(),
            [
                vec![0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf],
                RECORD.as_bytes().to_vec(),
            ]
            .concat(),
            [
                vec![0xff, 0xfe],
                RECORD.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ]
            .concat(),
            format!("{RECORD}{}", " ".repeat(RECORD_BYTES - RECORD.len())).into_bytes(),
            format!("{RECORD}{}", " ".repeat(RECORD_BYTES + 1 - RECORD.len())).into_bytes(),
        ]);
        let input = stringify(
            &IvyJson::Array(
                records
                    .iter()
                    .map(|bytes| {
                        IvyJson::Array(
                            bytes
                                .iter()
                                .map(|byte| IvyJson::Number(f64::from(*byte)))
                                .collect(),
                        )
                    })
                    .collect(),
            ),
            1_000_000,
        )
        .unwrap();
        let mut child = Command::new(std::env::var_os("IVY_POWERSHELL").unwrap_or("pwsh".into()))
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .args([
                "-NoProfile",
                "-File",
                "tests/fixtures/phone-native-owner-oracle.ps1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let original = parse(std::str::from_utf8(&output.stdout).unwrap(), 1_000_000).unwrap();
        let original = original.as_array().unwrap();
        assert_eq!(records.len(), original.len());
        let comparisons: Vec<_> = records
            .iter()
            .zip(original)
            .enumerate()
            .map(|(index, (bytes, original))| {
                let native = NativeOwner::parse(bytes);
                assert_eq!(
                    native.is_ok(),
                    original.get("ok").unwrap().as_bool().unwrap(),
                    "record {index}: {}",
                    String::from_utf8_lossy(bytes)
                );
                if let Ok(owner) = native {
                    assert_eq!(
                        owner.epoch(),
                        original.get("epoch").unwrap().as_str().unwrap()
                    );
                    assert_eq!(
                        f64::from(owner.pid),
                        original.get("pid").unwrap().as_f64().unwrap()
                    );
                    assert_eq!(
                        owner.creation_filetime.to_string(),
                        original.get("creationFileTime").unwrap().as_str().unwrap()
                    );
                }
                IvyJson::Object(vec![
                    ("index".into(), IvyJson::Number(index as f64)),
                    ("original".into(), original.clone()),
                    (
                        "bytes".into(),
                        IvyJson::Array(
                            bytes
                                .iter()
                                .map(|byte| IvyJson::Number(f64::from(*byte)))
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        if let Some(path) = std::env::var_os("IVY_PHONE_NATIVE_OWNER_TRACE") {
            std::fs::write(
                path,
                stringify(&IvyJson::Array(comparisons), 1_000_000).unwrap(),
            )
            .unwrap();
        }
    }
}
