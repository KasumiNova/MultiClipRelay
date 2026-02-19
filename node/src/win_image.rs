#![cfg(windows)]

//! Windows bitmap helpers.
//!
//! We use DIBV5 (32bpp BGRA) for clipboard write, and convert clipboard DIB/DIBV5 to PNG
//! for sending over the wire.

use anyhow::Context;

use std::io::Cursor;

// DIB compression constants (same numeric values as Win32).
const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;

fn le_u16(b: &[u8], off: usize) -> anyhow::Result<u16> {
    b.get(off..off + 2)
        .context("u16 range")
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn le_u32(b: &[u8], off: usize) -> anyhow::Result<u32> {
    b.get(off..off + 4)
        .context("u32 range")
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le_i32(b: &[u8], off: usize) -> anyhow::Result<i32> {
    b.get(off..off + 4)
        .context("i32 range")
        .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn row_stride_padded(width: u32, bpp: u16) -> anyhow::Result<usize> {
    let bits_per_row = (width as usize)
        .checked_mul(bpp as usize)
        .context("stride mul")?;
    let bytes_per_row = (bits_per_row + 7) / 8;
    // Rows are aligned to 4 bytes.
    Ok((bytes_per_row + 3) & !3)
}

/// Parse a DIB/DIBV5 buffer into RGBA8 (top-down).
pub fn dib_to_rgba(dib: &[u8]) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    if dib.len() < 40 {
        anyhow::bail!("DIB too small");
    }

    let header_size = le_u32(dib, 0)? as usize;
    if header_size < 40 || header_size > dib.len() {
        anyhow::bail!("invalid DIB header size: {}", header_size);
    }

    let width_i = le_i32(dib, 4)?;
    let height_i = le_i32(dib, 8)?;
    if width_i == 0 || height_i == 0 {
        anyhow::bail!("invalid DIB dimensions");
    }

    let width = width_i.unsigned_abs();
    let height = height_i.unsigned_abs();

    let planes = le_u16(dib, 12)?;
    if planes != 1 {
        anyhow::bail!("unsupported planes={}", planes);
    }

    let bpp = le_u16(dib, 14)?;
    let compression = le_u32(dib, 16)?;

    // Compute pixel data offset.
    let mut pixel_off = header_size;

    // Special-case: BITMAPINFOHEADER (40) + BI_BITFIELDS => masks follow header.
    if header_size == 40 && compression == BI_BITFIELDS {
        // Usually 3 masks (RGB). Some producers include 4 (RGBA), but it's safe to skip 3.
        pixel_off = pixel_off.saturating_add(12);
        if pixel_off > dib.len() {
            anyhow::bail!("DIB masks out of range");
        }
    }

    if compression != BI_RGB && compression != BI_BITFIELDS {
        anyhow::bail!("unsupported DIB compression={}", compression);
    }

    let stride = row_stride_padded(width, bpp)?;
    let needed = pixel_off
        .checked_add(stride.checked_mul(height as usize).context("stride mul")?)
        .context("pixel buffer size")?;
    if needed > dib.len() {
        anyhow::bail!("DIB pixel buffer out of range: need={} have={}", needed, dib.len());
    }

    let top_down = height_i < 0;

    let mut rgba: Vec<u8> = vec![0u8; (width as usize) * (height as usize) * 4];

    match bpp {
        32 => {
            // Assume BGRA (common for DIBV5 and many producers).
            for y in 0..height as usize {
                let src_y = if top_down { y } else { (height as usize - 1) - y };
                let src_row = pixel_off + src_y * stride;
                let dst_row = y * (width as usize) * 4;
                let src = &dib[src_row..src_row + (width as usize) * 4];
                for x in 0..width as usize {
                    let b = src[x * 4 + 0];
                    let g = src[x * 4 + 1];
                    let r = src[x * 4 + 2];
                    let a = src[x * 4 + 3];
                    rgba[dst_row + x * 4 + 0] = r;
                    rgba[dst_row + x * 4 + 1] = g;
                    rgba[dst_row + x * 4 + 2] = b;
                    rgba[dst_row + x * 4 + 3] = a;
                }
            }
        }
        24 => {
            for y in 0..height as usize {
                let src_y = if top_down { y } else { (height as usize - 1) - y };
                let src_row = pixel_off + src_y * stride;
                let dst_row = y * (width as usize) * 4;
                let src = &dib[src_row..src_row + (width as usize) * 3];
                for x in 0..width as usize {
                    let b = src[x * 3 + 0];
                    let g = src[x * 3 + 1];
                    let r = src[x * 3 + 2];
                    rgba[dst_row + x * 4 + 0] = r;
                    rgba[dst_row + x * 4 + 1] = g;
                    rgba[dst_row + x * 4 + 2] = b;
                    rgba[dst_row + x * 4 + 3] = 255;
                }
            }
        }
        _ => anyhow::bail!("unsupported DIB bitcount={}", bpp),
    }

    Ok((width, height, rgba))
}

pub fn rgba_to_dibv5(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|v| v.checked_mul(4))
        .context("rgba size")?;
    if rgba.len() != expected {
        anyhow::bail!("rgba len mismatch: got={} expected={}", rgba.len(), expected);
    }

    // Build a BITMAPV5HEADER (124 bytes).
    // We store pixels as top-down BGRA (negative height), with BI_BITFIELDS masks.
    let mut hdr = [0u8; 124];
    let w = width as i32;
    let h = -(height as i32);

    // Offsets per BITMAPV5HEADER layout.
    hdr[0..4].copy_from_slice(&(124u32).to_le_bytes());
    hdr[4..8].copy_from_slice(&w.to_le_bytes());
    hdr[8..12].copy_from_slice(&h.to_le_bytes());
    hdr[12..14].copy_from_slice(&(1u16).to_le_bytes());
    hdr[14..16].copy_from_slice(&(32u16).to_le_bytes());
    hdr[16..20].copy_from_slice(&(BI_BITFIELDS as u32).to_le_bytes());

    let img_size = (width as usize)
        .checked_mul(height as usize)
        .and_then(|v| v.checked_mul(4))
        .context("img_size")? as u32;
    hdr[20..24].copy_from_slice(&img_size.to_le_bytes());

    // Color masks for BGRA stored as 0xAARRGGBB u32.
    let red_mask: u32 = 0x00FF0000;
    let green_mask: u32 = 0x0000FF00;
    let blue_mask: u32 = 0x000000FF;
    let alpha_mask: u32 = 0xFF000000;

    hdr[40..44].copy_from_slice(&red_mask.to_le_bytes());
    hdr[44..48].copy_from_slice(&green_mask.to_le_bytes());
    hdr[48..52].copy_from_slice(&blue_mask.to_le_bytes());
    hdr[52..56].copy_from_slice(&alpha_mask.to_le_bytes());

    // Convert RGBA -> BGRA.
    let mut bgra: Vec<u8> = vec![0u8; rgba.len()];
    for i in 0..(rgba.len() / 4) {
        let r = rgba[i * 4 + 0];
        let g = rgba[i * 4 + 1];
        let b = rgba[i * 4 + 2];
        let a = rgba[i * 4 + 3];
        bgra[i * 4 + 0] = b;
        bgra[i * 4 + 1] = g;
        bgra[i * 4 + 2] = r;
        bgra[i * 4 + 3] = a;
    }

    let mut out: Vec<u8> = Vec::with_capacity(hdr.len() + bgra.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(&bgra);
    Ok(out)
}

pub fn dib_to_png(dib: &[u8]) -> anyhow::Result<Vec<u8>> {
    let (w, h, rgba) = dib_to_rgba(dib)?;
    let img = image::RgbaImage::from_raw(w, h, rgba).context("RgbaImage::from_raw")?;
    let dyn_img = image::DynamicImage::ImageRgba8(img);

    let mut out: Vec<u8> = Vec::new();
    dyn_img
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .context("encode png")?;
    Ok(out)
}

pub fn bytes_to_dibv5(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    // Decode common formats (png/jpeg/webp/gif) using image crate.
    let dyn_img = image::load_from_memory(bytes).context("decode image")?;
    let rgba = dyn_img.to_rgba8();
    let (w, h) = rgba.dimensions();
    rgba_to_dibv5(w, h, rgba.as_raw())
}
