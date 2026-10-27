//! Replays the fuzz seed corpus through the same entry functions the cargo-fuzz targets in
//! `fuzz/` call (ruling R3), on stable, so every `cargo test` checks that no seed panics.
//!
//! The seeds in `testdata/fuzz/<target>/` are generated here, from scratch, by the ignored
//! test `write_fuzz_seeds` (nothing is copied from other files):
//!
//! ```text
//! cargo test -p lv-core --test fuzz_corpus -- --ignored write_fuzz_seeds
//! ```
//!
//! A crash found by fuzzing is fixed and then added to its target's folder by hand, so it
//! is replayed from then on.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use exif::experimental::Writer;
use exif::{Field, In, Rational, Tag, Value};
use image::{ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};
use lv_core::fuzzing;

/// A fuzz target's entry function.
type Entry = fn(&[u8]);
/// A target's seed files: (file name, contents).
type Seeds = Vec<(&'static str, Vec<u8>)>;

/// Every fuzz target and its entry function, as `fuzz/fuzz_targets/<name>.rs` calls it.
const TARGETS: [(&str, Entry); 6] = [
    ("format_decode", fuzzing::format_decode),
    ("exif_info", fuzzing::exif_info),
    ("exif_edit", fuzzing::exif_edit),
    ("heif_edit", fuzzing::heif_edit),
    ("jpeg_markers", fuzzing::jpeg_markers),
    ("ipc_frame", lv_ipc::fuzzing::ipc_frame),
];

fn corpus_dir(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/fuzz")
        .join(target)
}

fn seeds(target: &str) -> Vec<PathBuf> {
    let dir = corpus_dir(target);
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

#[test]
fn fuzz_corpus_replays() {
    for (target, entry) in TARGETS {
        let files = seeds(target);
        assert!(!files.is_empty(), "no seeds for {target}");
        for file in files {
            let data = fs::read(&file).unwrap();
            let run = std::panic::catch_unwind(|| entry(&data));
            assert!(run.is_ok(), "{target}: {} panicked", file.display());
        }
    }
}

fn seed(target: &str, name: &str) -> Vec<u8> {
    fs::read(corpus_dir(target).join(name)).unwrap()
}

/// The seeds are worth mutating only if they get past the parsers' first checks.
#[test]
fn seeds_reach_the_parsers() {
    use lv_core::decode::{decode_bytes, DecodeError, DecodeLimits};
    use lv_core::format::{sniff_format, ImageFormat};
    let limits = DecodeLimits {
        max_pixels: fuzzing::FUZZ_MAX_PIXELS,
    };
    for file in seeds("format_decode") {
        let data = fs::read(&file).unwrap();
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("heif-") {
            assert_eq!(sniff_format(&data), Some(ImageFormat::Heif), "{name}");
            continue;
        }
        if name.contains("truncated") {
            // Past the sniff and the header; the decoder may or may not accept the rest.
            assert!(!matches!(
                decode_bytes(&data, &limits),
                Err(DecodeError::Unsupported)
            ));
            continue;
        }
        let image =
            decode_bytes(&data, &limits).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        if name.starts_with("gif-") || name.contains("animated") {
            assert!(image.frames.len() >= 2, "{name}");
        }
        let scaled = file.to_string_lossy().contains("scaled");
        assert_eq!(image.scaled_from.is_some(), scaled, "{}", file.display());
    }
    for file in seeds("exif_info") {
        let data = fs::read(&file).unwrap();
        let found = if file.extension().is_some_and(|e| e == "bin") {
            exif::Reader::new().read_raw(data).is_ok()
        } else {
            lv_core::exif_info::read_exif(&data).is_some()
        };
        assert!(found, "{}", file.display());
    }
    for file in seeds("exif_edit") {
        let data = fs::read(&file).unwrap();
        assert!(lv_core::exif_edit::set_orientation(&data[3..], 6).is_ok());
    }
    for file in seeds("heif_edit") {
        let data = fs::read(&file).unwrap();
        let out = lv_core::heif_edit::set_rotation(&data[1..], data[0])
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(lv_core::heif_edit::read_rotation(&out), Ok(data[0]));
    }
    assert_eq!(
        lv_core::heif_edit::read_rotation(&seed("heif_edit", "irot-q3.bin")[1..]),
        Ok(1)
    );
    for file in seeds("jpeg_markers") {
        assert!(lv_core::jpeg_check::inspect(&fs::read(&file).unwrap()).is_ok());
    }
    for name in ["requests.bin", "responses.bin"] {
        let data = seed("ipc_frame", name);
        let mut r = Cursor::new(&data[..]);
        let mut count = 0;
        loop {
            let frame = if name == "requests.bin" {
                lv_ipc::read_request(&mut r, 1 << 20).map(|f| f.is_some())
            } else {
                lv_ipc::read_response(&mut r, 1 << 20).map(|f| f.is_some())
            };
            if !frame.unwrap() {
                break;
            }
            count += 1;
        }
        assert!(count >= 3, "{name}");
    }
}

// ---------------------------------------------------------------------------------------
// Seed generation.

#[test]
#[ignore = "rewrites testdata/fuzz; run by hand"]
fn write_fuzz_seeds() {
    let sets: [(&str, Seeds); 6] = [
        ("format_decode", format_decode_seeds()),
        ("exif_info", exif_info_seeds()),
        ("exif_edit", exif_edit_seeds()),
        ("heif_edit", heif_edit_seeds()),
        ("jpeg_markers", jpeg_markers_seeds()),
        ("ipc_frame", ipc_frame_seeds()),
    ];
    for (target, files) in sets {
        let dir = corpus_dir(target);
        fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in files {
            fs::write(dir.join(name), bytes).unwrap();
        }
    }
}

/// Left half red, right half blue, with a vertical green ramp.
fn rgb(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let g = (y * 255 / h.max(1)) as u8;
        if x < w / 2 {
            Rgb([255, g, 0])
        } else {
            Rgb([0, g, 255])
        }
    })
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .write_image(rgb(w, h).as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = RgbaImage::from_fn(w, h, |x, y| Rgba([x as u8 * 40, y as u8 * 40, 128, 200]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn gif(w: u32, h: u32) -> Vec<u8> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame};
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(Repeat::Infinite).unwrap();
        for (i, ms) in [(0u8, 50u32), (1, 120)] {
            let img = RgbaImage::from_pixel(w, h, Rgba([200 * i, 100, 50, 255]));
            let delay = Delay::from_numer_denom_ms(ms, 1);
            enc.encode_frame(Frame::from_parts(img, 0, 0, delay))
                .unwrap();
        }
    }
    out
}

fn webp(w: u32, h: u32) -> Vec<u8> {
    let img = RgbaImage::from_fn(w, h, |x, y| Rgba([x as u8 * 30, y as u8 * 30, 90, 255]));
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn bmp(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut out)
        .write_image(rgb(w, h).as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

/// A 16x8 grayscale progressive JPEG (SOF2), built by hand since the encoders here write
/// only baseline: a DC-first scan and an AC-first scan over two blocks, quantization all 1.
/// Block 1 has DC 5 and AC[1] 3; block 2 has DC 5 and no AC.
fn progressive_jpeg() -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    let mut dqt = vec![0x00];
    dqt.extend([1u8; 64]);
    out.extend(segment(0xDB, &dqt));
    // 8-bit, height 8, width 16, one component (id 1, 1x1 sampling, table 0).
    out.extend(segment(0xC2, &[8, 0, 8, 0, 16, 1, 1, 0x11, 0]));
    // Each table: two 2-bit codes, 00 and 01.
    let table = |class_id: u8, symbols: [u8; 2]| {
        let mut p = vec![class_id, 0, 2];
        p.extend([0u8; 14]);
        p.extend(symbols);
        segment(0xC4, &p)
    };
    out.extend(table(0x00, [0, 3])); // DC: categories 0 and 3
    out.extend(table(0x10, [0x00, 0x02])); // AC: EOB and (run 0, size 2)
                                           // DC first: block 1 category 3 (01) + 101, block 2 difference 0 (00), pad with 1s.
    out.extend(segment(0xDA, &[1, 1, 0x00, 0, 0, 0]));
    out.push(0b0110_1001);
    // AC first, coefficients 1..=63: block 1 (0,2) (01) + 11, EOB (00); block 2 EOB (00).
    out.extend(segment(0xDA, &[1, 1, 0x00, 1, 63, 0]));
    out.push(0b0111_0000);
    out.extend([0xFF, 0xD9]);
    out
}

/// RIFF chunks of a WebP file after the `WEBP` tag: (fourcc, payload).
fn webp_chunks(file: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    let mut chunks = Vec::new();
    let mut at = 12;
    while at + 8 <= file.len() {
        let fourcc: [u8; 4] = file[at..at + 4].try_into().unwrap();
        let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        chunks.push((fourcc, file[at + 8..at + 8 + len].to_vec()));
        at += 8 + len + (len & 1);
    }
    chunks
}

fn riff_chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut c = fourcc.to_vec();
    c.extend((payload.len() as u32).to_le_bytes());
    c.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        c.push(0);
    }
    c
}

