//! VNC's Tight encoding (RFB encoding 7), without JPEG.
//!
//! Linecard never asks for lossy quality (the quality pseudo-encodings), so a
//! server keeps to the lossless forms: a solid fill, or pixels sent plain, as
//! a palette or as gradient differences, each optionally zlib-compressed.
//! This is the encoding TightVNC (common on Windows) speaks instead of ZRLE.
//!
//! Pixels are TPIXELs: three bytes, red, green, blue (the pixel format the
//! client asks for has 8 bits per colour).

use std::io;

use flate2::Decompress;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::vnc::inflate;

/// The four zlib streams a connection shares across rectangles; the server
/// says which to restart.
pub struct Streams([Decompress; 4]);

impl Default for Streams {
    fn default() -> Self {
        Streams(std::array::from_fn(|_| Decompress::new(true)))
    }
}

fn fail(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// A length in one to three bytes, seven bits each, low bits first.
async fn compact<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<usize> {
    let a = r.read_u8().await?;
    let mut n = usize::from(a & 0x7f);
    if a & 0x80 != 0 {
        let b = r.read_u8().await?;
        n |= usize::from(b & 0x7f) << 7;
        if b & 0x80 != 0 {
            n |= usize::from(r.read_u8().await?) << 14;
        }
    }
    Ok(n)
}

/// Read one rectangle of `w`×`h` pixels and return it as RGBA.
pub async fn read<R: AsyncRead + Unpin>(r: &mut R, w: u16, h: u16, streams: &mut Streams) -> io::Result<Vec<u8>> {
    let (w, h) = (usize::from(w), usize::from(h));
    let ctrl = r.read_u8().await?;
    for (i, z) in streams.0.iter_mut().enumerate() {
        if ctrl & (1 << i) != 0 {
            *z = Decompress::new(true);
        }
    }
    match ctrl >> 4 {
        8 => {
            let mut c = [0u8; 3];
            r.read_exact(&mut c).await?;
            Ok([c[0], c[1], c[2], 255].repeat(w * h))
        }
        9 => Err(fail("the server sent a JPEG rectangle, which Linecard never asks for")),
        kind @ 0..=7 => {
            let stream = usize::from(kind & 3);
            let filter = if kind & 4 != 0 { r.read_u8().await? } else { 0 };
            match filter {
                0 => {
                    let raw = body(r, streams, stream, w * h * 3).await?;
                    Ok(rgb_to_rgba(&raw))
                }
                1 => {
                    let colors = usize::from(r.read_u8().await?) + 1;
                    let mut palette = vec![0u8; colors * 3];
                    r.read_exact(&mut palette).await?;
                    let two = colors == 2;
                    let size = if two { w.div_ceil(8) * h } else { w * h };
                    let raw = body(r, streams, stream, size).await?;
                    let mut out = Vec::with_capacity(w * h * 4);
                    for y in 0..h {
                        for x in 0..w {
                            let i = if two {
                                usize::from(raw[y * w.div_ceil(8) + x / 8] >> (7 - x % 8) & 1)
                            } else {
                                usize::from(raw[y * w + x])
                            };
                            let c = palette
                                .get(i * 3..i * 3 + 3)
                                .ok_or_else(|| fail("a palette index outside the palette"))?;
                            out.extend([c[0], c[1], c[2], 255]);
                        }
                    }
                    Ok(out)
                }
                2 => {
                    let mut raw = body(r, streams, stream, w * h * 3).await?;
                    gradient(&mut raw, w, h);
                    Ok(rgb_to_rgba(&raw))
                }
                other => Err(fail(format!("an unknown Tight filter ({other})"))),
            }
        }
        _ => Err(fail("a Tight rectangle of an unknown kind")),
    }
}

/// `size` bytes of pixel data: under 12 are sent as they are, more come
/// zlib-compressed on `stream`.
async fn body<R: AsyncRead + Unpin>(
    r: &mut R,
    streams: &mut Streams,
    stream: usize,
    size: usize,
) -> io::Result<Vec<u8>> {
    if size < 12 {
        let mut raw = vec![0u8; size];
        r.read_exact(&mut raw).await?;
        return Ok(raw);
    }
    let len = compact(r).await?;
    if len > size * 2 + 1024 {
        return Err(fail("a Tight rectangle claims more data than it can hold"));
    }
    let mut data = vec![0u8; len];
    r.read_exact(&mut data).await?;
    let raw = inflate(&mut streams.0[stream], &data)?;
    if raw.len() != size {
        return Err(fail("a Tight rectangle decompressed to the wrong size"));
    }
    Ok(raw)
}

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for p in rgb.as_chunks::<3>().0 {
        out.extend([p[0], p[1], p[2], 255]);
    }
    out
}

