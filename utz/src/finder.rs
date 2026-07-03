//! `Finder`: grid prefilter → lazy per-polygon integer PIP (PLAN.md §3, §9).
//!
//! Lazy memory strategy: a lookup decodes AT MOST the candidate polygons whose
//! bbox contains the point, one at a time, into a reused scratch buffer.
//! Interior cells answer O(1) from the grid with zero geometry decoded.

use alloc::vec::Vec;
use core::cell::RefCell;

use crate::format::{self, fixed_bytes, read_fixed, read_u16, read_u32, read_varint, unzigzag, Header};
use crate::{decompress, pip, Error};

const NO_ZONE: u16 = 0x7FFF;

enum Source {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl Source {
    fn bytes(&self) -> &[u8] {
        match self {
            Source::Static(b) => b,
            Source::Owned(v) => v,
        }
    }
}

/// A loaded timezone index. Build once, query many.
pub struct Finder {
    payload: Source,
    hdr: Header,
    /// scratch for the polygon being tested (coords + ring split points)
    scratch: RefCell<(Vec<(i32, i32)>, Vec<usize>)>,
}

impl Finder {
    /// Borrow a container from `&'static` bytes (e.g. a flash partition).
    /// Zero-copy: only the `uncompressed` codec is accepted here.
    pub fn from_static(bytes: &'static [u8]) -> Result<Finder, Error> {
        let (codec, _, start) = format::outer(bytes)?;
        if codec != 0 {
            return Err(Error::Decompress); // compressed containers need an owned buffer
        }
        let payload = &bytes[start..];
        let hdr = format::parse(payload)?;
        Ok(Finder { payload: Source::Static(payload), hdr, scratch: RefCell::new((Vec::new(), Vec::new())) })
    }

    /// Take ownership of a container buffer (e.g. read from flash / OTA blob),
    /// decompressing per the codec byte if a backend is compiled in. The
    /// `no_std` entry point for compressed containers.
    pub fn from_vec(bytes: Vec<u8>) -> Result<Finder, Error> {
        let (codec, raw_len, start) = format::outer(&bytes)?;
        let payload = if codec == 0 {
            let mut p = bytes;
            p.copy_within(start.., 0); // reuse the allocation
            p.truncate(p.len() - start);
            p
        } else {
            decompress::decompress(codec, raw_len, &bytes[start..])?
        };
        let hdr = format::parse(&payload)?;
        Ok(Finder { payload: Source::Owned(payload), hdr, scratch: RefCell::new((Vec::new(), Vec::new())) })
    }

    /// Read a container from any `Read` source into an owned buffer.
    #[cfg(feature = "std")]
    pub fn from_reader(mut r: impl std::io::Read) -> Result<Finder, Error> {
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes).map_err(|_| Error::BadFormat)?;
        Finder::from_vec(bytes)
    }

    /// TZBB release recorded in the container header.
    pub fn tzbb_release(&self) -> &str {
        core::str::from_utf8(format::release(self.payload.bytes())).unwrap_or("")
    }

    /// Accurate lookup: grid cell → interior zone (O(1)) or candidates → PIP.
    /// `(lon, lat)` order — x before y.
    pub fn lookup(&self, lon: f64, lat: f64) -> Option<&str> {
        let (px, py) = self.quantize(lon, lat);
        match self.cell_value(px, py) {
            v if v == NO_ZONE => None,
            v if v & 0x8000 == 0 => self.tzid(v),
            v => {
                let (s, e) = self.list_bounds(v & 0x7FFF);
                let b = self.payload.bytes();
                let mut first = None;
                for pos in (s..e).step_by(2) {
                    let fid = read_u16(b, pos);
                    first.get_or_insert(fid);
                    if self.feature_contains(fid, px, py) {
                        return self.tzid(fid);
                    }
                }
                // quantization edge: no candidate claims the point — the
                // dominant-first head is the best answer (measured ~0/100k)
                first.and_then(|fid| self.tzid(fid))
            }
        }
    }

    /// Grid-only approximate lookup: no geometry decoded, ~cell-size border
    /// error. Border cells answer with the cell's dominant zone.
    pub fn fuzzy(&self, lon: f64, lat: f64) -> Option<&str> {
        let (px, py) = self.quantize(lon, lat);
        match self.cell_value(px, py) {
            v if v == NO_ZONE => None,
            v if v & 0x8000 == 0 => self.tzid(v),
            v => {
                let (s, _) = self.list_bounds(v & 0x7FFF);
                self.tzid(read_u16(self.payload.bytes(), s)) // dominant-first head
            }
        }
    }

    fn qmax(&self) -> f64 {
        ((1u64 << (self.hdr.quant_bits - 1)) - 1) as f64
    }
    fn quantize(&self, lon: f64, lat: f64) -> (i32, i32) {
        // round-half-away like the encoder (f64::round is std-only)
        let r = |v: f64| (v + if v >= 0.0 { 0.5 } else { -0.5 }) as i32;
        let q = self.qmax();
        (r(lon / 180.0 * q), r(lat / 90.0 * q))
    }

