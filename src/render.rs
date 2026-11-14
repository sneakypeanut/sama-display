//! Prototype-style round layout rendered with a system sans font and rotated
//! counterclockwise on request.

use ab_glyph::{Font, FontArc};
use anyhow::{Context, Result, bail};
use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_hollow_circle_mut, draw_line_segment_mut, draw_text_mut, text_size};
use std::path::Path;

use crate::encode::{HEIGHT, WIDTH};
use crate::run::Renderer;

pub const BACKGROUND: [u8; 3] = [0x08, 0x0d, 0x16];
pub const FONT_CANDIDATES: [&str; 4] = [
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
];

pub struct SystemRenderer {
    font: FontArc,
}

impl SystemRenderer {
    /// Loads Liberation Sans, then DejaVu Sans, and fails clearly when no
    /// supported system font is installed.
    pub fn load() -> Result<Self> {
        for candidate in FONT_CANDIDATES {
            if !Path::new(candidate).is_file() {
                continue;
            }
            let bytes = std::fs::read(candidate)
                .with_context(|| format!("read system font {candidate}"))?;
            if let Ok(font) = FontArc::try_from_vec(bytes) {
                return Ok(Self { font });
            }
        }
        bail!(
            "no usable Liberation Sans or DejaVu Sans font found; tried {FONT_CANDIDATES:?}. \
             Install fonts-liberation or fonts-dejavu."
        )
    }

    pub fn from_font_bytes(bytes: Vec<u8>) -> Result<Self> {
        Ok(Self {
            font: FontArc::try_from_vec(bytes).context("parse font")?,
        })
    }
}

impl Renderer for SystemRenderer {
    fn render(
        &self,
        cpu: Option<f64>,
        gpu: Option<f64>,
        clock: &str,
        rotation: u16,
    ) -> Result<RgbaImage> {
        render_with(&self.font, cpu, gpu, clock, rotation)
    }
}

fn value_text(value: Option<f64>) -> String {
    match value {
        None => "--°C".to_string(),
        Some(value) => format!("{value:.0}°C"),
    }
}

fn color(rgb: [u8; 3]) -> Rgba<u8> {
    Rgba([rgb[0], rgb[1], rgb[2], 255])
}

fn center_text(
    image: &mut RgbaImage,
    font: &impl Font,
    scale: f32,
    center: (i32, i32),
    text: &str,
    rgb: [u8; 3],
) {
    let (width, height) = text_size(scale, font, text);
    let x = center.0 - width as i32 / 2;
    let y = center.1 - height as i32 / 2;
    draw_text_mut(image, color(rgb), x, y, scale, font, text);
}

/// Positive rotation is counterclockwise, matching `Image.rotate(90)` in the
/// verified Python prototype.
pub fn rotate(image: RgbaImage, rotation: u16) -> Result<RgbaImage> {
    Ok(match rotation {
        0 => image,
        90 => image::imageops::rotate270(&image),
        180 => image::imageops::rotate180(&image),
        270 => image::imageops::rotate90(&image),
        other => bail!("Rotation must be 0, 90, 180 or 270 CCW, got {other}"),
    })
}

pub fn render_with(
    font: &impl Font,
    cpu: Option<f64>,
    gpu: Option<f64>,
    clock: &str,
    rotation: u16,
) -> Result<RgbaImage> {
    if !matches!(rotation, 0 | 90 | 180 | 270) {
        bail!("Rotation must be 0, 90, 180 or 270 CCW, got {rotation}");
    }
    let mut image = RgbaImage::from_pixel(WIDTH, HEIGHT, color(BACKGROUND));
    draw_hollow_circle_mut(&mut image, (240, 240), 228, color([0x1d, 0x34, 0x4c]));
    center_text(
        &mut image,
        font,
        24.0,
        (240, 73),
        "LIVE TEMPERATURES",
        [0xb4, 0xc6, 0xda],
    );
    center_text(
        &mut image,
        font,
        28.0,
        (240, 130),
        "CPU",
        [0x4e, 0xd9, 0xee],
    );
    center_text(
        &mut image,
        font,
        78.0,
        (240, 192),
        &value_text(cpu),
        [0xed, 0xf8, 0xff],
    );
    let divider = color([0x23, 0x37, 0x4c]);
    draw_line_segment_mut(&mut image, (100.0, 245.0), (380.0, 245.0), divider);
    draw_line_segment_mut(&mut image, (100.0, 246.0), (380.0, 246.0), divider);
    center_text(
        &mut image,
        font,
        28.0,
        (240, 282),
        "GPU",
        [0x91, 0xda, 0x68],
    );
    center_text(
        &mut image,
        font,
        78.0,
        (240, 344),
        &value_text(gpu),
        [0xed, 0xf8, 0xff],
    );
    center_text(
        &mut image,
        font,
        24.0,
        (240, 411),
        clock,
        [0x81, 0x95, 0xad],
    );
    rotate(image, rotation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer() -> Option<SystemRenderer> {
        match SystemRenderer::load() {
            Ok(renderer) => Some(renderer),
            Err(error) => {
                eprintln!("skipping rendering assertions: {error}");
                None
            }
        }
    }

    #[test]
    fn positive_rotation_is_counterclockwise() {
        let mut image = RgbaImage::from_pixel(4, 3, color(BACKGROUND));
        image.put_pixel(1, 0, Rgba([255, 0, 0, 255]));
        let rotated = rotate(image.clone(), 90).expect("rotate");
        assert_eq!(rotated.dimensions(), (3, 4));
        assert_eq!(rotated.get_pixel(0, 2), &Rgba([255, 0, 0, 255]));

        let base = image.clone();
        let expected = image::imageops::rotate270(&base);
        assert_eq!(
            rotate(base, 90).expect("rotate").into_raw(),
            expected.into_raw()
        );
        assert!(
            rotate(image, 45)
                .unwrap_err()
                .to_string()
                .contains("Rotation")
        );
    }

    #[test]
    fn style_and_ccw_rotation() {
        let Some(renderer) = renderer() else {
            return;
        };
        let font = &renderer.font;
        let upright = render_with(font, Some(55.0), Some(49.0), "12:34:56", 0).expect("render");
        assert_eq!(upright.dimensions(), (WIDTH, HEIGHT));
        assert_eq!(upright.get_pixel(0, 0), &color(BACKGROUND));
        assert_eq!(
            render_with(font, Some(55.0), Some(49.0), "12:34:56", 90)
                .expect("rotated render")
                .into_raw(),
            image::imageops::rotate270(&upright).into_raw()
        );
        assert_ne!(
            render_with(font, None, None, "12:34:56", 0)
                .expect("missing sensors")
                .into_raw(),
            upright.into_raw()
        );
        assert!(render_with(font, Some(55.0), Some(49.0), "now", 45).is_err());
    }
}
