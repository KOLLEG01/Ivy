use super::*;
use std::time::{Duration, Instant};

struct Source(std::path::PathBuf);
impl Source {
    fn new(bytes: &[u8]) -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-phone-wave-test-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&((frames * usize::from(channels) * 2) as u32).to_le_bytes());
    for index in 0..frames * usize::from(channels) {
        bytes.extend_from_slice(&(index as i16).wrapping_mul(7919).to_le_bytes());
    }
    bytes
}
fn ready(playback: &AnnouncementPlayback, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut state = playback.shared.state.lock().unwrap();
    while state.used < count && !state.complete {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "prefetch did not make progress");
        state = playback
            .shared
            .changed
            .wait_timeout(state, remaining)
            .unwrap()
            .0;
    }
    assert!(!state.failed);
    assert!(state.used >= count);
    assert!(state.used <= PREFETCH_SAMPLES);
}

#[test]
fn streamed_pcm_keeps_verified_original_bytes_across_source_replacement_windows_and_repeats() {
    for (rate, channels) in [(8000, 1), (11025, 2), (44100, 8), (48000, 3), (192000, 8)] {
        // Cross the source window, partial output blocks and interpolation edges.
        let mut bytes = wav(rate, channels, 18019);
        if channels == 2 {
            bytes[40..44].fill(255); // Original streaming data sentinel.
            bytes.splice(12..12, *b"JUNK\x01\0\0\0\x09\0");
        }
        let expected = parse_announcement(&bytes).unwrap();
        let source = Source::new(&bytes);
        let original = load_announcement(&source.0, &ivy_contracts::hash::digest(&bytes)).unwrap();
        // The producer must use its verified copy, never hash then reopen the path.
        std::fs::write(&source.0, b"replaced and truncated after verification").unwrap();
        let playback = AnnouncementPlayback::new(original, 3).unwrap();
        assert_eq!(playback.samples, expected.len());
        let mut offset = 0;
        let mut output = [0.0; 997];
        let total = expected.len() * 3;
        while offset < total {
            let count = output.len().min(total - offset);
            ready(&playback, count);
            assert_eq!(playback.read(&mut output[..count]), count);
            for (index, sample) in output[..count].iter().enumerate() {
                assert_eq!(
                    sample.to_bits(),
                    expected[(offset + index) % expected.len()].to_bits(),
                    "rate={rate} channels={channels} sample={}",
                    offset + index
                );
            }
            offset += count;
        }
        assert_eq!(playback.read(&mut output), 0);
        assert!(!playback.failed());
    }
}

#[tokio::test]
async fn long_announcement_prefetch_stays_bounded_and_disposal_waits_for_its_original_producer() {
    let bytes = wav(8000, 1, 8000 * 120);
    let source = Source::new(&bytes);
    let original = load_announcement(&source.0, &ivy_contracts::hash::digest(&bytes)).unwrap();
    let playback = AnnouncementPlayback::new(original, 4).unwrap();
    assert_eq!(playback.samples, 48000 * 120);
    ready(&playback, PREFETCH_SAMPLES);
    let shared = Arc::downgrade(&playback.shared);
    let cleanup = playback.cleanup();
    {
        let state = playback.shared.state.lock().unwrap();
        assert_eq!(state.used, PREFETCH_SAMPLES);
        assert!(
            !state.complete,
            "producer should wait at its fixed buffer bound"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(1), cleanup.wait())
            .await
            .is_err()
    );
    drop(playback);
    tokio::time::timeout(Duration::from_secs(5), cleanup.wait())
        .await
        .unwrap();
    // Completion may precede observation; repeated waits still finish immediately.
    tokio::time::timeout(Duration::from_secs(1), cleanup.wait())
        .await
        .unwrap();
    drop(cleanup);
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.upgrade().is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        shared.upgrade().is_none(),
        "full producer retained its original spool after close"
    );
}

#[test]
fn verified_spool_rejects_changed_hash_size_layout_duration_and_failed_initial_read() {
    let bytes = wav(8000, 1, 8000);
    let hash = ivy_contracts::hash::digest(&bytes);
    let source = Source::new(&bytes);
    assert!(matches!(
        load_announcement(Path::new("relative.wav"), &hash),
        Err(WavError::Path)
    ));
    assert!(matches!(
        load_announcement(&source.0, "sha256:wrong"),
        Err(WavError::Hash)
    ));
    let original = load_announcement(&source.0, &hash).unwrap();
    original.file.set_len(44).unwrap();
    assert!(matches!(
        AnnouncementPlayback::new(original, 1),
        Err(WavError::Io)
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source.0)
        .unwrap()
        .set_len(MAX_WAV_BYTES as u64 + 1)
        .unwrap();
    assert!(matches!(
        load_announcement(&source.0, &hash),
        Err(WavError::Layout)
    ));
    for (bytes, expected) in [
        (wav(8000, 1, 8000 * 120 + 1), WavError::Duration),
        (wav(192000, 1, 1), WavError::Duration),
        (
            {
                let mut b = bytes.clone();
                b[34] = 8;
                b
            },
            WavError::Layout,
        ),
        (
            {
                let mut b = bytes.clone();
                b[40..44].copy_from_slice(&u32::MAX.wrapping_sub(1).to_le_bytes());
                b
            },
            WavError::Truncated,
        ),
    ] {
        let source = Source::new(&bytes);
        assert!(
            matches!(load_announcement(&source.0, &ivy_contracts::hash::digest(&bytes)), Err(error) if error == expected)
        );
    }
}
