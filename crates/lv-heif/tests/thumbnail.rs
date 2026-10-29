//! `thumbnail_heif` (ruling R4): the smallest embedded thumbnail whose long side reaches the
//! requested size, else the primary image scaled down; turned like the primary image.
//!
//! The files are muxed from the samples' HEVC images by `support/heif_build.rs` (no
//! encoder). Two of them are committed for the worker's tests, see
//! `testdata/heif/README.md`:
//!
//! ```text
//! cargo test -p lv-heif --test thumbnail -- --ignored write_thumbnail_fixtures
//! ```

#[path = "support/heif_build.rs"]
mod heif_build;

use heif_build::{alpha_sample_colour, imir, irot, rainbow, sample, Builder};
use lv_heif::{decode_heif, take_decoded_items, thumbnail_heif, ColorInfo, HeifError, HeifImage};

const NO_LIMIT: u64 = 100_000_000;

/// `grid-4032x3024.heic`: a 9 x 7 grid of the rainbow sample's coded image (452 x 462,
/// no `clap` on the tiles), cropped to 4032 x 3024, with the rainbow's ICC profile. Item 64
/// is the grid; items 1 to 63 the tiles.
fn grid_file() -> Vec<u8> {
    let mut b = Builder::new();
    let grid = b.grid(&rainbow(), (9, 7), (4032, 3024), &[]);
    b.primary(grid);
    b.finish()
}

/// Item ids in `grid-4032x3024-thumbs.heic`.
const GRID: u32 = 64;
const RAINBOW_THUMB: u32 = 65;
const ALPHA_COLOUR_THUMB: u32 = 66;

/// `grid-4032x3024-thumbs.heic`: the grid above plus two `thmb` thumbnails: the rainbow
/// image with its `clap` (451 x 461, item 65) and the colour image of the alpha sample
/// without its alpha plane (512 x 512, item 66).
fn grid_thumbs_file() -> Vec<u8> {
    let mut b = Builder::new();
    let grid = b.grid(&rainbow(), (9, 7), (4032, 3024), &[]);
    let small = b.image(&rainbow(), true, &[]);
    let large = b.image(&alpha_sample_colour(), false, &[]);
    assert_eq!(
        (grid, small, large),
        (GRID, RAINBOW_THUMB, ALPHA_COLOUR_THUMB)
    );
    b.thumbnail(small, grid);
    b.thumbnail(large, grid);
    b.primary(grid);
    b.finish()
}

/// A committed fixture: its file name and how it is made.
type Fixture = (&'static str, fn() -> Vec<u8>);

const FIXTURES: [Fixture; 2] = [
    ("grid-4032x3024.heic", grid_file),
    ("grid-4032x3024-thumbs.heic", grid_thumbs_file),
];

#[test]
#[ignore = "rewrites testdata/heif; run by hand"]
fn write_thumbnail_fixtures() {
    for (name, make) in FIXTURES {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/heif")
            .join(name);
        std::fs::write(&path, make()).unwrap();
    }
}

#[test]
fn committed_fixtures_are_what_the_builder_makes() {
    for (name, make) in FIXTURES {
        assert!(
            sample(name) == make(),
            "{name} differs: rerun write_thumbnail_fixtures"
        );
    }
}

fn thumb(bytes: &[u8], max_side: u32) -> HeifImage {
    take_decoded_items();
    thumbnail_heif(bytes, max_side, NO_LIMIT).unwrap_or_else(|e| panic!("thumbnail: {e:?}"))
}

fn assert_shape(img: &HeifImage, size: (u32, u32)) {
    assert_eq!((img.width, img.height), size);
    assert_eq!(
        img.rgba.len() as u64,
        u64::from(size.0) * u64::from(size.1) * 4
    );
}

/// Mean absolute RGBA difference of two images of the same size.
fn mean_diff(a: &HeifImage, b: &HeifImage) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let sum: u64 = a
        .rgba
        .iter()
        .zip(&b.rgba)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    sum as f64 / a.rgba.len() as f64
}

