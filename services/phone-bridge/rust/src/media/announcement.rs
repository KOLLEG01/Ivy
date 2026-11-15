//! Verified WAV bytes and bounded PCM prefetch, outside the audio callback.
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Condvar, Mutex},
};
use tokio::sync::Notify;

pub(crate) const MAX_WAV_BYTES: usize = 10 * 1024 * 1024;
const BLOCK_BYTES: usize = 32768;
const PCM_BLOCK: usize = 960;
const PREFETCH_SAMPLES: usize = PCM_BLOCK * 6;
const INITIAL_SAMPLES: usize = PCM_BLOCK * 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavError {
    Invalid,
    Truncated,
    Layout,
    Duration,
    Path,
    Hash,
    Io,
}
impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WavError {}

#[derive(Clone, Copy)]
struct WaveLayout {
    offset: usize,
    rate: usize,
    channels: usize,
    frames: usize,
    samples: usize,
}
impl WaveLayout {
    fn parse(reader: &mut (impl Read + Seek), length: usize) -> Result<Self, WavError> {
        if length < 44 {
            return Err(WavError::Invalid);
        }
        let mut header = [0; 16];
        reader
            .read_exact(&mut header[..12])
            .map_err(|_| WavError::Io)?;
        if &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
            return Err(WavError::Invalid);
        }
        let mut rate = 0;
        let mut channels = 0;
        let mut data_offset = 0;
        let mut data_length = 0;
        let mut offset = 12;
        // Preserve the original last-fmt/last-data policy and streaming data
        // sentinel. The declared RIFF size is not an admission authority.
        while offset + 8 <= length {
            reader
                .seek(SeekFrom::Start(offset as u64))
                .map_err(|_| WavError::Io)?;
            reader
                .read_exact(&mut header[..8])
                .map_err(|_| WavError::Io)?;
            let data = &header[..4] == b"data";
            let format = &header[..4] == b"fmt ";
            let start = offset + 8;
            let mut size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            if data && size == u32::MAX as usize {
                size = length - start;
            }
            if size > length - start {
                return Err(WavError::Truncated);
            }
            if format {
                if size < 16 {
                    return Err(WavError::Layout);
                }
                reader.read_exact(&mut header).map_err(|_| WavError::Io)?;
                if u16::from_le_bytes(header[..2].try_into().unwrap()) != 1
                    || u16::from_le_bytes(header[14..16].try_into().unwrap()) != 16
                {
                    return Err(WavError::Layout);
                }
                channels = u16::from_le_bytes(header[2..4].try_into().unwrap()) as usize;
                rate = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            } else if data {
                data_offset = start;
                data_length = size;
            }
            offset = start + size + (size & 1);
        }
        if !(1..=8).contains(&channels)
            || !(8000..=192000).contains(&rate)
            || data_length == 0
            || data_length % (2 * channels) != 0
        {
            return Err(WavError::Layout);
        }
        let frames = data_length / (2 * channels);
        let samples = frames as u64 * 48000 / rate as u64;
        if samples == 0 || samples > 48000 * 120 {
            return Err(WavError::Duration);
        }
        Ok(Self {
            offset: data_offset,
            rate,
            channels,
            frames,
            samples: samples as usize,
        })
    }
    fn sample(
        &self,
        index: usize,
        mut frame: impl FnMut(usize) -> Result<[i16; 8], WavError>,
    ) -> Result<f32, WavError> {
        if index >= self.samples {
            return Ok(0.0);
        }
        let position = index as f64 * self.rate as f64 / 48000.0;
        let first = (position as usize).min(self.frames - 1);
        let second = (first + 1).min(self.frames - 1);
        let a = frame(first)?;
        let b = frame(second)?;
        let mut sum = 0.0;
        for channel in 0..self.channels {
            let a = f64::from(a[channel]);
            let b = f64::from(b[channel]);
            sum += a + (b - a) * (position - first as f64);
        }
        Ok((sum / self.channels as f64 / 32768.0) as f32)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Pcm16Wave<'a> {
    bytes: &'a [u8],
    layout: WaveLayout,
    pub samples: usize,
}
impl<'a> Pcm16Wave<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, WavError> {
        let layout = WaveLayout::parse(&mut Cursor::new(bytes), bytes.len())?;
        Ok(Self {
            bytes,
            layout,
            samples: layout.samples,
        })
    }
    pub fn sample(&self, index: usize) -> f32 {
        self.layout
            .sample(index, |frame| {
                let mut channels = [0; 8];
                let start = self.layout.offset + frame * self.layout.channels * 2;
                for (index, value) in channels.iter_mut().enumerate().take(self.layout.channels) {
                    let offset = start + index * 2;
                    *value = i16::from_le_bytes(self.bytes[offset..offset + 2].try_into().unwrap());
                }
                Ok(channels)
            })
            .unwrap()
    }
}
pub fn parse_announcement(bytes: &[u8]) -> Result<Vec<f32>, WavError> {
    let wave = Pcm16Wave::parse(bytes)?;
    Ok((0..wave.samples).map(|index| wave.sample(index)).collect())
}

