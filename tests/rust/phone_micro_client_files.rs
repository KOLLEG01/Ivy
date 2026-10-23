use super::*;
use ivy_contracts::{
    IvyJson as J,
    json::{parse, stringify},
};
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs::File,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

fn obj<const N: usize>(v: [(&str, J); N]) -> J {
    J::object(v)
}
fn number(n: usize) -> J {
    J::Number(n as f64)
}
fn string(v: &J, key: &str) -> String {
    v.get(key).unwrap().as_str().unwrap()
}
fn flag(v: &J, key: &str) -> bool {
    v.get(key).and_then(J::as_bool).unwrap_or(false)
}
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(out, "{b:02X}").unwrap();
    }
    out
}
fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn hash(bytes: &[u8]) -> String {
    ivy_contracts::hash::digest(bytes)[7..].into()
}
fn trace(name: &str, value: &J) {
    if let Ok(root) = std::env::var("IVY_PHONE_MICRO_CLIENT_FILES_TRACE") {
        std::fs::write(
            std::path::Path::new(&root).join(name),
            stringify(value, 4 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
fn fixture(mode: &str, cases: &[J]) -> J {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-micro-client-files-cases.json");
    let data = parse(&std::fs::read_to_string(path).unwrap(), 4 * 1024 * 1024).unwrap();
    let row = data.get(mode).unwrap();
    assert_eq!(
        row.get("cases").unwrap(),
        &J::Array(cases.to_vec()),
        "{mode} input corpus"
    );
    row.get("expected").unwrap().clone()
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ivy-phone-micro-files-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        assert!(self.0.is_absolute());
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-phone-micro-files-")
        );
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn probe(root: &Path, names: &[String]) -> J {
    J::Array(
        names
            .iter()
            .map(|name| {
                let path = root.join(name);
                let read = OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .open(&path)
                    .is_ok();
                let write = OpenOptions::new()
                    .write(true)
                    .share_mode(7)
                    .open(&path)
                    .is_ok();
                let delete = OpenOptions::new()
                    .read(true)
                    .access_mode(0x10000)
                    .share_mode(7)
                    .open(&path)
                    .is_ok();
                obj([
                    ("name", name.as_str().into()),
                    ("read", read.into()),
                    ("write", write.into()),
                    ("delete", delete.into()),
                ])
            })
            .collect(),
    )
}
fn release() -> J {
    J::object(
        MicroClientPins::release()
            .unwrap()
            .0
            .into_iter()
            .map(|(k, v)| (k, v.map_or(J::Null, J::from))),
    )
}
fn file(name: &str, data: &[u8]) -> J {
    obj([("name", name.into()), ("bytes", hex(data).into())])
}
fn pin(name: &str, hash: Option<&str>) -> J {
    obj([
        ("name", name.into()),
        ("hash", hash.map_or(J::Null, J::from)),
    ])
}
fn row(
    name: &str,
    files: Vec<J>,
    pins: Vec<J>,
    cancelled: bool,
    blocked: Option<&str>,
    nul: bool,
) -> J {
    obj([
        ("name", name.into()),
        ("files", J::Array(files)),
        ("pins", J::Array(pins)),
        ("cancelled", cancelled.into()),
        ("blocked", blocked.map_or(J::Null, J::from)),
        ("nulDirectory", nul.into()),
    ])
}
fn cases() -> Vec<J> {
    let names = ["usbip.exe", "libusbip.dll", "resources.dll"];
    let payloads = [
        b"inert executable\0\xFF".to_vec(),
        b"inert sibling library".to_vec(),
        b"inert resources".to_vec(),
    ];
    let files = || {
        names
            .iter()
            .zip(&payloads)
            .map(|(name, data)| file(name, data))
            .collect::<Vec<_>>()
    };
    let hashes = payloads.iter().map(|b| hash(b)).collect::<Vec<_>>();
    let pins = || {
        names
            .iter()
            .zip(&hashes)
            .map(|(name, h)| pin(name, Some(h)))
            .collect::<Vec<_>>()
    };
    let mut rows = vec![row("all-files", files(), pins(), false, None, false)];
    let mut reversed = pins();
    reversed.reverse();
    rows.push(row("reverse-order", files(), reversed, false, None, false));
    for index in 0..3 {
        let mut missing = files();
        missing.remove(index);
        rows.push(row(
            &format!("missing-{index}"),
            missing,
            pins(),
            false,
            None,
            false,
        ));
        let mut wrong = pins();
        wrong[index] = pin(names[index], Some(&"0".repeat(64)));
        rows.push(row(
            &format!("wrong-hash-{index}"),
            files(),
            wrong,
            false,
            None,
            false,
        ));
    }
    for (label, value) in [
        ("null-hash", None),
        ("uppercase-hash", Some(hashes[0].to_uppercase())),
        ("prefixed-hash", Some(format!("sha256:{}", hashes[0]))),
        ("trailing-lf-hash", Some(format!("{}\n", hashes[0]))),
    ] {
        let mut p = pins();
        p[0] = pin(names[0], value.as_deref());
        rows.push(row(label, files(), p, false, None, false));
    }
    for (label, name) in [
        ("parent-name", "../usbip.exe"),
        ("slash-name", "sub/usbip.exe"),
        ("backslash-name", "sub\\usbip.exe"),
        ("drive-name", "C:usbip.exe"),
        ("drive-root", "C:"),
        ("numeric-colon-name", "1:file"),
        ("unicode-colon-name", "é:file"),
        ("empty-name", ""),
        ("dot-name", "."),
        ("parent-directory-name", ".."),
        ("nul-name", "bad\0name"),
    ] {
        let mut p = pins();
        p[2] = pin(name, Some(&hashes[2]));
        rows.push(row(label, files(), p, false, None, false));
    }
    rows.push(row("empty-pins", files(), vec![], false, None, false));
    rows.push(row(
        "empty-pins-cancelled",
        files(),
        vec![],
        true,
        None,
        false,
    ));
    rows.push(row(
        "cancelled-before-hash",
        files(),
        pins(),
        true,
        None,
        false,
    ));
    let mut missing = files();
    missing.remove(0);
    rows.push(row(
        "missing-precedes-cancellation",
        missing,
        pins(),
        true,
        None,
        false,
    ));
    let mut bad = pins();
    bad[0] = pin("../usbip.exe", Some(&hashes[0]));
    rows.push(row(
        "name-precedes-cancellation",
        files(),
        bad,
        true,
        None,
        false,
    ));
    rows.push(row(
        "sharing-failure-after-first-hash",
        files(),
        pins(),
        false,
        Some("libusbip.dll"),
        false,
    ));
    rows.push(row(
        "sharing-precedes-cancellation",
        files(),
        pins(),
        true,
        Some("usbip.exe"),
        false,
    ));
    rows.push(row("nul-directory", files(), pins(), false, None, true));
    for length in [0, 1, 8191, 8192, 8193, 65536, 65537] {
        let data = (0..length).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        rows.push(row(
            &format!("stream-{length}"),
            vec![file("usbip.exe", &data)],
            vec![pin("usbip.exe", Some(&hash(&data)))],
            false,
            None,
            false,
        ));
    }
    rows
}
fn error(error: MicroClientFileError) -> J {
    let (kind, message) = match error {
        MicroClientFileError::Changed => ("changed", Some("Reviewed USBip client files changed.")),
        MicroClientFileError::InvalidName => {
            ("argument", Some("Client pins require sibling files."))
        }
        MicroClientFileError::Argument(_) => ("argument", None),
        MicroClientFileError::Cancelled => ("cancelled", None),
        MicroClientFileError::Io(_) => ("io", None),
        MicroClientFileError::Bundle => ("unexpected", None),
    };
    obj([
        ("kind", kind.into()),
        ("message", message.map_or(J::Null, J::from)),
    ])
}
async fn matrix_row(row: &J) -> J {
    let root = Temp::new();
    let mut names = Vec::new();
    for f in row.get("files").unwrap().as_array().unwrap() {
        let name = string(f, "name");
        assert!(
            ["usbip.exe", "libusbip.dll", "resources.dll", "extra.bin"].contains(&name.as_str())
        );
        std::fs::write(root.0.join(&name), bytes(&string(f, "bytes"))).unwrap();
        names.push(name);
    }
    let pins = MicroClientPins(
        row.get("pins")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    string(p, "name"),
                    p.get("hash")
                        .filter(|v| !v.is_null())
                        .map(|v| v.as_str().unwrap()),
                )
            })
            .collect(),
    );
    let stop = CancellationToken::new();
    if flag(row, "cancelled") {
        stop.cancel();
    }
    let blocked: Option<File> = row.get("blocked").filter(|v| !v.is_null()).map(|b| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(root.0.join(b.as_str().unwrap()))
            .unwrap()
    });
    let directory = if flag(row, "nulDirectory") {
        let mut value = root.0.as_os_str().to_os_string();
        value.push("\0");
        PathBuf::from(value)
    } else {
        root.0.clone()
    };
    let (count, held, e) = match MicroClientFiles::acquire(&directory, &pins, &stop).await {
        Ok(mut lease) => {
            let count = number(lease.files.len());
            let held = probe(&root.0, &names);
            drop(blocked);
            lease.dispose();
            lease.dispose();
            assert!(lease.files.is_empty());
            (count, held, J::Null)
        }
        Err(e) => {
            drop(blocked);
            (J::Null, J::Null, error(e))
        }
    };
    obj([
        ("name", row.get("name").unwrap().clone()),
        ("error", e),
        ("count", count),
        ("held", held),
        ("after", probe(&root.0, &names)),
    ])
}
#[test]
fn stream_hashes_errors_cancellation_and_read_leases_follow_public_cases() {
    let rows = cases();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let values = runtime.block_on(async {
        let mut out = Vec::new();
        for row in &rows {
            out.push(matrix_row(row).await);
        }
        out
    });
    let actual = obj([("release", release()), ("result", J::Array(values))]);
    trace("matrix-rust.json", &actual);
    let expected = fixture("matrix", &rows);
    assert_eq!(actual, expected);
    trace(
        "matrix-comparisons.json",
        &obj([
            ("cases", number(rows.len())),
            ("rust", actual),
            ("expected", expected),
        ]),
    );
}
async fn coexisting() -> J {
    let root = Temp::new();
    let names = ["usbip.exe", "libusbip.dll", "resources.dll"]
        .map(str::to_owned)
        .to_vec();
    let pins = MicroClientPins(
        names
            .iter()
            .map(|name| {
                std::fs::write(root.0.join(name), name.as_bytes()).unwrap();
                (name.clone(), Some(hash(name.as_bytes())))
            })
            .collect(),
    );
    let mut first = MicroClientFiles::acquire(&root.0, &pins, &CancellationToken::new())
        .await
        .unwrap();
    let mut second = MicroClientFiles::acquire(&root.0, &pins, &CancellationToken::new())
        .await
        .unwrap();
    let both = probe(&root.0, &names);
    first.dispose();
    let one = probe(&root.0, &names);
    first.dispose();
    second.dispose();
    let none = probe(&root.0, &names);
    obj([
        ("both", both),
        ("one", one),
        ("none", none),
        ("firstCount", number(first.files.len())),
        ("secondCount", number(second.files.len())),
    ])
}
#[test]
fn coexisting_readers_keep_independent_leases_until_each_disposal() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let actual = obj([
        ("release", release()),
        ("result", runtime.block_on(coexisting())),
    ]);
    trace("coexisting-rust.json", &actual);
    let expected = fixture("coexisting", &[]);
    assert_eq!(actual, expected);
    trace(
        "coexisting-comparisons.json",
        &obj([("rust", actual), ("expected", expected)]),
    );
}