fn u24(v: u32) -> [u8; 3] {
    let b = v.to_le_bytes();
    [b[0], b[1], b[2]]
}

/// A two-frame animated WebP (`VP8X` + `ANIM` + two `ANMF`), each frame the lossless
/// bitstream the encoder writes for a still image.
fn animated_webp(w: u32, h: u32) -> Vec<u8> {
    let frame = |seed: u8| {
        let img = RgbaImage::from_fn(w, h, |x, y| {
            Rgba([(x as u8 * 30) ^ seed, y as u8 * 30, 90, 255])
        });
        let mut out = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        let (_, vp8l) = webp_chunks(&out)
            .into_iter()
            .find(|(fourcc, _)| fourcc == b"VP8L")
            .expect("lossless WebP has a VP8L chunk");
        vp8l
    };
    let mut vp8x = vec![0x12, 0, 0, 0]; // animation + alpha
    vp8x.extend(u24(w - 1));
    vp8x.extend(u24(h - 1));
    let mut body = b"WEBP".to_vec();
    body.extend(riff_chunk(b"VP8X", &vp8x));
    body.extend(riff_chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
    for (seed, ms) in [(0u8, 80u32), (100, 150)] {
        let mut anmf = Vec::new();
        anmf.extend(u24(0)); // x / 2
        anmf.extend(u24(0)); // y / 2
        anmf.extend(u24(w - 1));
        anmf.extend(u24(h - 1));
        anmf.extend(u24(ms));
        anmf.push(0); // alpha-blend, no dispose
        anmf.extend(riff_chunk(b"VP8L", &frame(seed)));
        body.extend(riff_chunk(b"ANMF", &anmf));
    }
    let mut out = b"RIFF".to_vec();
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

/// CRC-32 (ISO-HDLC) as PNG chunks use it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut body = typ.to_vec();
    body.extend_from_slice(payload);
    let mut c = (payload.len() as u32).to_be_bytes().to_vec();
    c.extend_from_slice(&body);
    c.extend(crc32(&body).to_be_bytes());
    c
}

/// A two-frame APNG: the PNG encoder's `IDAT` as the default image and first frame, and
/// the same data again as an `fdAT` second frame.
fn apng(w: u32, h: u32) -> Vec<u8> {
    let still = png(w, h);
    let mut at = 8;
    let (mut ihdr, mut idat) = (Vec::new(), Vec::new());
    while at + 8 <= still.len() {
        let len = u32::from_be_bytes(still[at..at + 4].try_into().unwrap()) as usize;
        let payload = &still[at + 8..at + 8 + len];
        match &still[at + 4..at + 8] {
            b"IHDR" => ihdr = payload.to_vec(),
            b"IDAT" => idat.extend_from_slice(payload),
            _ => {}
        }
        at += 12 + len;
    }
    let fctl = |seq: u32, delay: u16| {
        let mut p = seq.to_be_bytes().to_vec();
        p.extend(w.to_be_bytes());
        p.extend(h.to_be_bytes());
        p.extend([0u8; 8]); // x and y offsets
        p.extend(delay.to_be_bytes());
        p.extend(10u16.to_be_bytes()); // delay in tenths of a second
        p.extend([0, 0]); // dispose none, blend source
        png_chunk(b"fcTL", &p)
    };
    let mut out = still[..8].to_vec();
    out.extend(png_chunk(b"IHDR", &ihdr));
    let mut actl = 2u32.to_be_bytes().to_vec();
    actl.extend(0u32.to_be_bytes()); // loop forever
    out.extend(png_chunk(b"acTL", &actl));
    out.extend(fctl(0, 1));
    out.extend(png_chunk(b"IDAT", &idat));
    out.extend(fctl(1, 3));
    let mut fdat = 2u32.to_be_bytes().to_vec();
    fdat.extend(&idat);
    out.extend(png_chunk(b"fdAT", &fdat));
    out.extend(png_chunk(b"IEND", &[]));
    out
}

/// A TIFF block (as in EXIF) with these fields.
fn tiff(fields: &[Field], big_endian: bool) -> Vec<u8> {
    let mut writer = Writer::new();
    for field in fields {
        writer.push_field(field);
    }
    let mut buf = Cursor::new(Vec::new());
    writer.write(&mut buf, !big_endian).unwrap();
    buf.into_inner()
}

fn field(tag: Tag, ifd: In, value: Value) -> Field {
    Field {
        tag,
        ifd_num: ifd,
        value,
    }
}

fn orientation(o: u16) -> Field {
    field(Tag::Orientation, In::PRIMARY, Value::Short(vec![o]))
}

/// A camera-like field set: IFD0, the Exif IFD (with pixel dimensions) and GPS.
fn camera_fields(o: u16) -> Vec<Field> {
    vec![
        orientation(o),
        field(Tag::Make, In::PRIMARY, Value::Ascii(vec![b"Fuzz".to_vec()])),
        field(
            Tag::Model,
            In::PRIMARY,
            Value::Ascii(vec![b"Seed 1".to_vec()]),
        ),
        field(
            Tag::DateTimeOriginal,
            In::PRIMARY,
            Value::Ascii(vec![b"2026:10:27 12:34:56".to_vec()]),
        ),
        field(
            Tag::ExposureTime,
            In::PRIMARY,
            Value::Rational(vec![Rational { num: 1, denom: 125 }]),
        ),
        field(
            Tag::FNumber,
            In::PRIMARY,
            Value::Rational(vec![Rational { num: 28, denom: 10 }]),
        ),
        field(Tag::PixelXDimension, In::PRIMARY, Value::Long(vec![64])),
        field(Tag::PixelYDimension, In::PRIMARY, Value::Short(vec![32])),
        field(
            Tag::GPSLatitudeRef,
            In::PRIMARY,
            Value::Ascii(vec![b"N".to_vec()]),
        ),
        field(
            Tag::GPSLatitude,
            In::PRIMARY,
            Value::Rational(vec![
                Rational { num: 37, denom: 1 },
                Rational { num: 33, denom: 1 },
                Rational {
                    num: 1234,
                    denom: 100,
                },
            ]),
        ),
        field(Tag::Compression, In::THUMBNAIL, Value::Short(vec![6])),
    ]
}

fn segment(code: u8, payload: &[u8]) -> Vec<u8> {
    let mut s = vec![0xFF, code];
    s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    s.extend_from_slice(payload);
    s
}

/// `jpeg` with these segments inserted right after SOI.
fn with_segments(jpeg: &[u8], segments: &[Vec<u8>]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    for s in segments {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn exif_app1(tiff: &[u8]) -> Vec<u8> {
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(tiff);
    segment(0xE1, &payload)
}

fn icc_app2() -> Vec<u8> {
    let icc = include_bytes!("../../../testdata/icc/sRGB-compact.icc");
    let mut payload = b"ICC_PROFILE\0\x01\x01".to_vec();
    payload.extend_from_slice(icc);
    segment(0xE2, &payload)
}

/// A PNG with an `eXIf` chunk before IEND.
fn png_with_exif(tiff: &[u8]) -> Vec<u8> {
    let png = png(4, 4);
    let iend = png.len() - 12;
    let mut out = png[..iend].to_vec();
    out.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
    out.extend_from_slice(b"eXIf");
    out.extend_from_slice(tiff);
    // The CRC is not checked by the EXIF reader; a fixed value keeps the seed simple.
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&png[iend..]);
    out
}

// --- HEIF: box structure only (no coded image), enough for the box editor and reader. ---

fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    b.extend_from_slice(typ);
    b.extend_from_slice(payload);
    b
}

fn full(typ: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = (u32::from(version) << 24).to_be_bytes().to_vec();
    p.extend_from_slice(payload);
    bx(typ, &p)
}

struct HeifItem {
    id: u16,
    typ: [u8; 4],
    data: Vec<u8>,
    /// Indices (1-based) into the property list, all essential.
    props: Vec<u8>,
}

/// A HEIF file: `ftyp`, `meta` (`hdlr`, `pitm` 1, `iloc` v0, `iinf`, `iprp`, optional
/// `iref`) and `mdat` holding every item's data in one extent each.
fn heif(items: &[HeifItem], props: &[Vec<u8>], refs: &[([u8; 4], u16, u16)]) -> Vec<u8> {
    let build = |mdat_body: usize| {
        let mut hdlr = vec![0; 4];
        hdlr.extend_from_slice(b"pict");
        hdlr.extend_from_slice(&[0; 13]);
        let mut iinf = (items.len() as u16).to_be_bytes().to_vec();
        let mut iloc = vec![0x44, 0x00];
        iloc.extend((items.len() as u16).to_be_bytes());
        let mut mdat = Vec::new();
        let mut ipma = Vec::new();
        let mut ipma_count = 0u32;
        for item in items {
            let mut infe = item.id.to_be_bytes().to_vec();
            infe.extend_from_slice(&[0, 0]);
            infe.extend_from_slice(&item.typ);
            infe.push(0);
            iinf.extend(full(b"infe", 2, &infe));
            iloc.extend(item.id.to_be_bytes());
            iloc.extend([0, 0]); // data reference index
            iloc.extend(1u16.to_be_bytes());
            iloc.extend(((mdat_body + mdat.len()) as u32).to_be_bytes());
            iloc.extend((item.data.len() as u32).to_be_bytes());
            mdat.extend_from_slice(&item.data);
            if !item.props.is_empty() {
                ipma_count += 1;
                ipma.extend(item.id.to_be_bytes());
                ipma.push(item.props.len() as u8);
                ipma.extend(item.props.iter().map(|i| 0x80 | i));
            }
        }
        let mut ipma_box = ipma_count.to_be_bytes().to_vec();
        ipma_box.extend(ipma);
        let mut iprp = bx(b"ipco", &props.concat());
        iprp.extend(full(b"ipma", 0, &ipma_box));

        let mut meta = vec![0; 4];
        meta.extend(full(b"hdlr", 0, &hdlr));
        meta.extend(full(b"pitm", 0, &1u16.to_be_bytes()));
        meta.extend(full(b"iloc", 0, &iloc));
        meta.extend(full(b"iinf", 0, &iinf));
        meta.extend(bx(b"iprp", &iprp));
        if !refs.is_empty() {
            let mut iref = Vec::new();
            for (typ, from, to) in refs {
                let mut r = from.to_be_bytes().to_vec();
                r.extend(1u16.to_be_bytes());
                r.extend(to.to_be_bytes());
                iref.extend(bx(typ, &r));
            }
            meta.extend(full(b"iref", 0, &iref));
        }
        let mut file = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        file.extend(bx(b"meta", &meta));
        file.extend(bx(b"mdat", &mdat));
        file
    };
    // The layout does not depend on the offsets: build once to find where `mdat` starts.
    let first = build(0);
    let mdat_body = first.len() - items.iter().map(|i| i.data.len()).sum::<usize>();
    build(mdat_body)
}

fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut p = w.to_be_bytes().to_vec();
    p.extend(h.to_be_bytes());
    full(b"ispe", 0, &p)
}

