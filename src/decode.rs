use crate::audio::Clip;
use std::path::Path;
use std::sync::Arc;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub const EXTENSIONS: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a", "aac", "mp4", "aiff"];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn decode(path: &Path) -> Result<Clip, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported file ({e})"))?;

    let (track_id, mut rate, mut channels) = {
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| "no audio track".to_string())?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| "unsupported codec".to_string())?;
        (
            track.id,
            params.sample_rate.unwrap_or(44_100),
            params.channels.clone().map(|c| c.count()).unwrap_or(2).max(1),
        )
    };

    let mut decoder = {
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| "no audio track".to_string())?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| "unsupported codec".to_string())?;
        symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| format!("decoder: {e}"))?
    };

    let mut samples: Vec<f32> = Vec::new();
    while let Some(packet) = format.next_packet().map_err(|e| format!("read: {e}"))? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                rate = spec.rate();
                channels = spec.channels().count().max(1);
                let start = samples.len();
                samples.resize(start + buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut samples[start..]);
            }
            Err(Error::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }

    if samples.is_empty() {
        return Err("no audio decoded".to_string());
    }

    let duration = samples.len() as f32 / (rate as f32 * channels as f32);
    Ok(Clip {
        samples: Arc::new(samples),
        rate,
        channels,
        duration,
    })
}
