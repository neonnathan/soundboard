use crate::audio::Engine;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const DOWNLOAD_URL: &str =
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack43.zip";
pub const HOME_URL: &str = "https://vb-audio.com/Cable/";

pub fn has_cable(devices: &[String]) -> bool {
    devices
        .iter()
        .any(|name| name.to_lowercase().contains("cable input"))
}

pub fn cable_input(devices: &[String]) -> Option<String> {
    devices
        .iter()
        .find(|name| name.to_lowercase().contains("cable input"))
        .cloned()
}

pub enum Event {
    Status(String),
    Done(Result<(), String>),
}

pub fn install(tx: std::sync::mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        let result = run_install(&tx);
        let _ = tx.send(Event::Done(result));
    });
}

fn run_install(tx: &std::sync::mpsc::Sender<Event>) -> Result<(), String> {
    let temp = std::env::temp_dir().join("soundboard-vbcable");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).map_err(|e| format!("temp folder: {e}"))?;

    let _ = tx.send(Event::Status("Downloading VB-Audio Cable...".to_string()));
    let archive_path = temp.join("cable.zip");
    crate::library::download(DOWNLOAD_URL, &archive_path)
        .map_err(|e| format!("{e} - open vb-audio.com/Cable manually"))?;

    let _ = tx.send(Event::Status("Extracting installer...".to_string()));
    let file = std::fs::File::open(&archive_path).map_err(|e| format!("{e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("could not read archive: {e}"))?;
    archive
        .extract(&temp)
        .map_err(|e| format!("could not extract: {e}"))?;

    let installer = find_installer(&temp)
        .ok_or_else(|| "installer not found in archive".to_string())?;

    let _ = tx.send(Event::Status(
        "Approve the admin prompt, then click Install Driver".to_string(),
    ));

    let escaped = installer.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$p = Start-Process -FilePath '{escaped}' -Verb RunAs -PassThru; if ($p) {{ $p.WaitForExit() }}"
    );
    Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .status()
        .map_err(|e| format!("could not start installer: {e}"))?;

    let _ = tx.send(Event::Status("Waiting for the new device...".to_string()));
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(500));
        if has_cable(&Engine::list_devices()) {
            let _ = std::fs::remove_dir_all(&temp);
            return Ok(());
        }
    }

    let _ = std::fs::remove_dir_all(&temp);
    Err("Cable installed but the device did not show up yet. A reboot may be needed.".to_string())
}

fn find_installer(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut fallback = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_installer(&path) {
                return Some(found);
            }
            continue;
        }
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        if name.ends_with(".exe") && name.contains("setup") {
            if name.contains("x64") {
                return Some(path);
            }
            fallback = Some(path);
        }
    }
    fallback
}

pub fn open_home_page() {
    let _ = Command::new("cmd")
        .args(["/C", "start", "", HOME_URL])
        .spawn();
}