/// Undo the gradient filter in place: each value was sent as its difference
/// from left + above - upper-left (missing neighbours count as 0), clamped.
fn gradient(rgb: &mut [u8], w: usize, h: usize) {
    let at = |x: usize, y: usize, c: usize| (y * w + x) * 3 + c;
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let left = if x > 0 { i32::from(rgb[at(x - 1, y, c)]) } else { 0 };
                let above = if y > 0 { i32::from(rgb[at(x, y - 1, c)]) } else { 0 };
                let corner = if x > 0 && y > 0 {
                    i32::from(rgb[at(x - 1, y - 1, c)])
                } else {
                    0
                };
                let predicted = (left + above - corner).clamp(0, 255);
                let i = at(x, y, c);
                rgb[i] = (i32::from(rgb[i]) + predicted) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::write::ZlibEncoder;
    use flate2::Compression;

    use super::*;

    async fn decode(bytes: &[u8], w: u16, h: u16, streams: &mut Streams) -> io::Result<Vec<u8>> {
        let mut r = bytes;
        let out = read(&mut r, w, h, streams).await?;
        assert!(r.is_empty(), "the whole rectangle was read");
        Ok(out)
    }

    #[tokio::test]
    async fn fill_is_one_colour() {
        let out = decode(&[0x80, 10, 20, 30], 3, 2, &mut Streams::default())
            .await
            .unwrap();
        assert_eq!(out, [10, 20, 30, 255].repeat(6));
    }

    #[tokio::test]
    async fn short_plain_pixels_are_sent_as_they_are() {
        // 2×1 pixels = 6 bytes, under the 12-byte compression threshold.
        let out = decode(&[0x00, 1, 2, 3, 4, 5, 6], 2, 1, &mut Streams::default())
            .await
            .unwrap();
        assert_eq!(out, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[tokio::test]
    async fn two_colour_palette_is_one_bit_per_pixel_with_padded_rows() {
        // 10×2: rows of 2 bytes. Red and blue; bits 1 mean the second colour.
        let mut msg = vec![0x40, 1, 1, 255, 0, 0, 0, 0, 255];
        msg.extend([0b1010_1010, 0b1100_0000]); // row 0: columns 0,2,4,6,8 are blue... (bit set = blue)
        msg.extend([0b0000_0000, 0b0000_0000]); // row 1: all red
        let out = decode(&msg, 10, 2, &mut Streams::default()).await.unwrap();
        let px = |x: usize, y: usize| &out[(y * 10 + x) * 4..(y * 10 + x) * 4 + 3];
        assert_eq!(px(0, 0), [0, 0, 255], "first bit set: second colour");
        assert_eq!(px(1, 0), [255, 0, 0]);
        assert_eq!(px(8, 0), [0, 0, 255], "the second byte is read for columns 8 and 9");
        assert_eq!(px(9, 0), [0, 0, 255]);
        assert_eq!(px(9, 1), [255, 0, 0]);
    }

    #[tokio::test]
    async fn larger_palettes_use_a_byte_per_pixel() {
        let mut msg = vec![0x40, 1, 2, 10, 0, 0, 0, 20, 0, 0, 0, 30]; // three colours
        msg.extend([2, 1, 0, 2]); // 4×1 pixels, under 12 bytes: sent plain
        let out = decode(&msg, 4, 1, &mut Streams::default()).await.unwrap();
        let px: Vec<[u8; 3]> = out.chunks(4).map(|p| [p[0], p[1], p[2]]).collect();
        assert_eq!(px, [[0, 0, 30], [0, 20, 0], [10, 0, 0], [0, 0, 30]]);
    }

    fn zlib(data: &[u8], z: &mut ZlibEncoder<Vec<u8>>) -> Vec<u8> {
        z.write_all(data).unwrap();
        z.flush().unwrap();
        std::mem::take(z.get_mut())
    }

    fn compact_len(mut n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let low = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(low);
                return out;
            }
            out.push(low | 0x80);
        }
    }

    #[tokio::test]
    async fn compressed_pixels_continue_one_zlib_stream_and_can_restart_it() {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        let mut streams = Streams::default();
        let a: Vec<u8> = (0..4 * 4 * 3).map(|i| i as u8).collect();
        let b: Vec<u8> = (0..4 * 4 * 3).map(|i| 200 - i as u8).collect();
        for pixels in [&a, &b] {
            let z = zlib(pixels, &mut enc);
            let mut msg = vec![0x00];
            msg.extend(compact_len(z.len()));
            msg.extend(z);
            let out = decode(&msg, 4, 4, &mut streams).await.unwrap();
            assert_eq!(out, rgb_to_rgba(pixels));
        }
        // Restart stream 0 (control bit 0) and begin a new compressor.
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        let z = zlib(&a, &mut enc);
        let mut msg = vec![0x01];
        msg.extend(compact_len(z.len()));
        msg.extend(z);
        assert_eq!(decode(&msg, 4, 4, &mut streams).await.unwrap(), rgb_to_rgba(&a));
    }

    #[tokio::test]
    async fn gradient_rebuilds_a_ramp_from_its_differences() {
        // A 4×4 ramp where value = 10*x + 5*y in every channel; the
        // differences from left + above - corner are 0 except the first
        // row and column (which only have some neighbours).
        let (w, h) = (4usize, 4usize);
        let mut diffs = Vec::new();
        let value = |x: usize, y: usize| (10 * x + 5 * y) as i32;
        for y in 0..h {
            for x in 0..w {
                let left = if x > 0 { value(x - 1, y) } else { 0 };
                let above = if y > 0 { value(x, y - 1) } else { 0 };
                let corner = if x > 0 && y > 0 { value(x - 1, y - 1) } else { 0 };
                let predicted = (left + above - corner).clamp(0, 255);
                let d = (value(x, y) - predicted) as u8;
                diffs.extend([d, d, d]);
            }
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        let z = zlib(&diffs, &mut enc);
        // Stream 1, explicit filter 2 (gradient): control 0x50 = 0b0101_0000.
        let mut msg = vec![0x50, 2];
        msg.extend(compact_len(z.len()));
        msg.extend(z);
        let out = decode(&msg, w as u16, h as u16, &mut Streams::default()).await.unwrap();
        for y in 0..h {
            for x in 0..w {
                let p = &out[(y * w + x) * 4..(y * w + x) * 4 + 4];
                assert_eq!(
                    p,
                    [value(x, y) as u8, value(x, y) as u8, value(x, y) as u8, 255],
                    "at {x},{y}"
                );
            }
        }
    }

    #[tokio::test]
    async fn jpeg_and_nonsense_are_refused() {
        assert!(decode(&[0x90], 1, 1, &mut Streams::default()).await.is_err());
        assert!(decode(&[0xa0], 1, 1, &mut Streams::default()).await.is_err());
        assert!(decode(&[0x40, 7], 1, 1, &mut Streams::default()).await.is_err());
    }

    #[tokio::test]
    async fn a_lying_length_is_refused() {
        // 4×4 plain pixels are 48 bytes; claim 100 KB of compressed data.
        let mut msg = vec![0x00];
        msg.extend(compact_len(100_000));
        assert!(decode(&msg, 4, 4, &mut Streams::default()).await.is_err());
    }
}
