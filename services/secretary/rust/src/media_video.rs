//! Original bounded video policy. OS process ownership remains a required Host binding.
use crate::{
    local_files::{native, safe_directory, same_path},
    media_input::{self, AcquiredMediaPreparer, PrepareFuture, Prepared, REQUEST_BYTES, Source},
    video_schema,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, Outcome,
    hash::digest,
    json::{canonical, number_from_string, parse, try_js_string},
};
use ivy_host_runtime::{filesystem, node_path, paths};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub const MAXIMUM_SOURCE: usize = 32 * 1024 * 1024;
pub const MAXIMUM_AUDIO: usize = 90 * 16000 * 2;
pub const MAXIMUM_FRAME: usize = 512 * 1024;
pub type VideoFuture<'a, T> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
#[derive(Clone, Debug)]
pub struct VideoCommand {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub maximum_bytes: usize,
    pub timeout_ms: u32,
    pub environment: BTreeMap<OsString, OsString>,
}
/// Exact binary stdout, discard-only stderr, closed stdin, exact environment,
/// hidden launch and confirmed process retirement are required. There is no
/// text-decoding or unbounded launcher fallback. Unknown retains the scratch.
pub trait VideoCommands: Send + Sync {
    fn run<'a>(
        &'a self,
        command: VideoCommand,
        cancel: watch::Receiver<bool>,
    ) -> VideoFuture<'a, Vec<u8>>;
}
fn check(valid: bool, code: &str) -> IvyResult<()> {
    if valid {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Video preparation requires original verified bytes and bounded local decoding.",
        ))
    }
}
fn unavailable(source: &Source, code: &str) -> Prepared {
    Prepared::Unavailable {
        source: source.clone(),
        code: code.into(),
    }
}
fn number(value: Option<&IvyJson>) -> f64 {
    match value {
        None => f64::NAN,
        Some(IvyJson::Number(n)) => *n,
        Some(IvyJson::Null) => 0.0,
        Some(IvyJson::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(v) => try_js_string(v)
            .map(|s| number_from_string(&s))
            .unwrap_or(f64::NAN),
    }
}
fn nonnull(value: Option<&IvyJson>) -> Option<&IvyJson> {
    value.filter(|v| !v.is_null())
}
fn object(value: &IvyJson) -> IvyResult<&IvyJson> {
    check(value.as_object().is_some(), "media_video_invalid")?;
    Ok(value)
}
fn path(value: &IvyJson) -> IvyResult<PathBuf> {
    Ok(node_path::to_native(
        value
            .required("path")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?,
    ))
}
async fn executable(value: &IvyJson) -> IvyResult<()> {
    let selected = path(value)?;
    check(
        node_path::is_absolute(
            node_path::Style::native(),
            &node_path::from_native(&selected),
        ),
        "media_video_decoder_invalid",
    )?;
    let metadata = tokio::fs::symlink_metadata(native(&selected))
        .await
        .map_err(|_| IvyError::internal())?;
    check(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() > 0
            && metadata.len() <= 256 * 1024 * 1024
            && same_path(
                &paths::realpath(&selected).map_err(|_| IvyError::internal())?,
                &selected,
            )?,
        "media_video_decoder_invalid",
    )?;
    let bytes = tokio::fs::read(native(&selected))
        .await
        .map_err(|_| IvyError::internal())?;
    check(
        value.get("hash") == Some(&digest(&bytes).into()),
        "media_video_decoder_changed",
    )
}
pub async fn load_video_configuration(path: impl Into<PathBuf>) -> IvyResult<Option<IvyJson>> {
    let config = crate::config::load_service_configuration(
        path,
        1024 * 1024,
        IvyError::new(
            "media_video_config_unprotected",
            "Secretary service configuration must be a bounded protected regular file.",
        ),
    )
    .await?;
    let Some(value) = config.as_ref().and_then(|v| v.get("video")) else {
        return Ok(None);
    };
    video_schema::validate("VideoConfiguration", value)?;
    executable(value.required("ffmpeg")?).await?;
    executable(value.required("ffprobe")?).await?;
    Ok(Some(value.clone()))
}
fn signature(bytes: &[u8], format: &str) -> bool {
    // Buffer.toString('ascii') decodes seven-bit tags in the original reader.
    let ascii = |value: Option<&[u8]>, tag: &[u8]| {
        value.is_some_and(|v| v.len() == tag.len() && v.iter().zip(tag).all(|(a, b)| a & 127 == *b))
    };
    match format {
        "mov" => bytes.len() >= 16 && ascii(bytes.get(4..8), b"ftyp"),
        "avi" => {
            bytes.len() >= 16 && ascii(bytes.get(..4), b"RIFF") && ascii(bytes.get(8..12), b"AVI ")
        }
        _ => bytes.len() >= 8 && bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
    }
}
fn wave(pcm: &[u8]) -> IvyResult<Vec<u8>> {
    check(
        !pcm.is_empty() && pcm.len() % 2 == 0 && pcm.len() <= MAXIMUM_AUDIO,
        "media_video_audio_invalid",
    )?;
    let mut value = Vec::with_capacity(44 + pcm.len());
    value.extend_from_slice(b"RIFF");
    value.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    value.extend_from_slice(b"WAVEfmt ");
    value.extend_from_slice(&16u32.to_le_bytes());
    value.extend_from_slice(&1u16.to_le_bytes());
    value.extend_from_slice(&1u16.to_le_bytes());
    value.extend_from_slice(&16000u32.to_le_bytes());
    value.extend_from_slice(&32000u32.to_le_bytes());
    value.extend_from_slice(&2u16.to_le_bytes());
    value.extend_from_slice(&16u16.to_le_bytes());
    value.extend_from_slice(b"data");
    value.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    value.extend_from_slice(pcm);
    Ok(value)
}
/// Number.toPrecision(15) for the positive FPS ratio, including exact binary
/// midpoint ties (ECMAScript rounds up, Rust's formatter rounds to even).
fn fps(value: f64) -> String {
    if !value.is_finite() {
        return "Infinity".into();
    }
    let rendered = format!("{value:.14e}");
    let (mantissa, exponent) = rendered.split_once('e').unwrap();
    let mut exponent = exponent.parse::<i32>().unwrap();
    let mut digits = mantissa.replace('.', "").parse::<u64>().unwrap();
    let bits = value.to_bits();
    let m = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let e = ((bits >> 52) & 2047) as i32 - 1023 - 52;
    let k = exponent - 14;
    let odd = m >> m.trailing_zeros();
    if e + 1 - k + m.trailing_zeros() as i32 == 0 {
        let tie = if k <= 0 {
            Some(odd)
        } else {
            5u64.checked_pow(k as u32)
                .filter(|p| odd % p == 0)
                .map(|p| odd / p)
        };
        if tie.is_some_and(|v| v % 4 == 1) {
            digits += 1;
            if digits == 1_000_000_000_000_000 {
                digits /= 10;
                exponent += 1;
            }
        }
    }
    let digits = format!("{digits:015}");
    if !(-6..15).contains(&exponent) {
        format!(
            "{}.{}e{}{exponent}",
            &digits[..1],
            &digits[1..],
            if exponent >= 0 { "+" } else { "" }
        )
    } else if exponent < 0 {
        format!("0.{}{}", "0".repeat((-exponent - 1) as usize), digits)
    } else {
        let at = exponent as usize + 1;
        if at == 15 {
            digits
        } else {
            format!("{}.{}", &digits[..at], &digits[at..])
        }
    }
}
#[derive(Clone)]
struct Cached {
    key: String,
    inputs: Vec<IvyJson>,
    bytes: usize,
}
struct Active {
    result: watch::Sender<Option<IvyResult<Prepared>>>,
}
#[derive(Default)]
struct State {
    cache: VecDeque<Cached>,
    cache_bytes: usize,
    active: Option<Arc<Active>>,
}
struct Runtime {
    root: PathBuf,
    configuration: IvyJson,
    commands: Arc<dyn VideoCommands>,
    cancel: watch::Sender<bool>,
    state: Mutex<State>,
}
pub struct VideoPreparer {
    inner: Arc<Runtime>,
}
impl VideoPreparer {
    pub fn new(
        root: PathBuf,
        configuration: IvyJson,
        commands: Arc<dyn VideoCommands>,
    ) -> IvyResult<Self> {
        video_schema::validate("VideoConfiguration", &configuration)?;
        check(
            node_path::is_absolute(node_path::Style::native(), &node_path::from_native(&root)),
            "media_video_path_invalid",
        )?;
        let (cancel, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(Runtime {
                root,
                configuration,
                commands,
                cancel,
                state: Mutex::new(State::default()),
            }),
        })
    }
    pub async fn close(&self) {
        self.inner.cancel.send_replace(true);
        let active = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .active
            .clone();
        if let Some(active) = active {
            let _ = wait(&active).await;
        }
    }
    pub async fn prepare(
        &self,
        source: &Source,
        bytes: &[u8],
        maximum: f64,
    ) -> IvyResult<Prepared> {
        check(
            bytes.len() == source.byte_length && digest(bytes) == source.content_hash,
            "media_input_integrity_invalid",
        )?;
        check(
            maximum.is_finite()
                && maximum.fract() == 0.0
                && maximum > 0.0
                && maximum <= REQUEST_BYTES as f64,
            "media_input_limit_invalid",
        )?;
        if *self.inner.cancel.borrow() {
            return Err(IvyError::internal());
        }
        if bytes.len() > MAXIMUM_SOURCE {
            return Ok(unavailable(source, "media_video_too_large"));
        }
        let key = format!(
            "{}\0{}",
            source.content_hash,
            source
                .media_type
                .as_ref()
                .map(|s| String::from_utf16_lossy(&s.0))
                .unwrap_or_else(|| "null".into())
        );
        let active = {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = state.cache.iter().find(|c| c.key == key) {
                return Ok(if cached.bytes > maximum as usize {
                    unavailable(source, "media_native_input_too_large")
                } else {
                    Prepared::Video {
                        source: source.clone(),
                        inputs: cached.inputs.clone(),
                    }
                });
            }
            check(state.active.is_none(), "media_video_busy")?;
            let (result, _) = watch::channel(None);
            let active = Arc::new(Active { result });
            state.active = Some(active.clone());
            active
        };
        let runtime = self.inner.clone();
        let original = source.clone();
        let copy = bytes.to_vec();
        let completion = active.clone();
        tokio::spawn(async move {
            let mut result = runtime.run(&original, &copy).await;
            {
                let mut state = runtime.state.lock().unwrap_or_else(|p| p.into_inner());
                if let Ok(Prepared::Video { inputs, .. }) = &result {
                    match canonical(&IvyJson::Array(inputs.clone()), usize::MAX) {
                        Ok(text) => {
                            let size = text.len();
                            while !state.cache.is_empty()
                                && (state.cache_bytes + size > 16 * 1024 * 1024
                                    || state.cache.len() >= 8)
                            {
                                let first = state.cache.pop_front().unwrap();
                                state.cache_bytes -= first.bytes;
                            }
                            state.cache.push_back(Cached {
                                key,
                                inputs: inputs.clone(),
                                bytes: size,
                            });
                            state.cache_bytes += size;
                            if size > maximum as usize {
                                result = Ok(unavailable(&original, "media_native_input_too_large"));
                            }
                        }
                        Err(error) => result = Err(error),
                    }
                }
                let unknown = matches!(&result,Err(error) if error.outcome==Outcome::Unknown);
                completion.result.send_replace(Some(result));
                // A missing retirement receipt is not a new decoder admission.
                if !unknown {
                    state.active = None;
                }
            }
        });
        wait(&active).await
    }
}
async fn wait(active: &Active) -> IvyResult<Prepared> {
    let mut receiver = active.result.subscribe();
    loop {
        if let Some(result) = receiver.borrow_and_update().clone() {
            return result;
        }
        if receiver.changed().await.is_err() {
            return Err(IvyError::internal());
        }
    }
}
impl AcquiredMediaPreparer for VideoPreparer {
    fn prepare<'a>(
        &'a self,
        source: &'a Source,
        bytes: &'a [u8],
        maximum: f64,
    ) -> PrepareFuture<'a> {
        Box::pin(self.prepare(source, bytes, maximum))
    }
}
enum Failure {
    Uncoded,
    Coded(IvyError),
}
impl From<IvyError> for Failure {
    fn from(value: IvyError) -> Self {
        Self::Coded(value)
    }
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Uncoded
    }
}
impl Runtime {
    async fn command(
        &self,
        executable: PathBuf,
        args: Vec<String>,
        cwd: &Path,
        maximum_bytes: usize,
        timeout_ms: u32,
    ) -> Result<Vec<u8>, Failure> {
        if *self.cancel.borrow() {
            return Err(Failure::Uncoded);
        }
        let environment = [
            "SystemRoot",
            "WINDIR",
            "PATH",
            "TEMP",
            "TMP",
            "TMPDIR",
            "LANG",
        ]
        .into_iter()
        .filter_map(|key| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(|value| (key.into(), value))
        })
        .collect();
        self.commands
            .run(
                VideoCommand {
                    executable,
                    args,
                    cwd: cwd.into(),
                    maximum_bytes,
                    timeout_ms,
                    environment,
                },
                self.cancel.subscribe(),
            )
            .await
            .map_err(Failure::Coded)
    }
    async fn temporary(&self) -> Result<PathBuf, Failure> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(native(&root))?;
            safe_directory(&root)?;
            let entries = std::fs::read_dir(native(&root))?.collect::<Result<Vec<_>, _>>()?;
            check(entries.len() < 8, "media_video_scratch_full")?;
            let mut used = 0u64;
            for entry in entries {
                let kind = entry.file_type()?;
                let name = entry.file_name();
                check(
                    kind.is_dir()
                        && !kind.is_symlink()
                        && name.to_string_lossy().starts_with("video-"),
                    "media_video_path_invalid",
                )?;
                let directory = root.join(name);
                safe_directory(&directory)?;
                let files =
                    std::fs::read_dir(native(&directory))?.collect::<Result<Vec<_>, _>>()?;
                check(files.len() <= 9, "media_video_scratch_full")?;
                for file in files {
                    let metadata = std::fs::symlink_metadata(file.path())?;
                    check(
                        metadata.is_file() && !metadata.file_type().is_symlink(),
                        "media_video_path_invalid",
                    )?;
                    used += metadata.len();
                }
            }
            check(
                used + MAXIMUM_SOURCE as u64 + 8 * MAXIMUM_FRAME as u64 <= 256 * 1024 * 1024,
                "media_video_scratch_full",
            )?;
            loop {
                let path = root.join(format!("video-{}", uuid::Uuid::new_v4()));
                let builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = builder;
                    builder.mode(0o700);
                    match builder.create(native(&path)) {
                        Ok(()) => return Ok(path),
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(Failure::from(e)),
                    }
                }
                #[cfg(not(unix))]
                match builder.create(native(&path)) {
                    Ok(()) => return Ok(path),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(Failure::from(e)),
                }
            }
        })
        .await
        .map_err(|_| Failure::Uncoded)?
    }
    async fn run(&self, source: &Source, bytes: &[u8]) -> IvyResult<Prepared> {
        let Some(format) = video_schema::format(&media_input::mime(source)) else {
            return Ok(unavailable(source, "media_type_unsupported"));
        };
        if bytes.len() > MAXIMUM_SOURCE {
            return Ok(unavailable(source, "media_video_too_large"));
        }
        if !signature(bytes, format) {
            return Ok(unavailable(source, "media_signature_mismatch"));
        }
        let mut directory = None;
        let result:Result<Prepared,Failure>=async {
            // Filesystem errors are ordinary original errors; preserve their run
            // projection as media_video_invalid rather than leaking host details.
            for name in ["ffmpeg","ffprobe"] {executable(self.configuration.required(name)?).await.map_err(|e|if e.code=="internal_error"{Failure::Uncoded}else{Failure::Coded(e)})?;}
            let selected=self.temporary().await?;directory=Some(selected.clone());let input_path=selected.join("source.media");
            let mut options=tokio::fs::OpenOptions::new();options.write(true).create_new(true);
            #[cfg(unix)]{options.mode(0o600);}
            use tokio::io::AsyncWriteExt;
            let mut file=options.open(native(&input_path)).await?;file.write_all(bytes).await?;drop(file);
            let mut input=vec!["-protocol_whitelist".into(),"file".into(),"-f".into(),format.into()];if format=="mov"{input.extend(["-enable_drefs".into(),"0".into()]);}
            let selected_path=|path:&Path|String::from_utf16_lossy(&node_path::from_native(path).0);
            let mut args=vec!["-v".into(),"error".into(),"-max_alloc".into(),"67108864".into()];args.extend(input.clone());
            args.extend(["-show_entries".into(),"format=duration,start_time:stream=index,codec_type,width,height,duration,start_time".into(),"-of".into(),"json".into(),selected_path(&input_path)]);
            let probe_bytes=self.command(path(self.configuration.required("ffprobe")?)?,args,&selected,32768,15000).await?;
            let parsed=parse(&String::from_utf8_lossy(&probe_bytes),usize::MAX).map_err(|_|Failure::Uncoded)?;let probe=object(&parsed)?;
            let container=object(probe.get("format").ok_or_else(||IvyError::new("media_video_invalid","Video preparation requires original verified bytes and bounded local decoding."))?)?;
            let duration=number(container.get("duration"));check(duration.is_finite()&&duration>0.0&&duration<=90.0,"media_video_duration_unsupported")?;
            let streams=probe.get("streams").and_then(IvyJson::as_array);check(streams.is_some_and(|s|s.len()<=8),"media_video_streams_unsupported")?;
            let streams=streams.unwrap().iter().map(object).collect::<IvyResult<Vec<_>>>()?;
            let video:Vec<_>=streams.iter().copied().filter(|v|v.get("codec_type")==Some(&"video".into())).collect();let audio:Vec<_>=streams.iter().copied().filter(|v|v.get("codec_type")==Some(&"audio".into())).collect();
            check(video.len()==1&&audio.len()<=1,"media_video_streams_unsupported")?;
            let width=number(video[0].get("width"));let height=number(video[0].get("height"));
            check(width.is_finite()&&width.fract()==0.0&&(2.0..=4096.0).contains(&width)&&height.is_finite()&&height.fract()==0.0&&(2.0..=4096.0).contains(&height),"media_video_dimensions_unsupported")?;
            for stream in video.iter().chain(&audio){check(stream.get("index").and_then(IvyJson::as_f64).is_some_and(|n|n.is_finite()&&n.fract()==0.0&&(0.0..8.0).contains(&n)),"media_video_streams_unsupported")?;}
            let mut common=vec!["-v".into(),"error".into(),"-nostdin".into(),"-xerror".into(),"-max_alloc".into(),"67108864".into(),"-threads".into(),"1".into()];common.extend(input);common.extend(["-i".into(),selected_path(&input_path),"-map_metadata".into(),"-1".into(),"-fflags".into(),"+bitexact".into()]);
            let mut frames_args=common.clone();frames_args.extend(["-map".into(),format!("0:{}",video[0].required("index")?.as_f64().unwrap()),"-an".into(),"-sn".into(),"-dn".into(),"-filter_threads".into(),"1".into(),"-vf".into(),format!("fps=fps={}:start_time=0:round=down,scale=480:480:force_original_aspect_ratio=decrease:force_divisible_by=2",fps(8.0/duration)),"-frames:v".into(),"8".into(),"-c:v".into(),"mjpeg".into(),"-q:v".into(),"3".into(),"-threads".into(),"1".into(),"-flags".into(),"+bitexact".into(),"-f".into(),"image2".into(),selected_path(&selected.join("frame-%02d.jpg"))]);
            self.command(path(self.configuration.required("ffmpeg")?)?,frames_args,&selected,1024,30000).await?;
            let mut entries=tokio::fs::read_dir(native(&selected)).await?;let mut names=Vec::new();
            while let Some(entry)=entries.next_entry().await?{let name=entry.file_name().to_string_lossy().into_owned();let b=name.as_bytes();if b.len()==12&&b.starts_with(b"frame-")&&b[6].is_ascii_digit()&&b[7].is_ascii_digit()&&&b[8..]==b".jpg"{names.push(name);}}
            names.sort();check((1..=8).contains(&names.len()),"media_video_frames_invalid")?;
            let mut frames=Vec::new();let mut parts=Vec::new();
            use base64::{Engine,engine::general_purpose::STANDARD};
            for (index,name) in names.iter().enumerate(){check(*name==format!("frame-{:02}.jpg",index+1),"media_video_frames_invalid")?;let path=selected.join(name);let metadata=tokio::fs::symlink_metadata(native(&path)).await?;
                check(metadata.is_file()&&!metadata.file_type().is_symlink()&&metadata.len()<=MAXIMUM_FRAME as u64,"media_video_frames_invalid")?;
                let image=tokio::fs::read(native(&path)).await?;check(image.len()>=4&&image.starts_with(&[255,216])&&image.ends_with(&[255,217]),"media_video_frames_invalid")?;
                frames.push(IvyJson::object([("inputOffset",IvyJson::Number((index+1)as f64)),("sampleTimeSeconds",IvyJson::Number(index as f64*duration/8.0)),("contentHash",digest(&image).into())]));
                parts.push(IvyJson::object([("type","image".into()),("url",format!("data:image/jpeg;base64,{}",STANDARD.encode(image)).into())]));
            }
            let mut projected_audio=IvyJson::Null;
            if let Some(audio)=audio.first(){let mut args=common;args.extend(["-map".into(),format!("0:{}",audio.required("index")?.as_f64().unwrap()),"-vn".into(),"-sn".into(),"-dn".into(),"-ac".into(),"1".into(),"-ar".into(),"16000".into(),"-c:a".into(),"pcm_s16le".into(),"-threads".into(),"1".into(),"-flags".into(),"+bitexact".into(),"-f".into(),"s16le".into(),"pipe:1".into()]);
                let pcm=self.command(path(self.configuration.required("ffmpeg")?)?,args,&selected,MAXIMUM_AUDIO,30000).await?;let wav=wave(&pcm)?;let audio_duration=pcm.len()as f64/32000.0;
                let zero=IvyJson::Number(0.0);let container_start=nonnull(container.get("start_time")).unwrap_or(&zero);let start=number(nonnull(audio.get("start_time")).or(Some(container_start)))-number(Some(container_start));
                check(start.is_finite()&&start>=0.0&&start<=duration,"media_video_audio_invalid")?;
                if let Some(expected)=audio.get("duration"){let expected=number(Some(expected));check(expected.is_finite()&&(audio_duration-expected).abs()<=0.1,"media_video_audio_incomplete")?;}
                projected_audio=IvyJson::object([("inputOffset",IvyJson::Number((parts.len()+1)as f64)),("sampleRate",IvyJson::Number(16000.0)),("channels",IvyJson::Number(1.0)),("startTimeSeconds",IvyJson::Number(start)),("durationSeconds",IvyJson::Number(audio_duration)),("contentHash",digest(&wav).into())]);
                parts.push(IvyJson::object([("type","audio".into()),("url",format!("data:audio/wav;base64,{}",STANDARD.encode(wav)).into())]));
            }
            let projection=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("representation","video_samples".into()),("sourceHash",source.content_hash.clone().into()),("decoderHash",self.configuration.required("ffmpeg")?.required("hash")?.clone()),("probeHash",self.configuration.required("ffprobe")?.required("hash")?.clone()),("durationSeconds",IvyJson::Number(duration)),("width",IvyJson::Number(width)),("height",IvyJson::Number(height)),("frames",IvyJson::Array(frames)),("audio",projected_audio),("limitations",IvyJson::Array(["visual_frames_sampled","images_resized","subtitles_and_auxiliary_streams_not_interpreted",if audio.is_empty(){"source_has_no_audio"}else{"audio_downmixed_and_resampled"}].into_iter().map(Into::into).collect()))]);
            video_schema::validate("VideoProjection",&projection)?;
            parts.insert(0,IvyJson::object([("type","text".into()),("text",canonical(&projection,usize::MAX)?.into()),("text_elements",IvyJson::Array(vec![]))]));Ok(Prepared::Video{source:source.clone(),inputs:parts})
        }.await;
        // An unknown retirement cannot establish the original child-close point.
        // Retain its private scratch instead of deleting possibly active output.
        let unknown = matches!(&result,Err(Failure::Coded(e))if e.outcome==Outcome::Unknown);
        if let Some(directory) = directory.filter(|_| !unknown) {
            let root = self.root.clone();
            let actual = tokio::task::spawn_blocking(move || {
                let actual = paths::realpath(&directory).map_err(|_| IvyError::internal())?;
                let context = node_path::Context::current().map_err(|_| IvyError::internal())?;
                check(
                    paths::inside(&root, &actual).map_err(|_| IvyError::internal())?
                        && String::from_utf16_lossy(
                            &context
                                .relative(
                                    &node_path::from_native(&root),
                                    &node_path::from_native(&actual),
                                )
                                .0,
                        )
                        .starts_with("video-"),
                    "media_video_path_invalid",
                )?;
                Ok::<PathBuf, IvyError>(actual)
            })
            .await
            .map_err(|_| IvyError::internal())??;
            filesystem::remove_force(actual).await?;
        }
        match result {
            Ok(value) => Ok(value),
            Err(Failure::Coded(error)) if unknown || *self.cancel.borrow() => Err(error),
            Err(Failure::Coded(error)) => Ok(unavailable(source, &error.code)),
            Err(Failure::Uncoded) if *self.cancel.borrow() => Err(IvyError::internal()),
            Err(Failure::Uncoded) => Ok(unavailable(source, "media_video_invalid")),
        }
    }
}
