use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Clip {
    pub samples: Arc<Vec<f32>>,
    pub rate: u32,
    pub channels: usize,
    pub duration: f32,
}

struct Voice {
    pad: u64,
    clip: Arc<Clip>,
    pos: f64,
    volume: f32,
    looping: bool,
    done: bool,
}

struct Mixer {
    voices: Vec<Voice>,
    master: f32,
    out_rate: f64,
    out_channels: usize,
    capture: CaptureBuf,
}

struct CaptureBuf {
    data: VecDeque<f32>,
    rate: f64,
    channels: usize,
    pos: f64,
    gain: f32,
    active: bool,
}

impl CaptureBuf {
    fn new() -> Self {
        Self {
            data: VecDeque::new(),
            rate: 48_000.0,
            channels: 2,
            pos: 0.0,
            gain: 1.0,
            active: false,
        }
    }

    fn push(&mut self, samples: &[f32], rate: u32, channels: usize) {
        if !self.active || samples.is_empty() {
            return;
        }
        if self.channels != channels || (self.rate - rate as f64).abs() > 1.0 {
            self.data.clear();
            self.pos = 0.0;
            self.channels = channels.max(1);
            self.rate = rate as f64;
        }
        self.data.extend(samples.iter().copied());

        let max = (self.rate * 2.0 * self.channels as f64) as usize;
        if self.data.len() > max {
            let excess = self.data.len() - max;
            self.data.drain(..excess);
        }
    }

    fn render(&mut self, out: &mut [f32], out_channels: usize, out_rate: f64, master: f32) {
        if !self.active || self.data.is_empty() {
            return;
        }
        let ch = self.channels.max(1);
        let frames_available = self.data.len() / ch;
        if frames_available < 2 {
            return;
        }
        let out_frames = out.len() / out_channels;
        let step = self.rate / out_rate.max(1.0);
        let mut pos = self.pos;
        let gain = self.gain * master;

        for frame in 0..out_frames {
            let i0 = pos as usize;
            if i0 + 1 >= frames_available {
                break;
            }
            let frac = (pos - i0 as f64) as f32;
            for out_ch in 0..out_channels {
                let src_ch = if ch == 1 { 0 } else { out_ch.min(ch - 1) };
                let a = self.data[i0 * ch + src_ch];
                let b = self.data[(i0 + 1) * ch + src_ch];
                out[frame * out_channels + out_ch] += (a + (b - a) * frac) * gain;
            }
            pos += step;
        }

        let consumed = pos as usize;
        if consumed > 0 {
            self.data.drain(..consumed * ch);
            self.pos = pos - consumed as f64;
        } else {
            self.pos = pos;
        }
    }
}

#[derive(Clone)]
pub struct CaptureSink {
    mixer: Arc<Mutex<Mixer>>,
    frames: Arc<AtomicU64>,
}

impl CaptureSink {
    pub fn push(&self, samples: &[f32], rate: u32, channels: usize) {
        self.frames.fetch_add(
            samples.len() as u64 / channels.max(1) as u64,
            Ordering::Relaxed,
        );
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.capture.push(samples, rate, channels);
        }
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

impl Mixer {
    fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);

        let ch = self.out_channels.max(1);
        let frames = out.len() / ch;
        let master = self.master;

        for voice in self.voices.iter_mut() {
            if voice.done {
                continue;
            }

            let clip_ch = voice.clip.channels.max(1);
            let clip_frames = voice.clip.samples.len() / clip_ch;
            if clip_frames == 0 {
                voice.done = true;
                continue;
            }

            let step = voice.clip.rate as f64 / self.out_rate.max(1.0);
            let gain = voice.volume * master;
            let mut pos = voice.pos;

            for frame in 0..frames {
                if pos >= clip_frames as f64 {
                    if voice.looping {
                        pos -= clip_frames as f64;
                    } else {
                        voice.done = true;
                        break;
                    }
                }

                let i0 = pos as usize;
                let frac = (pos - i0 as f64) as f32;
                let i1 = if i0 + 1 < clip_frames {
                    i0 + 1
                } else if voice.looping {
                    0
                } else {
                    i0
                };

                for out_ch in 0..ch {
                    let src_ch = if clip_ch == 1 {
                        0
                    } else {
                        out_ch.min(clip_ch - 1)
                    };
                    let a = voice.clip.samples[i0 * clip_ch + src_ch];
                    let b = voice.clip.samples[i1 * clip_ch + src_ch];
                    out[frame * ch + out_ch] += (a + (b - a) * frac) * gain;
                }

                pos += step;
            }

            voice.pos = pos;
        }

        self.voices.retain(|v| !v.done);

        self.capture.render(out, ch, self.out_rate, master);

        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}