#[test]
fn thumbnail_fits_max_side() {
    let file = sample("grid-4032x3024.heic");
    let img = thumb(&file, 256);
    assert_shape(&img, (256, 192));
    assert_eq!(
        take_decoded_items(),
        vec![GRID],
        "the primary (grid) image is decoded"
    );
    // Colour is reported as for the full decode, for the worker's sRGB conversion.
    assert_eq!(img.color, decode_heif(&file, NO_LIMIT).unwrap().color);
    assert!(matches!(img.color, ColorInfo::Icc(_)));
    // Never scaled up; a long side equal to the request is kept as it is.
    let rainbow = sample("rainbow-451x461.heic");
    assert_eq!(
        thumb(&rainbow, 4096),
        decode_heif(&rainbow, NO_LIMIT).unwrap()
    );
    assert_eq!(
        thumb(&rainbow, 461),
        decode_heif(&rainbow, NO_LIMIT).unwrap()
    );
    assert_shape(&thumb(&rainbow, 460), (450, 460));
    assert_shape(&thumb(&rainbow, 1), (1, 1));
}

#[test]
fn scaled_thumbnail_looks_like_the_full_decode() {
    // A box-filtered and resampled rainbow keeps its colours where they are.
    let rainbow = sample("rainbow-451x461.heic");
    let full = decode_heif(&rainbow, NO_LIMIT).unwrap();
    let small = thumb(&rainbow, 46);
    assert_shape(&small, (45, 46));
    let mut worst = 0u8;
    for (y, row) in small.rgba.chunks_exact(45 * 4).enumerate() {
        for (x, px) in row.chunks_exact(4).enumerate() {
            // The centre of the 10 x 10 source block this pixel stands for.
            let (sx, sy) = (x * 451 / 45 + 5, y * 461 / 46 + 5);
            let at = (sy * 451 + sx) * 4;
            for (c, &v) in px[..3].iter().enumerate() {
                worst = worst.max(v.abs_diff(full.rgba[at + c]));
            }
        }
    }
    assert!(worst < 48, "largest channel difference {worst}");
}

#[test]
fn alpha_survives_scaling() {
    let img = thumb(&sample("with-alpha-512x512.heic"), 64);
    assert_shape(&img, (64, 64));
    let alphas: Vec<u8> = img.rgba.chunks_exact(4).map(|px| px[3]).collect();
    assert!(alphas.contains(&0) && alphas.contains(&255));
}

#[test]
fn embedded_thumbnail_is_used_when_big_enough() {
    let file = sample("grid-4032x3024-thumbs.heic");
    let img = thumb(&file, 256);
    // From the 451 x 461 thumbnail; the 4032 x 3024 grid would give 256 x 192.
    assert_shape(&img, (250, 256));
    assert_eq!(
        take_decoded_items(),
        vec![RAINBOW_THUMB],
        "only the thumbnail is decoded, never the primary image"
    );
    // It is the rainbow, scaled like the rainbow sample itself.
    let direct = thumb(&sample("rainbow-451x461.heic"), 256);
    assert!(mean_diff(&img, &direct) < 0.5);
}

#[test]
fn smallest_big_enough_thumbnail_is_chosen() {
    let file = sample("grid-4032x3024-thumbs.heic");
    for (max_side, size, item) in [
        (1, (1, 1), RAINBOW_THUMB),
        (461, (451, 461), RAINBOW_THUMB),
        (462, (462, 462), ALPHA_COLOUR_THUMB),
        (512, (512, 512), ALPHA_COLOUR_THUMB),
        (513, (513, 385), GRID),
        (1024, (1024, 768), GRID),
    ] {
        let img = thumb(&file, max_side);
        assert_shape(&img, size);
        assert_eq!(take_decoded_items(), vec![item], "max side {max_side}");
    }
}

#[test]
fn bad_max_side_is_refused() {
    let file = sample("rainbow-451x461.heic");
    for max_side in [0, 4097, u32::MAX] {
        assert!(
            matches!(
                thumbnail_heif(&file, max_side, NO_LIMIT),
                Err(HeifError::Corrupt(_))
            ),
            "max side {max_side}"
        );
    }
    assert!(thumbnail_heif(&file, 4096, NO_LIMIT).is_ok());
}

