use ivy_chat_bridge::whatsapp::auth::{AuthEntry, AuthMutation, AuthStore};
use ivy_contracts::{
    IvyJson as Json,
    json::{canonical, parse},
};
use std::{fs, path::Path, process::Command};

fn phase(mode: &str, root: &Path) -> Json {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args(["tests/rust/fixtures/chat-auth-compat.mjs", mode])
        .arg(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}

fn rows(entries: &[AuthEntry]) -> Json {
    Json::Array(
        entries
            .iter()
            .map(|e| {
                Json::object([
                    ("category", e.category.as_str().into()),
                    ("id", e.id.as_str().into()),
                    ("value", e.value.as_str().into()),
                ])
            })
            .collect(),
    )
}

fn transfer(from: &Path, to: &Path) -> Vec<AuthEntry> {
    let mut original = AuthStore::open(from).unwrap();
    let first = original.snapshot().unwrap();
    assert!(
        canonical(&rows(&first), usize::MAX).unwrap()
            == canonical(&rows(&original.snapshot().unwrap()), usize::MAX).unwrap(),
        "Inspection leaves encoded state untouched"
    );
    original.close().unwrap();
    let mut copied = AuthStore::open(to).unwrap();
    copied
        .set_batch(
            &first
                .iter()
                .map(|e| AuthMutation {
                    category: &e.category,
                    id: &e.id,
                    value: Some(&e.value),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert!(
        canonical(&rows(&first), usize::MAX).unwrap()
            == canonical(&rows(&copied.snapshot().unwrap()), usize::MAX).unwrap(),
        "Exact encoded rows copied through Rust storage"
    );
    copied.close().unwrap();
    first
}

#[test]
fn synthetic_auth_snapshot_preserves_full_state_and_exposes_legacy_reopen_failures() {
    let root = std::env::temp_dir().join(format!("ivy-rust-auth-compat-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let seed = phase("seed", &root);
    let control = phase("group-control", &root);
    let archived = phase("archived-control", &root);
    let original = transfer(&root.join("original.sqlite"), &root.join("copied.sqlite"));
    let evolved = phase("advance", &root);
    let advanced = transfer(&root.join("copied.sqlite"), &root.join("rollback.sqlite"));
    let restored = phase("verify", &root);
    assert_ne!(
        seed.get("logicalHash"),
        evolved.get("logicalHash"),
        "Actual protocol and app-state mutations occur"
    );
    let capture = Json::object([
        ("synthetic",true.into()),
        ("seed",seed),
        ("legacyGroupControl",control),
        ("legacyArchivedControl",archived),
        ("evolved",evolved),
        ("restored",restored),
        ("originalRows",rows(&original)),
        ("advancedRows",rows(&advanced)),
        ("wire",fs::read_to_string(root.join("wire.json")).unwrap().into()),
        ("boundary","Original Baileys/libsignal crypto and auth codec, Rust snapshot/opaque storage; native Rust crypto mapping is not exercised.".into()),
    ]);
    if let Some(path) = std::env::var_os("IVY_CHAT_AUTH_COMPAT_CAPTURE") {
        fs::write(path, canonical(&capture, usize::MAX).unwrap()).unwrap();
    }
    assert_eq!(
        root.canonicalize().unwrap().parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(
        !fs::symlink_metadata(&root)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(root).unwrap();
}
