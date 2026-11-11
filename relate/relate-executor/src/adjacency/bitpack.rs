//! Scalar bit packing (LSB-first, u128 accumulator so widths up to 64 work)
//! plus the zigzag value coding the hybrid codec's encoder and decoder share.
//! The experiment copy under `testing/relate-datasets/src/codec` stays
//! pinned (ADR-0551); this is the engine's scalar-only twin.

/// Bits needed to represent `max` (`0` for `max == 0`).
pub(crate) fn bit_width(max: u64) -> u32 {
    64 - max.leading_zeros()
}

/// Low-`width` mask; `width == 64` masks nothing.
fn mask(width: u32) -> u64 {
    debug_assert!(width <= 64);
    if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Zigzag encode: a signed delta becomes unsigned
/// (`0, -1, 1, -2, 2, …` → `0, 1, 2, 3, 4, …`).
pub(crate) fn zigzag(d: i64) -> u64 {
    ((d << 1) ^ (d >> 63)) as u64
}

/// Inverse of [`zigzag`].
pub(crate) fn unzigzag(z: u64) -> i64 {
    ((z >> 1) as i64) ^ -((z & 1) as i64)
}

pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    acc: u128,
    nbits: u32,
}

impl BitWriter {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            acc: 0,
            nbits: 0,
        }
    }

    pub(crate) fn write(&mut self, value: u64, width: u32) {
        debug_assert!(width <= 64);
        if width == 0 {
            return;
        }
        self.acc |= ((value & mask(width)) as u128) << self.nbits;
        self.nbits += width;
        while self.nbits >= 8 {
            self.bytes.push(self.acc as u8);
            self.acc >>= 8;
            self.nbits -= 8;
        }
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.bytes.push(self.acc as u8);
        }
        self.bytes
    }
}

pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    acc: u128,
    nbits: u32,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            acc: 0,
            nbits: 0,
        }
    }

    pub(crate) fn read(&mut self, width: u32) -> u64 {
        debug_assert!(width <= 64);
        if width == 0 {
            return 0;
        }
        while self.nbits < width {
            let byte = *self
                .bytes
                .get(self.pos)
                .expect("codec: truncated bit stream");
            self.acc |= (byte as u128) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
        let value = (self.acc & mask(width) as u128) as u64;
        self.acc >>= width;
        self.nbits -= width;
        value
    }
}
