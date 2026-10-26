//! Color management: converts decoded RGBA8 to sRGB, then to the premultiplied BGRA8
//! the renderer uploads as is.

/// The color space decoded pixels are in.
#[derive(Clone, Copy, Debug)]
pub enum ColorSource<'a> {
    /// Already sRGB, or untagged (treated as sRGB).
    Srgb,
    /// An embedded ICC profile.
    Icc(&'a [u8]),
    /// CICP code points from an `nclx` color box (ITU-T H.273).
    Nclx { primaries: u16, transfer: u16 },
}

/// Converts straight-alpha RGBA8 `rgba` from `src` to sRGB in place; alpha is untouched.
///
/// - ICC: converted with qcms. A profile qcms cannot parse or build a transform from
///   is treated as sRGB (with a warning). A profile whose data color space is not RGB
///   (gray, CMYK) is treated as sRGB: the decoder already expanded the pixels to RGB.
///   A matrix/TRC profile with the sRGB colorants and curves (the 3 KB
///   "sRGB IEC61966-2.1" most camera JPEGs embed, or a compact v4 sRGB) is left alone:
///   qcms would change it by at most 1 LSB and costs ~20 ms on a 12 MP photo.
/// - nclx: primaries 1 (BT.709), 12 (Display P3) and 9 (BT.2020) are converted. Transfer
///   13 (sRGB) and the camera curves 1, 6 (BT.709/BT.601), 14, 15 (BT.2020) all use the
///   sRGB curve: phones tag sRGB-encoded photos with them, and decoding them with the
///   BT.709 inverse OETF visibly lifts the shadows (Windows Photos also shows them as
///   sRGB). Every other combination is treated as sRGB.
pub fn to_srgb_in_place(rgba: &mut [u8], src: ColorSource) {
    SrgbConverter::new(src).apply(rgba);
}

/// A conversion to sRGB, built once and applied to every frame of an image.
pub(crate) enum SrgbConverter {
    Identity,
    Owned(qcms::Transform),
    Shared(&'static qcms::Transform),
}

impl SrgbConverter {
    pub(crate) fn new(src: ColorSource) -> Self {
        match src {
            ColorSource::Srgb => Self::Identity,
            ColorSource::Icc(icc) => icc_converter(icc),
            ColorSource::Nclx {
                primaries,
                transfer,
            } => nclx_transform(primaries, transfer).map_or(Self::Identity, Self::Shared),
        }
    }

    /// Converts RGBA8 pixels in place; a trailing partial pixel is left alone.
    pub(crate) fn apply(&self, rgba: &mut [u8]) {
        let whole = rgba.len() / 4 * 4;
        match self {
            Self::Identity => {}
            Self::Owned(t) => t.apply(&mut rgba[..whole]),
            Self::Shared(t) => t.apply(&mut rgba[..whole]),
        }
    }
}

/// qcms ignores the intent for matrix/TRC profiles; perceptual is its default.
const INTENT: qcms::Intent = qcms::Intent::Perceptual;

/// The sRGB output profile, with its output tables precomputed once.
fn srgb_output() -> &'static qcms::Profile {
    static SRGB: std::sync::OnceLock<Box<qcms::Profile>> = std::sync::OnceLock::new();
    SRGB.get_or_init(|| {
        let mut p = qcms::Profile::new_sRGB();
        p.precache_output_transform();
        p
    })
}

fn icc_converter(icc: &[u8]) -> SrgbConverter {
    // ICC header: the data color space signature is at bytes 16..20.
    if icc.get(16..20).is_some_and(|cs| cs != b"RGB ") || is_srgb_profile(icc) {
        return SrgbConverter::Identity;
    }
    let transform = qcms::Profile::new_from_slice(icc, false)
        .and_then(|p| qcms::Transform::new(&p, srgb_output(), qcms::DataType::RGBA8, INTENT));
    match transform {
        Some(t) => SrgbConverter::Owned(t),
        None => {
            eprintln!("light-view: unusable ICC profile, treating the image as sRGB");
            SrgbConverter::Identity
        }
    }
}

