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
    use lv_core::decode::{decode_bytes, DecodeLimits};
    let limits = DecodeLimits {
        max_pixels: fuzzing::FUZZ_MAX_PIXELS,
    };
    for file in seeds("format_decode") {
        let image = decode_bytes(&fs::read(&file).unwrap(), &limits)
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
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
