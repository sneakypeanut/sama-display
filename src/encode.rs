//! Observed Rev C full-frame wire encoding.
//!
//! Frames are not PNG on the wire: the header is the fixed `c8ef69000e100e10`
//! command and the BGRA payload is joined in 249-byte groups separated by NUL,
//! then padded to whole 250-byte protocol blocks.

use anyhow::{Result, bail};
use image::RgbaImage;

pub const WIDTH: u32 = 480;
pub const HEIGHT: u32 = 480;
pub const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;
pub const BLOCK: usize = 250;
pub const BODY_GROUP: usize = 249;
pub const HEADER_HEX: &str = "c8ef69000e100e10";

/// Reads a hex string into bytes without pulling in an extra crate.
pub fn unhex(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        bail!("hex string must have an even length");
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks(2) {
        let pair = std::str::from_utf8(pair)?;
        bytes.push(u8::from_str_radix(pair, 16)?);
    }
    Ok(bytes)
}

/// Fixed 250-byte frame header sent after the comma preamble.
pub fn header() -> Vec<u8> {
    let mut header = unhex(HEADER_HEX).expect("constant header hex");
    header.resize(BLOCK, 0);
    header
}

/// Encodes one 480x480 BGRA frame as (header, body) protocol blocks.
pub fn encode_frame(bgra: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if bgra.len() != FRAME_BYTES {
        bail!(
            "expected 480x480 BGRA ({FRAME_BYTES} bytes), got {}",
            bgra.len()
        );
    }
    let groups: Vec<&[u8]> = bgra.chunks(BODY_GROUP).collect();
    let mut body = Vec::with_capacity(bgra.len() + groups.len());
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            body.push(0);
        }
        body.extend_from_slice(group);
    }
    body.resize(body.len().div_ceil(BLOCK) * BLOCK, 0);
    Ok((header(), body))
}

/// Converts the renderer's RGBA buffer into the wire's BGRA order.
pub fn bgra_from_rgba(image: &RgbaImage) -> Vec<u8> {
    let mut bgra = Vec::with_capacity(image.as_raw().len());
    for pixel in image.as_raw().as_chunks::<4>().0 {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    bgra
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn digest(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    #[test]
    fn wire_frame_roundtrip() {
        let raw: Vec<u8> = (0..=255u8).cycle().take(FRAME_BYTES).collect();
        let (header, body) = encode_frame(&raw).expect("encode");
        assert_eq!(
            header,
            unhex(HEADER_HEX)
                .unwrap()
                .into_iter()
                .chain([0u8; 242])
                .collect::<Vec<_>>()
        );
        assert_eq!(body.len() % BLOCK, 0);
        let mut recovered = Vec::with_capacity(raw.len());
        for block in body.chunks(BLOCK) {
            recovered.extend_from_slice(&block[..BODY_GROUP]);
        }
        assert_eq!(&recovered[..raw.len()], &raw[..]);
        assert!(encode_frame(b"bad").is_err());
    }

    #[test]
    fn real_480x480_frame_matches_python_reference_wire() {
        // Golden digests produced by the known-working Python implementation
        // (scripts/sama-display/sama_display.py) for deterministic buffers.
        let cases = [
            (
                "range",
                (0..=255u8).cycle().take(FRAME_BYTES).collect::<Vec<_>>(),
                "e0e28abffd73749334544384f0e472ae852cbdad28153d9a8e8e0329e9bd7e5b",
            ),
            (
                "zeros",
                vec![0u8; FRAME_BYTES],
                "83271a66efe2febbdd4873e9157a9ffd1669731817dc2bfe4e1a4155d713a028",
            ),
            (
                "pattern",
                (0..FRAME_BYTES)
                    .map(|index| (index * 7 % 251) as u8)
                    .collect(),
                "32ad9b0ba4717460b70e31e0db751adaa9f70530976fd649572e698dda06a6ac",
            ),
        ];
        for (name, raw, expected) in cases {
            let (header, body) = encode_frame(&raw).expect("encode");
            assert_eq!(
                digest(&header),
                "d50a8df319ba25b1d437581deeea661baa3c0681656f21e9c756a94cefe9c8e2",
                "{name} header"
            );
            assert_eq!(body.len(), 925_500, "{name} body length");
            assert_eq!(digest(&body), expected, "{name} body digest");
        }
    }

    #[test]
    fn bgra_conversion_swaps_red_and_blue() {
        let image = RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 4]));
        assert_eq!(bgra_from_rgba(&image), vec![3, 2, 1, 4]);
    }
}
