//! Fixture builders for tests. Encoded in-process so no binary files are checked in.

use exif::experimental::Writer;
use exif::{Field, In, Tag, Value};
use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, ImageEncoder, Rgba, RgbaImage};

fn encode_png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}

/// 2x1 PNG: red pixel then green pixel.
pub fn png_2x1() -> Vec<u8> {
    let mut img = RgbaImage::new(2, 1);
    img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
    img.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
    encode_png(&img)
}

/// 1x1 PNG holding one straight-alpha RGBA pixel.
pub fn png_pixel(rgba: [u8; 4]) -> Vec<u8> {
    let mut img = RgbaImage::new(1, 1);
    img.put_pixel(0, 0, Rgba(rgba));
    encode_png(&img)
}

/// `w`x`h` PNG of one straight-alpha RGBA color.
pub fn png_solid(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    encode_png(&RgbaImage::from_pixel(w, h, Rgba(rgba)))
}

/// `w`x`h` opaque straight RGBA8 of deterministic per-channel noise (xorshift from `seed`).
pub fn rgba_noise(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for _ in 0..u64::from(w) * u64::from(h) {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let [r, g, b, _] = state.to_le_bytes();
        out.extend_from_slice(&[r, g, b, 255]);
    }
    out
}

/// 16x8 JPEG, left 8 columns red, right 8 columns blue, with an EXIF Orientation APP1
/// segment inserted right after SOI.
pub fn jpeg_16x8_with_orientation(orientation: u16) -> Vec<u8> {
    jpeg_halves(16, 8, orientation, None)
}

/// `w`x`h` JPEG, left half red, right half blue, with an EXIF Orientation APP1 segment
/// and, if given, an ICC profile APP2 segment inserted right after SOI.
pub fn jpeg_halves(w: u32, h: u32, orientation: u16, icc: Option<&[u8]>) -> Vec<u8> {
    let mut img = image::RgbImage::new(w, h);
    for (x, _, p) in img.enumerate_pixels_mut() {
        *p = if x < w / 2 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        };
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();

    let field = Field {
        tag: Tag::Orientation,
        ifd_num: In::PRIMARY,
        value: Value::Short(vec![orientation]),
    };
    let mut writer = Writer::new();
    writer.push_field(&field);
    let mut exif_buf = std::io::Cursor::new(Vec::new());
    writer.write(&mut exif_buf, false).unwrap();
    let exif = exif_buf.into_inner();

    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&((exif.len() + 8) as u16).to_be_bytes());
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&exif);

    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&app1);
    if let Some(icc) = icc {
        out.extend_from_slice(&[0xFF, 0xE2]);
        out.extend_from_slice(&((icc.len() + 16) as u16).to_be_bytes());
        out.extend_from_slice(b"ICC_PROFILE\0\x01\x01");
        out.extend_from_slice(icc);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// 2-frame animated GIF; the second frame has the given delay in ms.
pub fn gif_two_frames(delay_ms: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(Repeat::Infinite).unwrap();
        let a = RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255]));
        let b = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255]));
        enc.encode_frame(Frame::from_parts(
            a,
            0,
            0,
            Delay::from_numer_denom_ms(100, 1),
        ))
        .unwrap();
        enc.encode_frame(Frame::from_parts(
            b,
            0,
            0,
            Delay::from_numer_denom_ms(delay_ms, 1),
        ))
        .unwrap();
    }
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Structurally valid PNG whose IHDR declares `w`x`h` with a tiny IDAT (no real pixel data).
pub fn png_header_only(w: u32, h: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    // zlib stream: header + empty stored block + adler32
    chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x01, 0x01, 0, 0, 0xFF, 0xFF, 0, 0, 0, 1],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Animated GIF with `n` solid `size`x`size` frames of alternating colours.
pub fn gif_n_frames(n: u32, size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        for i in 0..n {
            let c = if i % 2 == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            };
            let img = RgbaImage::from_pixel(size, size, c);
            enc.encode_frame(Frame::from_parts(
                img,
                0,
                0,
                Delay::from_numer_denom_ms(100, 1),
            ))
            .unwrap();
        }
    }
    out
}