pub struct Engine {
    mixer: Arc<Mutex<Mixer>>,
    stream: Option<cpal::Stream>,
    pub device: Option<String>,
    pub error: Arc<Mutex<Option<String>>>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            mixer: Arc::new(Mutex::new(Mixer {
                voices: Vec::new(),
                master: 0.9,
                out_rate: 48_000.0,
                out_channels: 2,
                capture: CaptureBuf::new(),
            })),
            stream: None,
            device: None,
            error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn list_devices() -> Vec<String> {
        let host = cpal::default_host();
        let mut names = Vec::new();
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                if let Ok(description) = device.description() {
                    let name = description.name().to_string();
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
        }
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    pub fn start(&mut self, name: Option<&str>) {
        self.stream = None;
        if let Ok(mut err) = self.error.lock() {
            *err = None;
        }

        let host = cpal::default_host();
        let device = name
            .and_then(|wanted| {
                host.output_devices().ok().and_then(|mut devices| {
                    devices.find(|d| {
                        d.description()
                            .map(|description| description.name() == wanted)
                            .unwrap_or(false)
                    })
                })
            })
            .or_else(|| host.default_output_device());

        let Some(device) = device else {
            self.set_error("No audio output device found".to_string());
            return;
        };

        self.device = device
            .description()
            .ok()
            .map(|description| description.name().to_string());

        let supported = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                self.set_error(format!("Device config failed: {e}"));
                return;
            }
        };

        let config: StreamConfig = supported.config();
        let channels = config.channels as usize;
        let rate = config.sample_rate;

        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.out_channels = channels;
            mixer.out_rate = rate as f64;
        }

        let err_sink = self.error.clone();
        let err_fn = move |e: cpal::Error| {
            if let Ok(mut slot) = err_sink.lock() {
                *slot = Some(format!("Audio stream error: {e}"));
            }
        };

        let result = match supported.sample_format() {
            SampleFormat::F32 => Self::build::<f32>(&device, config, self.mixer.clone(), err_fn),
            SampleFormat::I16 => Self::build::<i16>(&device, config, self.mixer.clone(), err_fn),
            SampleFormat::U16 => Self::build::<u16>(&device, config, self.mixer.clone(), err_fn),
            SampleFormat::I32 => Self::build::<i32>(&device, config, self.mixer.clone(), err_fn),
            SampleFormat::F64 => Self::build::<f64>(&device, config, self.mixer.clone(), err_fn),
            other => Err(format!("Unsupported sample format: {other:?}")),
        };

        match result {
            Ok(stream) => self.stream = Some(stream),
            Err(e) => self.set_error(e),
        }
    }

    fn build<T>(
        device: &cpal::Device,
        config: StreamConfig,
        mixer: Arc<Mutex<Mixer>>,
        err_fn: impl FnMut(cpal::Error) + Send + 'static,
    ) -> Result<cpal::Stream, String>
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let mut scratch: Vec<f32> = Vec::new();
        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                    if scratch.len() < data.len() {
                        scratch.resize(data.len(), 0.0);
                    }
                    let mut mixer = match mixer.lock() {
                        Ok(m) => m,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    mixer.render(&mut scratch[..data.len()]);
                    let len = data.len();
                    for (out, sample) in data.iter_mut().zip(scratch[..len].iter()) {
                        *out = T::from_sample(*sample);
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("Could not open output stream: {e}"))?;

        stream
            .play()
            .map_err(|e| format!("Could not start playback: {e}"))?;
        Ok(stream)
    }

    fn set_error(&self, message: String) {
        if let Ok(mut err) = self.error.lock() {
            *err = Some(message);
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut e| e.take())
    }

    pub fn capture_sink(&self) -> CaptureSink {
        CaptureSink {
            mixer: self.mixer.clone(),
            frames: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_capture_active(&self, active: bool) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.capture.active = active;
            if !active {
                mixer.capture.data.clear();
                mixer.capture.pos = 0.0;
            }
        }
    }

    pub fn play(&self, pad: u64, clip: Arc<Clip>, volume: f32, looping: bool) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.voices.push(Voice {
                pad,
                clip,
                pos: 0.0,
                volume,
                looping,
                done: false,
            });
        }
    }

    pub fn stop_pad(&self, pad: u64) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.voices.retain(|v| v.pad != pad);
        }
    }

    pub fn stop_all(&self) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.voices.clear();
        }
    }

    pub fn active(&self) -> HashSet<u64> {
        match self.mixer.lock() {
            Ok(mixer) => mixer.voices.iter().map(|v| v.pad).collect(),
            Err(_) => HashSet::new(),
        }
    }

    pub fn set_master(&self, master: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.master = master.clamp(0.0, 2.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_mixes_into_output() {
        let mut capture = CaptureBuf::new();
        capture.active = true;
        capture.push(&vec![0.5f32; 480], 48_000, 2);

        let mut out = vec![0.0f32; 200];
        capture.render(&mut out, 2, 48_000.0, 1.0);

        assert!(out.iter().all(|sample| (*sample - 0.5).abs() < 0.01));
    }

    #[test]
    fn capture_resamples_without_panicking() {
        let mut capture = CaptureBuf::new();
        capture.active = true;
        let input: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.01).sin()).collect();
        capture.push(&input, 44_100, 1);

        let mut out = vec![0.0f32; 9600];
        capture.render(&mut out, 2, 48_000.0, 1.0);

        assert!(out.iter().any(|sample| sample.abs() > 0.0));
    }
}