/// D50 colorants (rXYZ, gXYZ, bXYZ) of sRGB as ICC profiles store them.
const SRGB_COLORANTS: [[f64; 3]; 3] = [
    [0.4361, 0.2225, 0.0139],
    [0.3851, 0.7169, 0.0971],
    [0.1431, 0.0606, 0.7141],
];
/// Colorant tolerance: s15Fixed16 rounding and the small differences between published
/// sRGB profiles stay well inside it; Display P3 or Adobe RGB are far outside.
const COLORANT_TOLERANCE: f64 = 0.002;
/// Curve tolerance in 8-bit code values of the re-encoded output.
const CURVE_TOLERANCE: f64 = 0.5;

/// Whether `icc` is a matrix/TRC RGB profile with the sRGB colorants and curves, so that
/// converting it to sRGB would change nothing visible. LUT-based profiles (`A2B*`), which
/// qcms would use instead of the matrix, never qualify.
fn is_srgb_profile(icc: &[u8]) -> bool {
    if icc.get(16..20) != Some(b"RGB ") || icc.get(20..24) != Some(b"XYZ ") {
        return false;
    }
    let Some(tags) = icc_tags(icc) else {
        return false;
    };
    let tag = |sig: &[u8; 4]| tags.iter().find(|(s, _)| s == sig).map(|(_, data)| *data);
    if [b"A2B0", b"A2B1", b"A2B2"]
        .iter()
        .any(|sig| tag(sig).is_some())
    {
        return false;
    }
    let colorants_match =
        [b"rXYZ", b"gXYZ", b"bXYZ"]
            .iter()
            .zip(SRGB_COLORANTS)
            .all(|(sig, expected)| {
                tag(sig).and_then(xyz_tag).is_some_and(|xyz| {
                    xyz.iter()
                        .zip(expected)
                        .all(|(got, want)| (got - want).abs() <= COLORANT_TOLERANCE)
                })
            });
    colorants_match
        && [b"rTRC", b"gTRC", b"bTRC"]
            .iter()
            .all(|sig| tag(sig).is_some_and(is_srgb_curve))
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn s15_fixed16(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from(be_u32(b, at)? as i32) / 65536.0)
}

/// The tag table as (signature, data) pairs; `None` if any entry lies outside `icc`.
fn icc_tags(icc: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let count = be_u32(icc, 128)? as usize;
    let table = icc.get(132..132usize.checked_add(count.checked_mul(12)?)?)?;
    table
        .chunks_exact(12)
        .map(|entry| {
            let sig: [u8; 4] = entry[0..4].try_into().ok()?;
            let offset = be_u32(entry, 4)? as usize;
            let size = be_u32(entry, 8)? as usize;
            Some((sig, icc.get(offset..offset.checked_add(size)?)?))
        })
        .collect()
}

/// The X, Y, Z of an `XYZ ` tag.
fn xyz_tag(tag: &[u8]) -> Option<[f64; 3]> {
    if tag.get(0..4)? != b"XYZ " {
        return None;
    }
    Some([
        s15_fixed16(tag, 8)?,
        s15_fixed16(tag, 12)?,
        s15_fixed16(tag, 16)?,
    ])
}

/// Whether a `curv` or `para` tag follows the sRGB EOTF: decoding each 8-bit code value
/// with it and re-encoding with sRGB gives the code value back within
/// [`CURVE_TOLERANCE`].
fn is_srgb_curve(tag: &[u8]) -> bool {
    let Some(curve) = Curve::parse(tag) else {
        return false;
    };
    (0..=255).all(|code| {
        let x = f64::from(code) / 255.0;
        (srgb_oetf(curve.eval(x)) * 255.0 - f64::from(code)).abs() <= CURVE_TOLERANCE
    })
}

