pub trait Le {
    fn u8_at(&self, off: usize) -> u8;
    fn u16_at(&self, off: usize) -> u16;
    fn u32_at(&self, off: usize) -> u32;
    fn u64_at(&self, off: usize) -> u64;
    fn i16_at(&self, off: usize) -> i16;
    fn ascii_at(&self, off: usize, len: usize) -> String;
}

fn slice<const N: usize>(buf: &[u8], off: usize) -> [u8; N] {
    let mut out = [0u8; N];
    if let Some(src) = buf.get(off..off + N) {
        out.copy_from_slice(src);
    }
    out
}

impl Le for [u8] {
    fn u8_at(&self, off: usize) -> u8 {
        self.get(off).copied().unwrap_or(0)
    }

    fn u16_at(&self, off: usize) -> u16 {
        u16::from_le_bytes(slice(self, off))
    }

    fn u32_at(&self, off: usize) -> u32 {
        u32::from_le_bytes(slice(self, off))
    }

    fn u64_at(&self, off: usize) -> u64 {
        u64::from_le_bytes(slice(self, off))
    }

    fn i16_at(&self, off: usize) -> i16 {
        i16::from_le_bytes(slice(self, off))
    }

    fn ascii_at(&self, off: usize, len: usize) -> String {
        let end = (off + len).min(self.len());
        let raw = self.get(off..end).unwrap_or(&[]);
        let raw = raw.split(|b| *b == 0).next().unwrap_or(&[]);
        raw.iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .trim()
            .to_string()
    }
}

pub trait LeMut {
    fn put_u8(&mut self, off: usize, v: u8);
    fn put_u16(&mut self, off: usize, v: u16);
    fn put_u32(&mut self, off: usize, v: u32);
    fn put_u64(&mut self, off: usize, v: u64);
    fn put_bytes(&mut self, off: usize, v: &[u8]);
}

impl LeMut for [u8] {
    fn put_u8(&mut self, off: usize, v: u8) {
        self[off] = v;
    }

    fn put_u16(&mut self, off: usize, v: u16) {
        self[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u32(&mut self, off: usize, v: u32) {
        self[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u64(&mut self, off: usize, v: u64) {
        self[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn put_bytes(&mut self, off: usize, v: &[u8]) {
        self[off..off + v.len()].copy_from_slice(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_and_tolerates_short_buffers() {
        let buf = [0x01u8, 0x02, 0x03, 0x04, 0x05];
        assert_eq!(buf.u16_at(0), 0x0201);
        assert_eq!(buf.u32_at(1), 0x0504_0302);
        assert_eq!(buf.u32_at(3), 0);
        assert_eq!(buf.u8_at(9), 0);
    }

    #[test]
    fn ascii_stops_at_nul_and_trims() {
        let buf = b"  LSI \0junk";
        assert_eq!(buf.ascii_at(0, buf.len()), "LSI");
    }

    #[test]
    fn writes_round_trip() {
        let mut buf = [0u8; 16];
        buf.put_u64(0, 0x1122_3344_5566_7788);
        buf.put_u16(8, 0xBEEF);
        assert_eq!(buf.u64_at(0), 0x1122_3344_5566_7788);
        assert_eq!(buf.u16_at(8), 0xBEEF);
    }
}