    fn cell_value(&self, px: i32, py: i32) -> u16 {
        let (h, q) = (&self.hdr, self.qmax());
        let d = h.grid_deg as f64;
        let lon = px as f64 / q * 180.0;
        let lat = py as f64 / q * 90.0;
        let c = (((lon + 180.0) / d) as i64).clamp(0, h.ncols as i64 - 1) as usize;
        let r = (((lat + 90.0) / d) as i64).clamp(0, h.nrows as i64 - 1) as usize;
        read_u16(self.payload.bytes(), h.primary + (r * h.ncols as usize + c) * 2)
    }

    fn list_bounds(&self, li: u16) -> (usize, usize) {
        let (h, b) = (&self.hdr, self.payload.bytes());
        let s = read_u16(b, h.list_offsets + li as usize * 2) as usize;
        let e = read_u16(b, h.list_offsets + li as usize * 2 + 2) as usize;
        (h.list_ids + s * 2, h.list_ids + e * 2)
    }

    fn tzid(&self, fid: u16) -> Option<&str> {
        let (h, b) = (&self.hdr, self.payload.bytes());
        let s = read_u16(b, h.str_offsets + fid as usize * 2) as usize;
        let e = read_u16(b, h.str_offsets + fid as usize * 2 + 2) as usize;
        core::str::from_utf8(&b[h.pool + s..h.pool + e]).ok().filter(|t| !t.is_empty())
    }

    /// Lazy per-polygon test: bbox skip, then decode one polygon into the
    /// scratch buffer and run the integer PIP at the width the header demands.
    fn feature_contains(&self, fid: u16, px: i32, py: i32) -> bool {
        let (h, b) = (&self.hdr, self.payload.bytes());
        let fb = fixed_bytes(h.quant_bits);
        let mut pos = h.ring_data + read_u32(b, h.feat_offsets + fid as usize * 4) as usize;
        let npolys = read_u16(b, pos);
        pos += 2;
        for _ in 0..npolys {
            let bb = [
                read_fixed(b, pos, h.quant_bits),
                read_fixed(b, pos + fb, h.quant_bits),
                read_fixed(b, pos + 2 * fb, h.quant_bits),
                read_fixed(b, pos + 3 * fb, h.quant_bits),
            ];
            pos += 4 * fb;
            let nrings = read_u16(b, pos);
            pos += 2;
            let inside_bb = px >= bb[0] && py >= bb[1] && px <= bb[2] && py <= bb[3];
            let mut scratch = self.scratch.borrow_mut();
            let (coords, ring_ends) = &mut *scratch;
            coords.clear();
            ring_ends.clear();
            for _ in 0..nrings {
                let (nrefs, mut p2) = read_varint(b, pos);
                for _ in 0..nrefs {
                    let (r, p3) = read_varint(b, p2);
                    p2 = p3;
                    if inside_bb {
                        self.append_arc(r as u32, coords);
                    }
                }
                pos = p2;
                if inside_bb {
                    if coords.last() == coords.first() && coords.len() > 1 {
                        coords.pop();
                    }
                    ring_ends.push(coords.len());
                }
            }
            if inside_bb {
                let mut rings: Vec<&[(i32, i32)]> = Vec::with_capacity(ring_ends.len());
                let mut start = 0;
                for &end in ring_ends.iter() {
                    rings.push(&coords[start..end]);
                    start = end;
                }
                let hit = if h.quant_bits == 32 {
                    pip::contains_i128(&rings, px, py)
                } else {
                    pip::contains_i64(&rings, px, py)
                };
                if hit {
                    return true;
                }
            }
        }
        false
    }

    /// Decode one signed arc ref onto the end of `coords` (join-deduplicated).
    fn append_arc(&self, r: u32, coords: &mut Vec<(i32, i32)>) {
        let (h, b) = (&self.hdr, self.payload.bytes());
        let (id, rev) = ((r >> 1) as usize, (r & 1) == 1);
        let mut pos = h.arc_data + read_u32(b, h.arc_offsets + id * 4) as usize;
        let (vcount, p2) = read_varint(b, pos);
        pos = p2;
        let fb = fixed_bytes(h.quant_bits);
        let mut x = read_fixed(b, pos, h.quant_bits) as i64;
        let mut y = read_fixed(b, pos + fb, h.quant_bits) as i64;
        pos += 2 * fb;
        let start = coords.len();
        coords.push((x as i32, y as i32));
        for _ in 1..vcount {
            let (dx, p3) = read_varint(b, pos);
            let (dy, p4) = read_varint(b, p3);
            pos = p4;
            x += unzigzag(dx);
            y += unzigzag(dy);
            coords.push((x as i32, y as i32));
        }
        if rev {
            coords[start..].reverse();
        }
        // drop the duplicated junction vertex where this arc joins the previous
        if start > 0 && coords.get(start - 1) == coords.get(start) {
            coords.remove(start);
        }
    }
}