#[test]
fn pixel_limit_applies_to_the_decoded_image() {
    // The primary and the 512 x 512 thumbnail are too large for the limit; the rainbow
    // thumbnail (coded 452 x 462, shown 451 x 461) is not. (libheif's own limit counts
    // its 456 x 464 aligned planes, as for `decode_heif`.)
    let file = sample("grid-4032x3024-thumbs.heic");
    take_decoded_items();
    assert_shape(&thumbnail_heif(&file, 256, 250_000).unwrap(), (250, 256));
    assert_eq!(take_decoded_items(), vec![RAINBOW_THUMB]);
    // Its coded size counts too: shown it fits 208 000 pixels, coded it does not, so the
    // primary image is tried (too large).
    assert!(matches!(
        thumbnail_heif(&file, 256, 208_000),
        Err(HeifError::TooLarge {
            width: 4032,
            height: 3024
        })
    ));
    assert!(matches!(
        thumbnail_heif(&file, 1024, 1_000_000),
        Err(HeifError::TooLarge {
            width: 4032,
            height: 3024
        })
    ));
    assert!(matches!(
        thumbnail_heif(&[], 256, NO_LIMIT),
        Err(HeifError::Corrupt(_))
    ));
}

#[test]
fn thumbnail_applies_rotation() {
    // irot 90 (anti-clockwise) on the grid: the 4:3 photo is shown portrait.
    let mut b = Builder::new();
    let grid = b.grid(&rainbow(), (9, 7), (4032, 3024), &[irot(1)]);
    b.primary(grid);
    let img = thumb(&b.finish(), 256);
    assert_shape(&img, (192, 256));

    // The same turn as the full decode of the rotated sample.
    let rotated = sample("rainbow-451x461-irot90.heic");
    let img = thumb(&rotated, 200);
    assert_shape(&img, (200, 196));
    let mut b = Builder::new();
    let p = b.image(&rainbow(), true, &[irot(1)]);
    b.primary(p);
    assert!(mean_diff(&img, &thumb(&b.finish(), 200)) < 0.5);
}

/// A file whose primary image is the rainbow (with `clap`) turned by `primary`, with a
/// `thmb` thumbnail of the same coded image turned by `thumbnail`; and the id of the
/// thumbnail.
/// Whole `irot`/`imir` boxes, in `ipma` order.
type Turns = Vec<Vec<u8>>;

fn turned_pair(primary: &[Vec<u8>], thumbnail: &[Vec<u8>]) -> (Vec<u8>, u32) {
    let mut b = Builder::new();
    let p = b.image(&rainbow(), true, primary);
    let t = b.image(&rainbow(), true, thumbnail);
    b.thumbnail(t, p);
    b.primary(p);
    (b.finish(), t)
}

#[test]
fn embedded_thumbnail_is_oriented_like_primary() {
    // (what, primary item's turns, thumbnail's turns)
    let cases: [(&str, Turns, Turns); 7] = [
        ("primary turned, thumbnail not", vec![irot(1)], vec![]),
        ("both turned alike (iPhone)", vec![irot(3)], vec![irot(3)]),
        ("thumbnail turned, primary not", vec![], vec![irot(1)]),
        ("primary mirrored on axis 0", vec![imir(0)], vec![]),
        ("primary mirrored on axis 1", vec![imir(1)], vec![]),
        ("mirror then turn", vec![imir(1), irot(1)], vec![irot(2)]),
        ("turn then mirror", vec![irot(1), imir(0)], vec![imir(1)]),
    ];
    for (what, p, t) in cases {
        let (file, thumb_id) = turned_pair(&p, &t);
        let img = thumb(&file, 300);
        assert_eq!(
            take_decoded_items(),
            vec![thumb_id],
            "{what}: thumbnail used"
        );
        // What the app shows: libheif's decode of the primary image, scaled the same way.
        let mut b = Builder::new();
        let only = b.image(&rainbow(), true, &p);
        b.primary(only);
        let expected = thumb(&b.finish(), 300);
        assert_eq!(
            (img.width, img.height),
            (expected.width, expected.height),
            "{what}"
        );
        let diff = mean_diff(&img, &expected);
        assert!(diff < 1.0, "{what}: mean difference {diff}");
    }
}

#[test]
fn corrupt_thumbnail_falls_back_to_the_primary_image() {
    // A thumbnail that is not an HEVC image libheif can decode: the primary is used.
    let mut b = Builder::new();
    let p = b.image(&rainbow(), true, &[]);
    let mut broken = alpha_sample_colour();
    broken.data.truncate(16);
    let t = b.image(&broken, false, &[]);
    b.thumbnail(t, p);
    b.primary(p);
    let img = thumb(&b.finish(), 256);
    assert_shape(&img, (250, 256));
    let decoded = take_decoded_items();
    assert_eq!(decoded.last(), Some(&p), "{decoded:?}");
}
