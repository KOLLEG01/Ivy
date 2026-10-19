//! Unchanged video configuration and sampled-media evidence admission.
use ivy_contracts::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle, json::canonical};
use std::sync::OnceLock;

pub fn format(media_type: &str) -> Option<&'static str> {
    match media_type {
        "video/mp4" | "video/quicktime" => Some("mov"),
        "video/webm" | "video/x-matroska" => Some("matroska"),
        "video/x-msvideo" => Some("avi"),
        _ => None,
    }
}
pub fn validate(name: &str, value: &IvyJson) -> IvyResult<()> {
    canonical(value, 1024 * 1024)?;
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            SchemaBundle::new(&[include_str!(
                "../../../../specs/schemas/secretary-video.schema.json"
            )])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if !matches!(name, "VideoConfiguration" | "VideoProjection")
        || !bundle.matches(&format!("#/$defs/{name}"), value)?
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Content does not satisfy its exact contract.",
        ));
    }
    if name == "VideoProjection" {
        let audio = value.required("audio")?;
        let has_audio = !audio.is_null();
        let limitations = value
            .required("limitations")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let required = [
            "visual_frames_sampled",
            "images_resized",
            "subtitles_and_auxiliary_streams_not_interpreted",
            if has_audio {
                "audio_downmixed_and_resampled"
            } else {
                "source_has_no_audio"
            },
        ];
        if !required
            .iter()
            .all(|s| limitations.contains(&IvyJson::from(*s)))
            || limitations.contains(&IvyJson::from(if has_audio {
                "source_has_no_audio"
            } else {
                "audio_downmixed_and_resampled"
            }))
        {
            return Err(IvyError::new(
                "media_video_projection_invalid",
                "Video context must declare its actual sampling and audio limitations.",
            ));
        }
        let duration = value.required("durationSeconds")?.as_f64().unwrap();
        let frames = value.required("frames")?.as_array().unwrap();
        let valid = frames.iter().enumerate().all(|(index, frame)| {
            frame.get("inputOffset").and_then(IvyJson::as_f64) == Some((index + 1) as f64)
                && frame.get("sampleTimeSeconds").and_then(IvyJson::as_f64)
                    == Some(index as f64 * duration / 8.0)
        });
        let audio_valid = !has_audio
            || (audio.get("inputOffset").and_then(IvyJson::as_f64)
                == Some((frames.len() + 1) as f64)
                && audio.required("startTimeSeconds")?.as_f64().unwrap()
                    + audio.required("durationSeconds")?.as_f64().unwrap()
                    <= duration + 0.1);
        if !valid || !audio_valid {
            return Err(IvyError::new(
                "media_video_projection_invalid",
                "Projected media offsets and times must remain in the original bounded timeline.",
            ));
        }
    }
    Ok(())
}
