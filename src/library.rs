use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct LocalSound {
    pub path: PathBuf,
    pub name: String,
}

pub fn scan_folders(folders: &[String]) -> Vec<LocalSound> {
    let mut out = Vec::new();
    for folder in folders {
        walk(Path::new(folder), 0, &mut out);
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<LocalSound>) {
    if depth > 8 || out.len() > 25_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth + 1, out);
        } else if crate::decode::is_audio(&path) {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("sound")
                .to_string();
            out.push(LocalSound { path, name });
        }
    }
}

pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    let mut response = ureq::get(url)
        .header("User-Agent", "soundboard/0.1 (Windows)")
        .call()
        .map_err(|error| format!("download: {error}"))?;
    let bytes = response
        .body_mut()
        .read_to_vec()
        .map_err(|error| format!("download: {error}"))?;
    std::fs::write(dest, bytes).map_err(|error| format!("download: {error}"))
}

pub fn extract_pack(zip_path: &Path, temp: &Path) -> Result<Vec<PathBuf>, String> {
    let _ = std::fs::remove_dir_all(temp);
    std::fs::create_dir_all(temp).map_err(|error| format!("temp folder: {error}"))?;

    let file = std::fs::File::open(zip_path).map_err(|error| format!("open pack: {error}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| format!("read pack: {error}"))?;

    let mut out = Vec::new();
    for index in 0..archive.len() {
        if out.len() >= 2000 {
            break;
        }
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        if entry.is_dir() || entry.size() > 60_000_000 {
            continue;
        }

        let Ok(entry_name) = entry.name() else {
            continue;
        };
        let entry_path = PathBuf::from(entry_name.as_ref());
        if !crate::decode::is_audio(&entry_path) {
            continue;
        }

        let Some(file_name) = entry_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let dest = unique_path(temp, file_name);
        let Ok(mut writer) = std::fs::File::create(&dest) else {
            continue;
        };
        let mut buffer = [0u8; 64 * 1024];
        let mut failed = false;
        loop {
            match entry.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if std::io::Write::write_all(&mut writer, &buffer[..n]).is_err() {
                        failed = true;
                        break;
                    }
                }
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        if !failed {
            out.push(dest);
        }
    }

    if out.is_empty() {
        let _ = std::fs::remove_dir_all(temp);
        return Err("no audio files found in that pack".to_string());
    }
    Ok(out)
}

fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(sanitize(file_name));
    if !candidate.exists() {
        return candidate;
    }
    let stem = candidate
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sound");
    let extension = candidate
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("wav");
    let mut counter = 2;
    loop {
        let next = dir.join(format!("{stem} ({counter}).{extension}"));
        if !next.exists() {
            return next;
        }
        counter += 1;
    }
}

pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "sound".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extracts_audio_from_zip() {
        let dir = std::env::temp_dir().join(format!("soundboard-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = dir.join("pack.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("sounds/blip.wav", options).unwrap();
            writer.write_all(b"RIFFxxxxWAVE").unwrap();
            writer.start_file("readme.txt", options).unwrap();
            writer.write_all(b"not audio").unwrap();
            writer.finish().unwrap();
        }

        let out = extract_pack(&zip_path, &dir.join("out")).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].to_string_lossy().ends_with("blip.wav"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
