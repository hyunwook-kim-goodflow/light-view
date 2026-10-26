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
///   An sRGB ICC profile still goes through qcms and may differ from the input by
///   1 LSB; only `Srgb` is a guaranteed identity.
/// - nclx: primaries 1 (BT.709), 12 (Display P3) and 9 (BT.2020) with transfer 1 or 6
///   (BT.709 curve) or 13 (sRGB curve) are converted; every other combination is
///   treated as sRGB.
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
    if icc.get(16..20).is_some_and(|cs| cs != b"RGB ") {
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

const NCLX_PRIMARIES: [u16; 3] = [1, 12, 9];
const NCLX_TRANSFERS: [u16; 3] = [1, 6, 13];

/// The cached transform for a supported nclx pair; `None` for sRGB itself (BT.709
/// primaries with the sRGB curve) and for unsupported pairs.
fn nclx_transform(primaries: u16, transfer: u16) -> Option<&'static qcms::Transform> {
    static CACHE: [std::sync::OnceLock<Option<qcms::Transform>>; 9] =
        [const { std::sync::OnceLock::new() }; 9];
    if (primaries, transfer) == (1, 13) {
        return None;
    }
    let p = NCLX_PRIMARIES.iter().position(|&v| v == primaries)?;
    let t = NCLX_TRANSFERS.iter().position(|&v| v == transfer)?;
    CACHE[p * NCLX_TRANSFERS.len() + t]
        .get_or_init(|| {
            // qcms does not export the CICP enums; they convert from the H.273 code.
            let profile =
                qcms::Profile::new_cicp((primaries as u8).into(), (transfer as u8).into())?;
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
        let srgb_nclx = ColorSource::Nclx {
            primaries: 1,
            transfer: 13,
        };
        assert_eq!(converted(srgb_nclx), samples());
        // An sRGB ICC profile goes through qcms: within 1 LSB, alpha untouched.
        let icc = converted(ColorSource::Icc(SRGB_ICC));
        assert!(max_diff(&icc, &samples()) <= 1);
        let alphas = |v: &[u8]| v.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>();
        assert_eq!(alphas(&icc), alphas(&samples()));
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
