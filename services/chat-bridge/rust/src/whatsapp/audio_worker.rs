//! Bounded Ogg/Opus decoding in a killable copy of the service executable.
use super::{audio_processing, messages::Translator};
use ivy_contracts::{IvyError, IvyResult};
use ogg::reading::PacketReader;
use std::{
    io::{Cursor, Read, Write},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const WORKER_ARGUMENT: &str = "--whatsapp-audio-worker";
const MAX_INPUT: usize = 32 * 1024 * 1024;
const MAX_FRAMES: usize = 600 * 16_000;
const MAX_WAV: usize = MAX_FRAMES * 2 + 44;

#[derive(Clone)]
pub struct AudioWorker {
    executable: PathBuf,
    timeout: Duration,
}
impl AudioWorker {
    pub fn new(executable: PathBuf) -> Self {
        Self::with_timeout(executable, Duration::from_secs(30))
    }
    pub fn with_timeout(executable: PathBuf, timeout: Duration) -> Self {
        Self {
            executable,
            timeout,
        }
    }
    pub async fn decode(&self, bytes: &[u8], translator: Translator) -> IvyResult<Vec<u8>> {
        let error = |code: &str, key| -> IvyError {
            IvyError::new_js(
                code.into(),
                translator
                    .translate(key, None)
                    .unwrap_or_else(|_| "The voice message could not be decoded.".into()),
            )
        };
        if bytes.len() > MAX_INPUT {
            return Err(error("whatsapp_audio_invalid", "audio.too_long"));
        }
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg(WORKER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command
            .spawn()
            .map_err(|_| error("whatsapp_audio_invalid", "audio.invalid"))?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap().take((MAX_WAV + 1) as u64);
        let result = tokio::time::timeout(self.timeout, async {
            let write = async move {
                input.write_all(bytes).await?;
                input.shutdown().await?;
                drop(input);
                Ok::<_, std::io::Error>(())
            };
            let read = async {
                let mut wav = Vec::new();
                output.read_to_end(&mut wav).await?;
                Ok::<_, std::io::Error>(wav)
            };
            let (_, wav) = tokio::try_join!(write, read)?;
            if wav.len() > MAX_WAV {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            Ok::<_, std::io::Error>((child.wait().await?, wav))
        })
        .await;
        match result {
            Ok(Ok((status, wav))) if status.success() && valid_wav(&wav) => Ok(wav),
            result => {
                // This fixed worker does not launch descendants. Its single
                // process is killed and reaped before a deadline is reported;
                // dropping the caller also kills it through Tokio ownership.
                let _ = child.kill().await;
                let _ = child.wait().await;
                match result {
                    Err(_) => Err(error("whatsapp_audio_timeout", "audio.timeout")),
                    Ok(Ok((status, _))) if status.code() == Some(2) => {
                        Err(error("whatsapp_audio_invalid", "audio.too_long"))
                    }
                    _ => Err(error("whatsapp_audio_invalid", "audio.incomplete")),
                }
            }
        }
    }
}
fn valid_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 46
        && bytes.len() <= MAX_WAV
        && &bytes[..4] == b"RIFF"
        && &bytes[8..16] == b"WAVEfmt "
        && bytes[20..24] == [1, 0, 1, 0]
        && bytes[24..28] == 16000u32.to_le_bytes()
        && bytes[34..36] == 16u16.to_le_bytes()
        && &bytes[36..40] == b"data"
        && u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize == bytes.len() - 44
}
/// Private worker entry: raw bounded stdin/stdout, with no configuration,
/// credentials, account owner, networking or provider initialization.
pub fn worker_main() -> i32 {
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_INPUT
    {
        return 2;
    }
    match decode_ogg_opus(&bytes) {
        Ok(wav) if std::io::stdout().write_all(&wav).is_ok() => 0,
        _ => 2,
    }
}

struct Stream {
    serial: u32,
    decoder: opus::MSDecoder,
    channels: usize,
    pre_skip: usize,
    decoded: usize,
    start: usize,
    tags: bool,
    scratch: Vec<f32>,
}
impl Stream {
    fn open(packet: &ogg::Packet, start: usize) -> Result<Self, ()> {
        let head = &packet.data;
        if !packet.first_in_stream()
            || head.len() < 19
            || &head[..8] != b"OpusHead"
            || head[8] > 15
            || head[9] == 0
        {
            return Err(());
        }
        let channels = head[9] as usize;
        let (streams, coupled, mapping) = if head[18] == 0 {
            if channels > 2 {
                return Err(());
            }
            (1, (channels == 2) as u8, (0..head[9]).collect::<Vec<_>>())
        } else {
            if head.len() < 21 + channels || ![1, 255].contains(&head[18]) {
                return Err(());
            }
            (head[19], head[20], head[21..21 + channels].to_vec())
        };
        let mut decoder =
            opus::MSDecoder::new(48000, streams, coupled, &mapping).map_err(|_| ())?;
        decoder
            .set_gain(i16::from_le_bytes([head[16], head[17]]) as i32)
            .map_err(|_| ())?;
        Ok(Self {
            serial: packet.stream_serial(),
            decoder,
            channels,
            pre_skip: u16::from_le_bytes([head[10], head[11]]) as usize,
            decoded: 0,
            start,
            tags: false,
            scratch: vec![0f32; 5760 * channels],
        })
    }
}

/// Decode only the Ogg/Opus container, trim pre-skip/final granules and apply
/// header gain. At 48 kHz the established 16 kHz linear resample selects every
/// third frame. Store bounded PCM rather than a whole decoded float recording.
pub fn decode_ogg_opus(bytes: &[u8]) -> Result<Vec<u8>, ()> {
    if bytes.len() > MAX_INPUT {
        return Err(());
    }
    let mut reader = PacketReader::new(Cursor::new(bytes));
    let mut stream: Option<Stream> = None;
    let mut samples = Vec::new();
    let mut saw_end = false;
    while let Some(packet) = reader.read_packet().map_err(|_| ())? {
        if stream.is_none() {
            stream = Some(Stream::open(&packet, samples.len())?);
            saw_end = false;
            continue;
        }
        let state = stream.as_mut().unwrap();
        if packet.stream_serial() != state.serial {
            return Err(());
        }
        if !state.tags {
            if !packet.data.starts_with(b"OpusTags") || packet.last_in_stream() {
                return Err(());
            }
            state.tags = true;
            continue;
        }
        if packet.data.is_empty() {
            return Err(());
        }
        let frames = state
            .decoder
            .decode_float(&packet.data, &mut state.scratch, false)
            .map_err(|_| ())?;
        let before = state.decoded;
        state.decoded = state.decoded.checked_add(frames).ok_or(())?;
        for frame in 0..frames {
            let absolute = before + frame;
            if absolute < state.pre_skip || (absolute - state.pre_skip) % 3 != 0 {
                continue;
            }
            let mean = state.scratch[frame * state.channels..(frame + 1) * state.channels]
                .iter()
                .map(|&s| if s.is_finite() { s as f64 } else { 0.0 })
                .sum::<f64>()
                / state.channels as f64;
            samples.push(audio_processing::pcm16_sample(mean));
        }
        // At most one packet of end padding and the 16-bit pre-skip are
        // temporarily present before the final page trims its recording.
        if samples.len() > MAX_FRAMES + 24_000 {
            return Err(());
        }
        if packet.last_in_stream() {
            let end = usize::try_from(packet.absgp_page()).map_err(|_| ())?;
            if end > state.decoded || end < state.pre_skip {
                return Err(());
            }
            samples.truncate(state.start + (end - state.pre_skip).div_ceil(3));
            if samples.len() > MAX_FRAMES {
                return Err(());
            }
            stream = None;
            saw_end = true;
        }
    }
    if stream.is_some() || !saw_end || samples.is_empty() {
        return Err(());
    }
    audio_processing::encode_pcm16_wav(&samples, 16000).map_err(|_| ())
}