fn coded(id: u16, props: Vec<u8>) -> HeifItem {
    HeifItem {
        id,
        typ: *b"hvc1",
        data: b"coded-image-bytes".to_vec(),
        props,
    }
}

fn exif_item(id: u16, o: u16) -> HeifItem {
    let mut data = 0u32.to_be_bytes().to_vec();
    data.extend(tiff(&camera_fields(o), true));
    HeifItem {
        id,
        typ: *b"Exif",
        data,
        props: vec![],
    }
}

/// [`heif_plain`] with a `mif1` major brand, so sniffing reads the compatible brands.
fn heif_mif1() -> Vec<u8> {
    let plain = heif_plain();
    let ftyp_len = u32::from_be_bytes(plain[..4].try_into().unwrap()) as usize;
    let mut out = bx(b"ftyp", b"mif1\0\0\0\0mif1heic");
    out.extend_from_slice(&plain[ftyp_len..]);
    out
}

fn heif_plain() -> Vec<u8> {
    heif(&[coded(1, vec![1])], &[ispe(64, 48)], &[])
}

fn heif_irot() -> Vec<u8> {
    heif(
        &[coded(1, vec![1, 2])],
        &[ispe(64, 48), bx(b"irot", &[1])],
        &[],
    )
}

fn heif_exif_thumb() -> Vec<u8> {
    heif(
        &[coded(1, vec![1]), coded(2, vec![2]), exif_item(3, 6)],
        &[ispe(64, 48), ispe(16, 12)],
        &[(*b"thmb", 2, 1), (*b"cdsc", 3, 1)],
    )
}