struct ByteBlock([u8; BLOCK_BYTES]);
impl Drop for ByteBlock {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}
fn private_spool() -> Result<File, WavError> {
    let path = std::env::temp_dir().join(format!("ivy-phone-pcm-{}", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{DELETE, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE},
        };
        options
            .share_mode(0)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|_| WavError::Io)?;
    #[cfg(unix)]
    std::fs::remove_file(path).map_err(|_| WavError::Io)?;
    Ok(file)
}

/// Own the copied, verified bytes rather than rereading a mutable source path.
pub struct Announcement {
    file: File,
    layout: WaveLayout,
}
pub fn load_announcement(path: &Path, hash: &str) -> Result<Announcement, WavError> {
    if !path.is_absolute() {
        return Err(WavError::Path);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut source = options.open(path).map_err(|_| WavError::Io)?;
    let length = source.metadata().map_err(|_| WavError::Io)?.len();
    if !(44..=MAX_WAV_BYTES as u64).contains(&length) {
        return Err(WavError::Layout);
    }
    let mut file = private_spool()?;
    let mut block = ByteBlock([0; BLOCK_BYTES]);
    let mut digest = Sha256::new();
    let mut remaining = length as usize;
    while remaining > 0 {
        let count = remaining.min(BLOCK_BYTES);
        source
            .read_exact(&mut block.0[..count])
            .map_err(|_| WavError::Io)?;
        file.write_all(&block.0[..count])
            .map_err(|_| WavError::Io)?;
        digest.update(&block.0[..count]);
        remaining -= count;
    }
    if ivy_contracts::hash::digest_string(digest.finalize().into()) != hash {
        return Err(WavError::Hash);
    }
    file.rewind().map_err(|_| WavError::Io)?;
    let layout = WaveLayout::parse(&mut file, length as usize)?;
    Ok(Announcement { file, layout })
}

struct WaveReader {
    original: Announcement,
    bytes: ByteBlock,
    start: usize,
    frames: usize,
}
impl WaveReader {
    fn frame(&mut self, frame: usize) -> Result<[i16; 8], WavError> {
        let layout = self.original.layout;
        let stride = layout.channels * 2;
        if frame < self.start || frame >= self.start + self.frames {
            self.start = frame;
            self.frames = (BLOCK_BYTES / stride).min(layout.frames - frame);
            self.original
                .file
                .seek(SeekFrom::Start((layout.offset + frame * stride) as u64))
                .map_err(|_| WavError::Io)?;
            self.original
                .file
                .read_exact(&mut self.bytes.0[..self.frames * stride])
                .map_err(|_| WavError::Io)?;
        }
        let start = (frame - self.start) * stride;
        let mut channels = [0; 8];
        for (index, value) in channels.iter_mut().enumerate().take(layout.channels) {
            let offset = start + index * 2;
            *value = i16::from_le_bytes(self.bytes.0[offset..offset + 2].try_into().unwrap());
        }
        Ok(channels)
    }
}

struct Prefetch {
    samples: Box<[f32; PREFETCH_SAMPLES]>,
    read: usize,
    used: usize,
    closed: bool,
    complete: bool,
    failed: bool,
}
struct Shared {
    state: Mutex<Prefetch>,
    changed: Condvar,
    retired: Notify,
}
struct WorkerCompletion {
    shared: Arc<Shared>,
    failed: bool,
}
impl Drop for WorkerCompletion {
    fn drop(&mut self) {
        // The reader and its original file have already left prefetch, including
        // on unwinding. Only then may disposal confirm resource release.
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        state.failed = self.failed || std::thread::panicking();
        state.complete = true;
        self.shared.changed.notify_all();
        self.shared.retired.notify_waiters();
    }
}
pub(super) struct AnnouncementCleanup(Arc<Shared>);
impl AnnouncementCleanup {
    pub async fn wait(&self) {
        loop {
            let retired = self.0.retired.notified();
            tokio::pin!(retired);
            retired.as_mut().enable();
            if self
                .0
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .complete
            {
                return;
            }
            retired.await;
        }
    }
}
pub(super) struct AnnouncementPlayback {
    shared: Arc<Shared>,
    pub samples: usize,
}
impl AnnouncementPlayback {
    pub fn new(original: Announcement, repeats: usize) -> Result<Self, WavError> {
        if !(1..=4).contains(&repeats) {
            return Err(WavError::Layout);
        }
        let samples = original.layout.samples;
        let shared = Arc::new(Shared {
            state: Mutex::new(Prefetch {
                samples: Box::new([0.0; PREFETCH_SAMPLES]),
                read: 0,
                used: 0,
                closed: false,
                complete: false,
                failed: false,
            }),
            changed: Condvar::new(),
            retired: Notify::new(),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("Ivy announcement".into())
            .spawn(move || {
                let mut completion = WorkerCompletion {
                    shared: worker,
                    failed: true,
                };
                completion.failed = prefetch(original, repeats, &completion.shared).is_err();
            })
            .map_err(|_| WavError::Io)?;
        let playback = Self { shared, samples };
        let mut state = playback
            .shared
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let initial = INITIAL_SAMPLES.min(samples * repeats);
        while state.used < initial && !state.complete {
            state = playback
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|p| p.into_inner());
        }
        let failed = state.failed;
        drop(state);
        if failed {
            Err(WavError::Io)
        } else {
            Ok(playback)
        }
    }
    /// Only bounded memory copies; never wait for disk or a producer here.
    pub fn read(&self, output: &mut [f32]) -> usize {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        let count = output.len().min(state.used);
        for sample in output.iter_mut().take(count) {
            *sample = state.samples[state.read];
            let index = state.read;
            state.samples[index] = 0.0;
            state.read = (index + 1) % PREFETCH_SAMPLES;
        }
        state.used -= count;
        self.shared.changed.notify_all();
        count
    }
    pub fn failed(&self) -> bool {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .failed
    }
    pub fn cleanup(&self) -> AnnouncementCleanup {
        AnnouncementCleanup(self.shared.clone())
    }
}
impl Drop for AnnouncementPlayback {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed = true;
        state.samples.fill(0.0);
        self.shared.changed.notify_all();
    }
}
fn prefetch(original: Announcement, repeats: usize, shared: &Shared) -> Result<(), WavError> {
    let layout = original.layout;
    let mut reader = WaveReader {
        original,
        bytes: ByteBlock([0; BLOCK_BYTES]),
        start: 0,
        frames: 0,
    };
    let mut block = [0.0; PCM_BLOCK];
    for _ in 0..repeats {
        for start in (0..layout.samples).step_by(PCM_BLOCK) {
            let count = PCM_BLOCK.min(layout.samples - start);
            for (index, sample) in block.iter_mut().enumerate().take(count) {
                *sample = layout.sample(start + index, |frame| reader.frame(frame))?;
            }
            let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
            while PREFETCH_SAMPLES - state.used < count && !state.closed {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|p| p.into_inner());
            }
            if state.closed {
                return Ok(());
            }
            for sample in block.iter().take(count) {
                let index = (state.read + state.used) % PREFETCH_SAMPLES;
                state.samples[index] = *sample;
                state.used += 1;
            }
            shared.changed.notify_all();
        }
    }
    block.fill(0.0);
    Ok(())
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_announcement.rs"]
mod tests;
