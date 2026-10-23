use ivy_host_runtime::streaming_hash;
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-stream-hash-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn streaming_hash_preserves_empty_binary_large_and_non_executable_byte_identity() {
    let fixture = Fixture::new();
    for (name, bytes) in [
        ("empty.data", vec![]),
        ("cli ü 空 😀.js", b"opaque bytes\0\xff\xfe".to_vec()),
        (
            "large.payload",
            (0..4 * 1024 * 1024 + 17).map(|n| (n % 251) as u8).collect(),
        ),
    ] {
        let file = fixture.0.join(name);
        fs::write(&file, &bytes).unwrap();
        assert_eq!(
            streaming_hash::file_hash(&file).await.unwrap(),
            ivy_contracts::hash::digest(&bytes)
        );
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }
}

#[tokio::test]
async fn streaming_hash_refuses_missing_directory_and_invalid_paths_without_writes() {
    let fixture = Fixture::new();
    for file in [
        fixture.0.clone(),
        fixture.0.join("missing"),
        fixture.0.join("invalid\0path"),
    ] {
        assert_eq!(
            streaming_hash::file_hash(file).await.unwrap_err().code,
            "internal_error"
        );
    }
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[cfg(windows)]
#[tokio::test]
async fn streaming_hash_respects_exclusive_windows_sharing_and_recovers_after_release() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let file = fixture.0.join("normal-cli.data");
    let bytes = b"normal CLI hash has no executable suffix policy";
    fs::write(&file, bytes).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file)
        .unwrap();
    assert_eq!(
        streaming_hash::file_hash(&file).await.unwrap_err().code,
        "internal_error"
    );
    drop(held);
    assert_eq!(
        streaming_hash::file_hash(&file).await.unwrap(),
        ivy_contracts::hash::digest(bytes)
    );
    assert_eq!(fs::read(file).unwrap(), bytes);
}
