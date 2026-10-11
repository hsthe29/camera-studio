//! Where captured media lives and how it is named.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gtk::glib;

use crate::settings::Settings;

pub const PHOTO_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "webm", "mkv", "mov"];

fn special_dir(kind: glib::UserDirectory, fallback: &str) -> PathBuf {
    glib::user_special_dir(kind)
        .unwrap_or_else(|| glib::home_dir().join(fallback))
        .join("Camera")
}

pub fn photo_dir(settings: &Settings) -> PathBuf {
    settings
        .get()
        .photo_dir
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| special_dir(glib::UserDirectory::Pictures, "Pictures"))
}

pub fn video_dir(settings: &Settings) -> PathBuf {
    settings
        .get()
        .video_dir
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| special_dir(glib::UserDirectory::Videos, "Videos"))
}

/// Reserve and return a new path like `IMG_20261009_211530.jpg` (adds `_2`, `_3`… on collision).
/// The empty file is created immediately so concurrent captures never share a name;
/// empty files are ignored by [`list_media`] until the encoder fills them.
pub fn unique_path(dir: &Path, prefix: &str, ext: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = glib::DateTime::now_local()
        .and_then(|d| d.format("%Y%m%d_%H%M%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "00000000_000000".into());
    let stem = format!("{prefix}_{stamp}");
    for n in 1.. {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        let candidate = dir.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaFile {
    pub path: PathBuf,
    pub is_video: bool,
    pub mtime: SystemTime,
    pub size: u64,
}

impl MediaFile {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// `Some(true)` for videos, `Some(false)` for photos.
pub fn classify(path: &Path) -> Option<bool> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        Some(true)
    } else if PHOTO_EXTENSIONS.contains(&ext.as_str()) {
        Some(false)
    } else {
        None
    }
}

/// All photos and videos in the directories, newest first.
pub fn list_media(dirs: &[PathBuf]) -> Vec<MediaFile> {
    let mut seen = Vec::new();
    let mut items = Vec::new();
    for dir in dirs {
        let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Some(is_video) = classify(&path) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || meta.len() == 0 {
                continue;
            }
            items.push(MediaFile {
                path,
                is_video,
                mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                size: meta.len(),
            });
        }
    }
    items.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| b.path.cmp(&a.path)));
    items
}

pub fn human_size(size: u64) -> String {
    let mut value = size as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if value < 1024.0 || unit == "GB" {
            return if unit == "B" {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1024.0;
    }
    unreachable!()
}

pub fn format_duration(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn format_time(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    glib::DateTime::from_unix_local(secs)
        .and_then(|d| d.format("%d/%m/%Y %H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cs-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unique_paths_are_reserved() {
        let d = tmpdir("uniq");
        let paths: std::collections::HashSet<_> = (0..5)
            .map(|_| unique_path(&d, "IMG", "jpg").unwrap())
            .collect();
        assert_eq!(paths.len(), 5);
        assert!(paths.iter().all(|p| p.exists()));
        assert!(list_media(&[d.clone()]).is_empty()); // empty reservations are hidden
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn media_listing_sorted_newest_first() {
        let d = tmpdir("list");
        for (i, name) in ["a.jpg", "b.mp4", "c.txt", "d.webm"].iter().enumerate() {
            let p = d.join(name);
            std::fs::write(&p, b"x").unwrap();
            let t = SystemTime::now() + std::time::Duration::from_secs(i as u64 * 10);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        let media = list_media(&[d.clone(), d.clone()]);
        let names: Vec<_> = media.iter().map(|m| m.name()).collect();
        assert_eq!(names, ["d.webm", "b.mp4", "a.jpg"]);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn formatting() {
        assert_eq!(format_duration(65.0), "01:05");
        assert_eq!(format_duration(3725.0), "1:02:05");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
    }
}
