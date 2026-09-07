#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Heuristic {
    Conservative,
    Balanced,
    Aggressive,
}

impl Heuristic {
    pub(crate) const fn from_ffi(value: u32) -> Self {
        match value {
            0 => Self::Conservative,
            2 => Self::Aggressive,
            _ => Self::Balanced,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PassConfig {
    pub(crate) heuristic: Heuristic,
    pub(crate) forced_vf: Option<u32>,
    pub(crate) emit_remarks: bool,
}

impl PassConfig {
    pub(crate) const fn new(heuristic: Heuristic, forced_vf: u32, emit_remarks: bool) -> Self {
        Self {
            heuristic,
            forced_vf: if forced_vf == 0 {
                None
            } else {
                Some(forced_vf)
            },
            emit_remarks,
        }
    }

    pub(crate) const fn vector_bits() -> u32 {
        // Fixed 128-bit vectors are the conservative portable baseline for the
        // currently supported AArch64 and SSE-class targets. The policy stays
        // explicit until TargetTransformInfo is bridged into Rust.
        128
    }

    pub(crate) const fn minimum_trip_count(self, vf: u32) -> u64 {
        if self.forced_vf.is_some() {
            return vf as u64;
        }
        match self.heuristic {
            Heuristic::Conservative => 4 * vf as u64,
            Heuristic::Balanced => 2 * vf as u64,
            Heuristic::Aggressive => vf as u64,
        }
    }

    pub(crate) const fn required_speedup_x100(self) -> u64 {
        match self.heuristic {
            Heuristic::Conservative => 125,
            Heuristic::Balanced => 108,
            Heuristic::Aggressive => 100,
        }
    }
}
