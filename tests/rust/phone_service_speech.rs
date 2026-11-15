use super::*;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("ivy-phone-speech-test-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn wav_header() -> [u8; 44] {
    let mut bytes = [0; 44];
    bytes[..4].copy_from_slice(b"RIFF");
    bytes[8..12].copy_from_slice(b"WAVE");
    bytes
}

#[tokio::test]
async fn response_chunks_keep_exact_original_hash_bytes_and_exclusive_creation() {
    let directory = Directory::new();
    let mut bytes = wav_header().to_vec();
    bytes.extend((0..100_003).map(|index| (index * 19) as u8));
    let mut download = WavDownload::new(directory.0.clone(), "original")
        .await
        .unwrap();
    for chunk in bytes[..13].chunks(1).chain(bytes[13..].chunks(70001)) {
        download.push(chunk).await.unwrap();
    }
    let (path, hash) = download.finish().await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(hash, ivy_contracts::hash::digest(&bytes));
    assert!(
        WavDownload::new(directory.0.clone(), "original")
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "exclusive failure overwrote original"
    );
}

#[tokio::test]
async fn streaming_limit_accepts_exact_bound_and_rejects_an_extra_byte_without_committing() {
    let directory = Directory::new();
    let mut download = WavDownload::new(directory.0.clone(), "exact")
        .await
        .unwrap();
    let header = wav_header();
    download.push(&header).await.unwrap();
    let mut expected = Sha256::new();
    expected.update(header);
    let block = [137; 32768];
    let mut remaining = MAX_WAV_BYTES - header.len();
    while remaining > 0 {
        let count = remaining.min(block.len());
        download.push(&block[..count]).await.unwrap();
        expected.update(&block[..count]);
        remaining -= count;
    }
    let (path, hash) = download.finish().await.unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), MAX_WAV_BYTES as u64);
    assert_eq!(
        hash,
        ivy_contracts::hash::digest_string(expected.finalize().into())
    );
    let mut overflow = WavDownload::new(directory.0.clone(), "overflow")
        .await
        .unwrap();
    overflow.push(&header).await.unwrap();
    let mut remaining = MAX_WAV_BYTES - header.len();
    while remaining > 0 {
        let count = remaining.min(block.len());
        overflow.push(&block[..count]).await.unwrap();
        remaining -= count;
    }
    let path = overflow.path.clone();
    assert!(overflow.push(&[0]).await.is_err());
    drop(overflow);
    assert!(!path.exists());
}

#[tokio::test]
async fn invalid_truncated_and_abandoned_downloads_cannot_become_original_success_files() {
    let directory = Directory::new();
    let header = wav_header();
    let invalid = [0u8; 44];
    for (operation, bytes) in [("truncated", &header[..43]), ("invalid", &invalid[..])] {
        let mut download = WavDownload::new(directory.0.clone(), operation)
            .await
            .unwrap();
        download.push(bytes).await.unwrap();
        let path = download.path.clone();
        assert!(download.finish().await.is_err());
        assert!(!path.exists());
    }
    let mut download = WavDownload::new(directory.0.clone(), "abandoned")
        .await
        .unwrap();
    download.push(&wav_header()).await.unwrap();
    let path = download.path.clone();
    drop(download);
    assert!(!path.exists());
}