// --- Per-target seed sets. ---

fn format_decode_seeds() -> Seeds {
    let small = jpeg(64, 32);
    vec![
        ("jpeg-64x32.jpg", small.clone()),
        (
            "jpeg-exif6-icc.jpg",
            with_segments(
                &small,
                &[exif_app1(&tiff(&[orientation(6)], false)), icc_app2()],
            ),
        ),
        // Over the entry's pixel limit: opens at reduced size.
        ("jpeg-512x256-scaled.jpg", jpeg(512, 256)),
        ("png-5x3.png", png(5, 3)),
        ("gif-2frames.gif", gif(6, 4)),
        ("webp-lossless.webp", webp(7, 5)),
        ("bmp-9x4.bmp", bmp(9, 4)),
        ("jpeg-progressive-16x8.jpg", progressive_jpeg()),
        // Cut inside the scan data: decoders may fill the rest or refuse it.
        ("jpeg-truncated.jpg", small[..small.len() * 3 / 5].to_vec()),
        ("webp-animated.webp", animated_webp(7, 5)),
        ("png-apng-2frames.png", apng(5, 3)),
        // Sniffed as HEIF (major brand `mif1`, `heic` among the compatible brands), which
        // this decoder leaves to the worker.
        ("heif-sniff-mif1.heic", heif_mif1()),
    ]
}

