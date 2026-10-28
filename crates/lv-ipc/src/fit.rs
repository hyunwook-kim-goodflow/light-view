//! The size target of a `FitWebp` request on the wire.

/// What a `FitWebp` request resizes to, as `lv_core::resize::Target` (lv-ipc does not
/// depend on lv-core; the app and the worker convert).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitTarget {
    /// The longer side at most this many pixels.
    LongEdge(u32),
    Width(u32),
    Height(u32),
    /// The file at most `bytes` long, the longer side at most `long_edge` if given.
    MaxBytes {
        bytes: u64,
        long_edge: Option<u32>,
    },
    /// Each side scaled to this percentage.
    Percent(u8),
}

/// Bytes of a target on the wire: `tag: u8 | value: u64 | long_edge: u32`.
pub(crate) const TARGET_LEN: u64 = 13;

const TAG_LONG_EDGE: u8 = 0;
const TAG_WIDTH: u8 = 1;
const TAG_HEIGHT: u8 = 2;
const TAG_MAX_BYTES: u8 = 3;
const TAG_PERCENT: u8 = 4;

impl FitTarget {
    /// Whether the target can go on the wire: sides and byte budgets of at least 1, a
    /// percentage of 1 to 100.
    pub fn is_valid(&self) -> bool {
        self.to_wire().is_some()
    }

    /// `(tag, value, long_edge)`, with `long_edge` 0 for "none" and every unused field 0,
    /// so that reading it back gives the same target. `None` for an invalid target.
    pub(crate) fn to_wire(self) -> Option<(u8, u64, u32)> {
        Some(match self {
            FitTarget::LongEdge(n @ 1..) => (TAG_LONG_EDGE, u64::from(n), 0),
            FitTarget::Width(n @ 1..) => (TAG_WIDTH, u64::from(n), 0),
            FitTarget::Height(n @ 1..) => (TAG_HEIGHT, u64::from(n), 0),
            FitTarget::MaxBytes {
                bytes: bytes @ 1..,
                long_edge,
            } => match long_edge {
                None => (TAG_MAX_BYTES, bytes, 0),
                Some(edge @ 1..) => (TAG_MAX_BYTES, bytes, edge),
                Some(0) => return None,
            },
            FitTarget::Percent(p @ 1..=100) => (TAG_PERCENT, u64::from(p), 0),
            _ => return None,
        })
    }

    /// The inverse of [`to_wire`](Self::to_wire); `None` for anything it never writes.
    pub(crate) fn from_wire(tag: u8, value: u64, long_edge: u32) -> Option<FitTarget> {
        let side = || u32::try_from(value).ok().filter(|_| long_edge == 0);
        let target = match tag {
            TAG_LONG_EDGE => FitTarget::LongEdge(side()?),
            TAG_WIDTH => FitTarget::Width(side()?),
            TAG_HEIGHT => FitTarget::Height(side()?),
            TAG_MAX_BYTES => FitTarget::MaxBytes {
                bytes: value,
                long_edge: (long_edge != 0).then_some(long_edge),
            },
            TAG_PERCENT => FitTarget::Percent(u8::try_from(side()?).ok()?),
            _ => return None,
        };
        (target.to_wire() == Some((tag, value, long_edge))).then_some(target)
    }
}
