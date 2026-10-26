use crate::audio::CaptureSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;
use wasapi::{
    AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode,
    WaveFormat,
};

pub struct Capture {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    pub device: String,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

pub fn list_devices() -> Vec<String> {
    let _ = wasapi::initialize_mta();
    let Ok(enumerator) = DeviceEnumerator::new() else {
        return Vec::new();
    };
    let Ok(collection) = enumerator.get_device_collection(&Direction::Render) else {
        return Vec::new();
    };
    let Ok(count) = collection.get_nbr_devices() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for index in 0..count {
        if let Ok(device) = collection.get_device_at_index(index) {
            if let Ok(name) = device.get_friendlyname() {
                names.push(name);
            }
        }
    }
    names
}

pub fn start(device: Option<String>, sink: CaptureSink) -> Result<Capture, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let (ready_tx, ready_rx) = channel::<Result<String, String>>();

    let join = std::thread::spawn(move || {
        run(device, sink, stop_thread, ready_tx);
    });

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(device)) => Ok(Capture {
            stop,
            join: Some(join),
            device,
        }),
        Ok(Err(error)) => {
            let _ = join.join();
            Err(error)
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            Err("timed out opening the audio device".to_string())
        }
    }
}

fn run(
    device: Option<String>,
    sink: CaptureSink,
    stop: Arc<AtomicBool>,
    ready: Sender<Result<String, String>>,
) {
    let _ = wasapi::initialize_mta();

    let opened = match open(device.as_deref()) {
        Ok(opened) => opened,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let (client, capture, format, name) = opened;
    if ready.send(Ok(name)).is_err() {
        return;
    }

    capture_loop(&client, &capture, &format, &sink, &stop);
    let _ = client.stop_stream();
}

type Opened = (AudioClient, AudioCaptureClient, WaveFormat, String);

fn open(name: Option<&str>) -> Result<Opened, String> {
    let enumerator = DeviceEnumerator::new().map_err(|e| format!("audio enumerator: {e}"))?;

    let device = match name {
        Some(wanted) => {
            let collection = enumerator
                .get_device_collection(&Direction::Render)
                .map_err(|e| format!("audio devices: {e}"))?;
            collection
                .get_device_with_name(wanted)
                .map_err(|_| format!("device not found: {wanted}"))?
        }
        None => enumerator
            .get_default_device(&Direction::Render)
            .map_err(|e| format!("default device: {e}"))?,
    };

    let friendly = device
        .get_friendlyname()
        .unwrap_or_else(|_| "System audio".to_string());

    let mut client = device
        .get_iaudioclient()
        .map_err(|e| format!("open device: {e}"))?;
    let format = client.get_mixformat().map_err(|e| format!("format: {e}"))?;

    let mode = StreamMode::PollingShared {
        autoconvert: false,
        buffer_duration_hns: 200 * 10_000,
    };
    client
        .initialize_client(&format, &Direction::Capture, &mode)
        .map_err(|e| format!("loopback init: {e}"))?;

    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| format!("capture client: {e}"))?;
    client
        .start_stream()
        .map_err(|e| format!("loopback start: {e}"))?;

    Ok((client, capture, format, friendly))
}

fn capture_loop(
    client: &AudioClient,
    capture: &AudioCaptureClient,
    format: &WaveFormat,
    sink: &CaptureSink,
    stop: &AtomicBool,
) {
    let rate = format.get_samplespersec();
    let channels = format.get_nchannels() as usize;
    let block = format.get_blockalign() as usize;
    let bits = format.get_bitspersample();
    let sample_type = format.get_subformat().unwrap_or(SampleType::Float);

    if channels == 0 || block == 0 {
        let _ = client.stop_stream();
        return;
    }

    let mut bytes: Vec<u8> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        let frames = match capture.get_next_packet_size() {
            Ok(Some(frames)) => frames,
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(_) => break,
        };

        if frames == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        let need = frames as usize * block;
        if bytes.len() < need {
            bytes.resize(need, 0);
        }

        let (read, info) = match capture.read_from_device(&mut bytes[..need]) {
            Ok(value) => value,
            Err(_) => break,
        };
        if read == 0 {
            continue;
        }

        let read = read as usize;
        scratch.clear();
        if info.flags.silent {
            scratch.resize(read * channels, 0.0);
        } else {
            convert(
                &bytes[..read * block],
                channels,
                bits,
                sample_type,
                &mut scratch,
            );
        }

        if !scratch.is_empty() {
            sink.push(&scratch, rate, channels);
        }
    }
}

fn convert(
    bytes: &[u8],
    channels: usize,
    bits: u16,
    sample_type: SampleType,
    out: &mut Vec<f32>,
) {
    out.clear();
    let _ = channels;

    match (sample_type, bits) {
        (SampleType::Float, 32) => {
            out.reserve(bytes.len() / 4);
            for chunk in bytes.chunks_exact(4) {
                out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        (SampleType::Int, 16) => {
            out.reserve(bytes.len() / 2);
            for chunk in bytes.chunks_exact(2) {
                out.push(i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32_768.0);
            }
        }
        (SampleType::Int, 24) => {
            out.reserve(bytes.len() / 3);
            for chunk in bytes.chunks_exact(3) {
                let value = (chunk[0] as i32) | ((chunk[1] as i32) << 8) | ((chunk[2] as i32) << 16);
                let value = (value << 8) >> 8;
                out.push(value as f32 / 8_388_608.0);
            }
        }
        (SampleType::Int, 32) => {
            out.reserve(bytes.len() / 4);
            for chunk in bytes.chunks_exact(4) {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push(value as f32 / 2_147_483_648.0);
            }
        }
        _ => {}
    }
}