fn exif_info_seeds() -> Seeds {
    let camera_le = tiff(&camera_fields(3), false);
    let camera_be = tiff(&camera_fields(8), true);
    vec![
        (
            "jpeg-camera.jpg",
            with_segments(&jpeg(16, 8), &[exif_app1(&camera_le)]),
        ),
        ("png-exif.png", png_with_exif(&camera_be)),
        ("heif-exif.heic", heif_exif_thumb()),
        ("tiff-raw.bin", camera_le),
    ]
}

fn exif_edit_seeds() -> Seeds {
    // The entry takes its parameters from the first bytes (see `lv_core::fuzzing`).
    let with_params = |params: &[u8], jpeg: Vec<u8>| {
        let mut v = params.to_vec();
        v.extend(jpeg);
        v
    };
    let base = jpeg(16, 8);
    let app0 = segment(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    vec![
        ("no-exif.bin", with_params(&[6, 8, 16], base.clone())),
        (
            "exif-o3.bin",
            with_params(
                &[1, 16, 8],
                with_segments(&base, &[exif_app1(&tiff(&[orientation(3)], false))]),
            ),
        ),
        (
            "app0-camera-be.bin",
            with_params(
                &[8, 8, 16],
                with_segments(&base, &[app0, exif_app1(&tiff(&camera_fields(5), true))]),
            ),
        ),
    ]
}

fn heif_edit_seeds() -> Seeds {
    let with_quarters = |q: u8, heif: Vec<u8>| {
        let mut v = vec![q];
        v.extend(heif);
        v
    };
    vec![
        ("plain-q1.bin", with_quarters(1, heif_plain())),
        ("irot-q3.bin", with_quarters(3, heif_irot())),
        ("exif-thumb-q2.bin", with_quarters(2, heif_exif_thumb())),
    ]
}

fn jpeg_markers_seeds() -> Seeds {
    let base = jpeg(24, 16);
    // Fill bytes before a marker and a COM segment.
    let mut filled = base[..2].to_vec();
    filled.extend_from_slice(&[0xFF, 0xFF]);
    filled.extend(segment(0xFE, b"comment"));
    filled.extend_from_slice(&base[2..]);
    // Data after EOI, as an MPF file or a trailer would have.
    let mut trailing = base.clone();
    trailing.extend_from_slice(b"trailer after EOI");
    vec![
        ("baseline.jpg", base),
        ("fill-and-comment.jpg", filled),
        ("trailing.jpg", trailing),
        ("icc.jpg", with_segments(&jpeg(8, 8), &[icc_app2()])),
    ]
}

fn ipc_frame_seeds() -> Seeds {
    use lv_ipc::{write_request, write_response, FailKind, Request, Response};
    let requests = [
        Request::DecodeHeif {
            id: 1,
            max_pixels: 1 << 20,
            data: b"heif-bytes".to_vec(),
        },
        Request::RotateJpeg {
            id: 2,
            quarters: 3,
            max_pixels: 1 << 20,
            data: b"jpeg-bytes".to_vec(),
        },
        Request::Shutdown,
    ];
    let responses = [
        Response::Image {
            id: 1,
            width: 2,
            height: 1,
            bgra: vec![1, 2, 3, 255, 4, 5, 6, 255],
        },
        Response::Failed {
            id: 2,
            kind: FailKind::TooLarge {
                width: 30000,
                height: 20000,
            },
        },
        Response::Failed {
            id: 3,
            kind: FailKind::Corrupt("bad box".into()),
        },
        Response::Failed {
            id: 4,
            kind: FailKind::Unsupported,
        },
        Response::Failed {
            id: 5,
            kind: FailKind::NotPerfect,
        },
        Response::Failed {
            id: 6,
            kind: FailKind::HasTrailingData,
        },
        Response::Bytes {
            id: 7,
            data: b"rotated".to_vec(),
        },
    ];
    let mut req_stream = Vec::new();
    for r in &requests {
        write_request(&mut req_stream, r).unwrap();
    }
    let mut resp_stream = Vec::new();
    for r in &responses {
        write_response(&mut resp_stream, r).unwrap();
    }
    let mut first = Vec::new();
    write_request(&mut first, &requests[1]).unwrap();
    vec![
        ("requests.bin", req_stream),
        ("responses.bin", resp_stream),
        ("rotate.bin", first),
    ]
}
