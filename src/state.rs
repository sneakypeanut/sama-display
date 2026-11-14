//! Acknowledged-frame state: atomic JSON status and PNG preview publication.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use tempfile::Builder;

use image::RgbaImage;
use image::{ExtendedColorType, ImageEncoder};

/// One acknowledged frame. Field names and ordering preserve the documented
/// JSON schema.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub sequence: u64,
    pub started_at: String,
    pub sampled_at: String,
    pub acknowledged_at: String,
    pub ack_latency_seconds: f64,
    pub frame_latency_seconds: f64,
    pub cpu_celsius: Option<f64>,
    pub gpu_celsius: Option<f64>,
    pub rotation: u16,
    pub display_id: String,
    pub ack: String,
    pub image: String,
}

/// Publication seam for tests that inject disk failures.
pub trait Publisher {
    fn publish(&self, state_dir: &Path, image: &RgbaImage, status: &Status) -> Result<()>;
}

pub struct FilePublisher;

impl Publisher for FilePublisher {
    fn publish(&self, state_dir: &Path, image: &RgbaImage, status: &Status) -> Result<()> {
        publish_files(state_dir, image, status)
    }
}

/// Replaces `path` only after the complete temporary file is flushed and
/// fsynced. On failure the previous file is untouched and no temp remains.
pub fn atomic_write<F>(path: &Path, writer: F) -> Result<()>
where
    F: FnOnce(&mut File) -> Result<()>,
{
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let mut temporary = Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    writer(temporary.as_file_mut())?;
    temporary.as_file_mut().flush()?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

pub fn publish_files(state_dir: &Path, image: &RgbaImage, status: &Status) -> Result<()> {
    atomic_write(&state_dir.join("latest-frame.png"), |file| {
        let mut writer = BufWriter::new(&mut *file);
        let encoder = image::codecs::png::PngEncoder::new(&mut writer);
        encoder
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .context("encode preview PNG")?;
        writer.flush().context("flush preview PNG")?;
        Ok(())
    })?;
    let json = serde_json::to_string_pretty(status).context("serialize status")? + "\n";
    atomic_write(&state_dir.join("status.json"), |file| {
        file.write_all(json.as_bytes())?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn atomic_failure_preserves_last_status() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("status.json");
        std::fs::write(&path, "old").unwrap();
        let error = atomic_write(&path, |file| {
            file.write_all(b"partial")?;
            Err(std::io::Error::new(ErrorKind::StorageFull, "disk full").into())
        })
        .unwrap_err();
        assert!(error.to_string().contains("disk full"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        let entries: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(entries, vec![path]);
    }

    #[test]
    fn publish_writes_png_and_status() {
        let directory = tempfile::tempdir().expect("tempdir");
        let image = RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 4]));
        let status = Status {
            sequence: 7,
            started_at: "2026-01-01T00:00:00+00:00".to_string(),
            sampled_at: "2026-01-01T00:00:01+00:00".to_string(),
            acknowledged_at: "2026-01-01T00:00:02+00:00".to_string(),
            ack_latency_seconds: 0.5,
            frame_latency_seconds: 0.75,
            cpu_celsius: Some(55.0),
            gpu_celsius: None,
            rotation: 90,
            display_id: "chs_5inch.dev1_rom1.89".to_string(),
            ack: "full_png_sucess".to_string(),
            image: "latest-frame.png".to_string(),
        };
        publish_files(directory.path(), &image, &status).expect("publish");
        let png = std::fs::read(directory.path().join("latest-frame.png")).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join("status.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(json["sequence"], 7);
        assert_eq!(json["rotation"], 90);
        assert_eq!(json["cpu_celsius"], 55.0);
        assert!(json["gpu_celsius"].is_null());
        assert_eq!(json["ack"], "full_png_sucess");
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            2,
            "no temporary files remain"
        );
    }
}
