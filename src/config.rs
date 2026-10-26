use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

fn one() -> f32 {
    1.0
}

fn default_master() -> f32 {
    0.9
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PadConfig {
    pub id: u64,
    pub name: String,
    pub file: String,
    #[serde(default = "one")]
    pub volume: f32,
    #[serde(default)]
    pub looping: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default = "default_master")]
    pub master: f32,
    #[serde(default)]
    pub pads: Vec<PadConfig>,
    #[serde(default)]
    pub window: Option<[f32; 2]>,
    #[serde(default)]
    pub library_folders: Vec<String>,
    #[serde(default)]
    pub system_audio: bool,
    #[serde(default)]
    pub capture_device: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: None,
            master: default_master(),
            pads: Vec::new(),
            window: Some([760.0, 500.0]),
            library_folders: Vec::new(),
            system_audio: false,
            capture_device: None,
        }
    }
}

pub fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    base_dir().join("soundboard.json")
}

pub fn sounds_dir() -> PathBuf {
    base_dir().join("sounds")
}

pub fn load() -> Config {
    let Ok(text) = fs::read_to_string(config_path()) else {
        return Config::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(cfg: &Config) {
    if let Ok(text) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(config_path(), text);
    }
}

pub fn import_file(src: &Path) -> Result<String, String> {
    let dir = sounds_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create sounds folder: {e}"))?;

    let stem: String = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sound")
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("wav")
        .to_lowercase();

    let mut candidate = format!("{stem}.{ext}");
    let mut n = 2;
    while dir.join(&candidate).exists() {
        candidate = format!("{stem} ({n}).{ext}");
        n += 1;
    }

    fs::copy(src, dir.join(&candidate)).map_err(|e| format!("Could not copy file: {e}"))?;
    Ok(candidate)
}