fn srgb_oetf(linear: f64) -> f64 {
    let v = linear.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// A tone curve from an ICC `curv` or `para` tag.
enum Curve<'a> {
    Gamma(f64),
    /// Two or more big-endian u16 samples spaced evenly over 0..=1.
    Table(&'a [u8]),
    /// Parametric function type 0..=4 with its parameters (g, a, b, c, d, e, f).
    Parametric(u16, [f64; 7]),
}

impl<'a> Curve<'a> {
    fn parse(tag: &'a [u8]) -> Option<Curve<'a>> {
        match tag.get(0..4)? {
            b"curv" => {
                let count = be_u32(tag, 8)? as usize;
                let table = tag.get(12..12usize.checked_add(count.checked_mul(2)?)?)?;
                Some(match count {
                    0 => Curve::Gamma(1.0),
                    1 => Curve::Gamma(f64::from(u16::from_be_bytes([table[0], table[1]])) / 256.0),
                    _ => Curve::Table(table),
                })
            }
            b"para" => {
                let kind = u16::from_be_bytes(tag.get(8..10)?.try_into().ok()?);
                let count = [1, 3, 4, 5, 7].get(usize::from(kind)).copied()?;
                let mut params = [0.0; 7];
                for (i, p) in params.iter_mut().take(count).enumerate() {
                    *p = s15_fixed16(tag, 12 + 4 * i)?;
                }
                Some(Curve::Parametric(kind, params))
            }
            _ => None,
        }
    }

    fn eval(&self, x: f64) -> f64 {
        match self {
            Curve::Gamma(g) => x.powf(*g),
            Curve::Table(table) => {
                let sample =
                    |i: usize| f64::from(u16::from_be_bytes([table[2 * i], table[2 * i + 1]]));
                let last = table.len() / 2 - 1;
                let pos = x * last as f64;
                let i = (pos.floor() as usize).min(last - 1);
                let t = pos - i as f64;
                (sample(i) * (1.0 - t) + sample(i + 1) * t) / 65535.0
            }
            // ICC.1 parametric curve types 0-4. NaN from degenerate parameters fails the
            // comparison in `is_srgb_curve`, so such a curve never counts as sRGB.
            Curve::Parametric(kind, [g, a, b, c, d, e, f]) => {
                let pow = |v: f64| v.max(0.0).powf(*g);
                match kind {
                    0 => pow(x),
                    1 if x >= -b / a => pow(a * x + b),
                    1 => 0.0,
                    2 if x >= -b / a => pow(a * x + b) + c,
                    2 => *c,
                    3 if x >= *d => pow(a * x + b),
                    3 => c * x,
                    _ if x >= *d => pow(a * x + b) + e,
                    _ => c * x + f,
                }
            }
        }
    }
}

const NCLX_PRIMARIES: [u16; 3] = [1, 12, 9];
/// H.273 transfer 13, the sRGB curve.
const SRGB_TRANSFER: u8 = 13;

/// The cached transform for a supported nclx pair; `None` for sRGB itself (BT.709
/// primaries with the sRGB curve, or with a camera curve read as sRGB) and for
/// unsupported pairs.
fn nclx_transform(primaries: u16, transfer: u16) -> Option<&'static qcms::Transform> {
    static CACHE: [std::sync::OnceLock<Option<qcms::Transform>>; 3] =
        [const { std::sync::OnceLock::new() }; 3];
    // The camera curves 1, 6, 14 and 15 are decoded as sRGB (see `to_srgb_in_place`).
    if !matches!(transfer, 1 | 6 | 13 | 14 | 15) || primaries == 1 {
        return None;
    }
    let p = NCLX_PRIMARIES.iter().position(|&v| v == primaries)?;
    CACHE[p]
        .get_or_init(|| {
            // qcms does not export the CICP enums; they convert from the H.273 code.
            let profile = qcms::Profile::new_cicp((primaries as u8).into(), SRGB_TRANSFER.into())?;
            qcms::Transform::new(&profile, srgb_output(), qcms::DataType::RGBA8, INTENT)
        })
        .as_ref()
}

/// Straight-alpha RGBA8 to premultiplied BGRA8, in place: `c * a / 255`, rounded.
pub fn premultiply_to_bgra(mut rgba: Vec<u8>) -> Vec<u8> {
    for px in rgba.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        let mul = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
        let (r, g, b) = (px[0], px[1], px[2]);
        px[0] = mul(b);
        px[1] = mul(g);
        px[2] = mul(r);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    const P3_ICC: &[u8] = include_bytes!("../../../testdata/icc/DisplayP3-compact.icc");
    const SRGB_ICC: &[u8] = include_bytes!("../../../testdata/icc/sRGB-compact.icc");

    /// Six levels per channel (step 51), with varying alpha.
    fn samples() -> Vec<u8> {
        let mut out = Vec::new();
        for r in (0..=255).step_by(51) {
            for g in (0..=255).step_by(51) {
                for b in (0..=255).step_by(51) {
                    out.extend_from_slice(&[r as u8, g as u8, b as u8, (r ^ b) as u8]);
                }
            }
        }
        out
    }

    fn converted(src: ColorSource) -> Vec<u8> {
        let mut px = samples();
        to_srgb_in_place(&mut px, src);
        px
    }

    fn max_diff(a: &[u8], b: &[u8]) -> u8 {
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap()
    }

    #[test]
    fn srgb_or_untagged_is_identity() {
        assert_eq!(converted(ColorSource::Srgb), samples());
        // BT.709 primaries with the sRGB curve, or with a camera curve read as sRGB (R8).
        for transfer in [1, 6, 13, 14, 15] {
            let srgb_nclx = ColorSource::Nclx {
                primaries: 1,
                transfer,
            };
            assert_eq!(converted(srgb_nclx), samples(), "transfer {transfer}");
        }
        // A well-known sRGB ICC profile skips qcms.
        assert_eq!(converted(ColorSource::Icc(SRGB_ICC)), samples());
    }

    #[test]
    fn camera_transfers_use_the_srgb_curve_with_real_primaries() {
        for primaries in [12, 9] {
            let srgb_curve = converted(ColorSource::Nclx {
                primaries,
                transfer: 13,
            });
            assert_ne!(srgb_curve, samples());
            for transfer in [1, 6, 14, 15] {
                let src = ColorSource::Nclx {
                    primaries,
                    transfer,
                };
                assert_eq!(converted(src), srgb_curve, "({primaries}, {transfer})");
            }
        }
    }

    /// A matrix/TRC RGB profile: `colorants` (D50 XYZ of R, G, B) and one TRC tag shared by
    /// the three channels, plus `extra` tags.
    fn profile(colorants: [[f64; 3]; 3], trc: &[u8], extra: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let xyz = |v: [f64; 3]| {
            let mut t = b"XYZ \0\0\0\0".to_vec();
            for c in v {
                t.extend_from_slice(&((c * 65536.0).round() as i32).to_be_bytes());
            }
            t
        };
        let mut tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"rXYZ", xyz(colorants[0])),
            (b"gXYZ", xyz(colorants[1])),
            (b"bXYZ", xyz(colorants[2])),
            (b"rTRC", trc.to_vec()),
            (b"gTRC", trc.to_vec()),
            (b"bTRC", trc.to_vec()),
        ];
        tags.extend(extra.iter().map(|(sig, data)| (*sig, data.to_vec())));
        let mut out = vec![0u8; 128];
        out[8] = 2; // version 2.1
        out[9] = 0x10;
        out[12..16].copy_from_slice(b"mntr");
        out[16..20].copy_from_slice(b"RGB ");
        out[20..24].copy_from_slice(b"XYZ ");
        out[36..40].copy_from_slice(b"acsp");
        out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut offset = 128 + 4 + 12 * tags.len();
        let mut data = Vec::new();
        for (sig, tag) in &tags {
            out.extend_from_slice(*sig);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(tag.len() as u32).to_be_bytes());
            data.extend_from_slice(tag);
            while data.len() % 4 != 0 {
                data.push(0);
            }
            offset = 128 + 4 + 12 * tags.len() + data.len();
        }
        out.extend_from_slice(&data);
        let len = out.len() as u32;
        out[0..4].copy_from_slice(&len.to_be_bytes());
        out
    }

    const SRGB_COLORANTS: [[f64; 3]; 3] = [
        [0.43607, 0.22249, 0.01392],
        [0.38515, 0.71687, 0.09708],
        [0.14307, 0.06061, 0.71410],
    ];

    /// A `curv` table of `n` entries sampling `f`.
    fn curv(n: usize, f: impl Fn(f64) -> f64) -> Vec<u8> {
        let mut t = b"curv\0\0\0\0".to_vec();
        t.extend_from_slice(&(n as u32).to_be_bytes());
        for i in 0..n {
            let v = f(i as f64 / (n - 1) as f64);
            t.extend_from_slice(&((v * 65535.0).round() as u16).to_be_bytes());
        }
        t
    }

    fn srgb_eotf(v: f64) -> f64 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    #[test]
    fn recognizes_well_known_srgb_profiles() {
        // v4 with a parametric curve (Compact ICC Profiles' sRGB).
        assert!(is_srgb_profile(SRGB_ICC));
        // v2 with a 1024-entry table, like the 3 KB "sRGB IEC61966-2.1" in camera JPEGs.
        let v2 = profile(SRGB_COLORANTS, &curv(1024, srgb_eotf), &[]);
        assert!(is_srgb_profile(&v2));
        // The fast path only skips what qcms would (nearly) leave alone.
        let mut px = samples();
        let transform = qcms::Profile::new_from_slice(&v2, false)
            .and_then(|p| qcms::Transform::new(&p, srgb_output(), qcms::DataType::RGBA8, INTENT))
            .expect("the test profile is valid");
        transform.apply(&mut px);
        assert!(max_diff(&px, &samples()) <= 1);
    }

    #[test]
    fn other_profiles_are_not_mistaken_for_srgb() {
        assert!(!is_srgb_profile(P3_ICC));
        let gamma22 = profile(SRGB_COLORANTS, &curv(1024, |v| v.powf(2.2)), &[]);
        assert!(!is_srgb_profile(&gamma22));
        let mut p3_colorants = SRGB_COLORANTS;
        p3_colorants[0] = [0.51512, 0.24120, -0.00105];
        let p3 = profile(p3_colorants, &curv(1024, srgb_eotf), &[]);
        assert!(!is_srgb_profile(&p3));
        // A LUT-based profile is not read through its matrix.
        let lut = profile(
            SRGB_COLORANTS,
            &curv(1024, srgb_eotf),
            &[(b"A2B0", b"mft2\0\0\0\0")],
        );
        assert!(!is_srgb_profile(&lut));
        for bad in [&b""[..], b"not an icc profile", &SRGB_ICC[..200]] {
            assert!(!is_srgb_profile(bad));
        }
    }

    #[test]
    fn p3_profile_changes_pixels() {
        let mut px = [100, 150, 100, 255];
        to_srgb_in_place(&mut px, ColorSource::Icc(P3_ICC));
        assert_ne!(px, [100, 150, 100, 255]);
        assert_eq!(px[3], 255);
        let mut green = [0, 255, 0, 77];
        to_srgb_in_place(&mut green, ColorSource::Icc(P3_ICC));
        assert_eq!(green[0], 0, "P3 green is outside sRGB: red clips to 0");
        assert!(green[1] >= 250, "green stays near 255, got {green:?}");
        assert_eq!(green[3], 77, "alpha is untouched");
    }

    #[test]
    fn nclx_p3_matches_icc_p3() {
        let nclx = converted(ColorSource::Nclx {
            primaries: 12,
            transfer: 13,
        });
        let icc = converted(ColorSource::Icc(P3_ICC));
        assert_ne!(nclx, samples());
        assert!(max_diff(&nclx, &icc) <= 2);
    }

    #[test]
    fn nclx_unsupported_pairs_are_identity() {
        for (primaries, transfer) in [(2, 2), (12, 16), (12, 18), (5, 13), (0, 0), (300, 13)] {
            let src = ColorSource::Nclx {
                primaries,
                transfer,
            };
            assert_eq!(converted(src), samples(), "({primaries}, {transfer})");
        }
    }

    #[test]
    fn unparsable_icc_is_identity() {
        assert_eq!(
            converted(ColorSource::Icc(b"not an icc profile")),
            samples()
        );
        assert_eq!(converted(ColorSource::Icc(&[])), samples());
        let mut truncated = P3_ICC.to_vec();
        truncated.truncate(200);
        assert_eq!(converted(ColorSource::Icc(&truncated)), samples());
    }

    #[test]
    fn premultiply_rounding() {
        let rgba = vec![255, 128, 0, 255, 200, 100, 50, 128, 10, 20, 30, 0];
        assert_eq!(
            premultiply_to_bgra(rgba),
            vec![0, 128, 255, 255, 25, 50, 100, 128, 0, 0, 0, 0]
        );
    }
}
