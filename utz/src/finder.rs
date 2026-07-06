//! `Finder`: grid prefilter → per-polygon integer PIP (PLAN.md §3, §9).
//!
//! Memory modes — API-selected, availability falls out of the environment
//! rung (§9, §11):
//! - **lazy** (the default `lookup`): candidates are PIP-tested by streaming
//!   their arcs straight off the container bytes through the per-edge kernel
//!   (§14.7) — O(1) state, no decode buffer, no allocation. Works on the
//!   `core` rung, including zero-copy static sources (`from_static`: flash
//!   partition, `include_bytes!`, …). Interior cells touch zero geometry.
//! - **eager** ([`Finder::preload`], `alloc`): decode all polygons into RAM
//!   once; lookups then scan decoded rings. Fastest repeat lookups.
//! - **coarse** ([`Finder::lookup_coarse`]): grid-only, ~cell-size border
//!   error, no geometry ever.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use crate::format::{self, fixed_bytes, read_fixed, read_u16, read_u32, read_varint, unzigzag, Header};
#[cfg(feature = "alloc")]
use crate::decompress;
use crate::{pip, Error};

const NO_ZONE: u16 = 0x7FFF;

enum Source {
    Static(&'static [u8]),
    #[cfg(feature = "alloc")]
    Owned(Vec<u8>),
}

impl Source {
    fn bytes(&self) -> &[u8] {
        match self {
            Source::Static(b) => b,
            #[cfg(feature = "alloc")]
            Source::Owned(v) => v,
        }
    }
}

/// Eager-mode storage: every ring decoded, flat (§9). Ranges are exclusive
/// ends; a range's start is the previous entry's end (global across
/// features, so no per-item start field).
#[cfg(feature = "alloc")]
struct Eager {
    coords: Vec<(i32, i32)>,
    /// exclusive end into `coords` per ring
    ring_ends: Vec<u32>,
    /// per polygon: bbox + exclusive end into `ring_ends`
    polys: Vec<([i32; 4], u32)>,
    /// per feature: exclusive end into `polys`
    feat_ends: Vec<u32>,
}

/// A loaded timezone index. Build once, query many.
///
/// Availability follows the environment ladder (§11): `core` gets
/// [`from_static`](Finder::from_static), [`lookup`](Finder::lookup) (lazy,
/// streaming) and [`lookup_coarse`](Finder::lookup_coarse); `alloc` adds
/// owned/compressed containers and [`preload`](Finder::preload) (eager mode);
/// `std` adds [`from_reader`](Finder::from_reader).
pub struct Finder {
    payload: Source,
    hdr: Header,
    /// eager-mode geometry, populated by `preload`
    #[cfg(feature = "alloc")]
    eager: Option<Eager>,
}

impl Finder {
    /// Load the preset selected by the (single) enabled preset feature.
    /// Cfg'd out when several presets are in the tree — load explicitly with
    /// `from_slice(utz::data::NANO)` instead.
    // extend per preset: all(feature = "nano", not(any(feature = "micro", …)))
    #[cfg(feature = "nano")]
    pub fn new() -> Result<Finder, Error> {
        Finder::from_slice(crate::data::NANO)
    }

    /// Borrow a container from `&'static` bytes (e.g. a flash partition).
    /// Zero-copy: only the `uncompressed` codec is accepted here.
    pub fn from_static(bytes: &'static [u8]) -> Result<Finder, Error> {
        let (codec, _, start) = format::outer(bytes)?;
        if codec != 0 {
            return Err(Error::Decompress); // compressed containers need an owned buffer
        }
        let payload = &bytes[start..];
        let hdr = format::parse(payload)?;
        Ok(Finder {
            payload: Source::Static(payload),
            hdr,
            #[cfg(feature = "alloc")]
            eager: None,
        })
    }

    /// Decode a borrowed container into an owned `Finder`, decompressing per
    /// the codec byte. For compressed assets already in memory/flash (preset
    /// statics, OTA blobs) — no copy of the compressed input is made.
    #[cfg(feature = "alloc")]
    pub fn from_slice(bytes: &[u8]) -> Result<Finder, Error> {
        let (codec, raw_len, start) = format::outer(bytes)?;
        let payload = if codec == 0 {
            bytes[start..].to_vec()
        } else {
            decompress::decompress(codec, raw_len, &bytes[start..])?
        };
        let hdr = format::parse(&payload)?;
        Ok(Finder { payload: Source::Owned(payload), hdr, eager: None })
    }

    /// Take ownership of a container buffer (e.g. read from flash / OTA blob),
    /// decompressing per the codec byte if a backend is compiled in. The
    /// `no_std` entry point for compressed containers.
    #[cfg(feature = "alloc")]
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
        Ok(Finder { payload: Source::Owned(payload), hdr, eager: None })
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

    /// Decode all polygons into RAM once (eager mode, §9): repeat lookups
    /// then skip the per-arc varint decode. Costs roughly the uncompressed
    /// geometry size in heap; a no-op if already preloaded.
    #[cfg(feature = "alloc")]
    pub fn preload(&mut self) {
        if self.eager.is_some() {
            return;
        }
        let (h, b) = (&self.hdr, self.payload.bytes());
        let fb = fixed_bytes(h.quant_bits);
        let mut e = Eager {
            coords: Vec::new(),
            ring_ends: Vec::new(),
            polys: Vec::new(),
            feat_ends: Vec::new(),
        };
        for fid in 0..h.n_features {
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
                for _ in 0..nrings {
                    let (nrefs, mut p2) = read_varint(b, pos);
                    let start = e.coords.len();
                    for _ in 0..nrefs {
                        let (r, p3) = read_varint(b, p2);
                        p2 = p3;
                        self.append_arc(r as u32, &mut e.coords);
                    }
                    pos = p2;
                    // drop the duplicated ring-closure vertex (ring_hit wraps)
                    if e.coords.len() > start + 1 && e.coords.last() == e.coords.get(start) {
                        e.coords.pop();
                    }
                    e.ring_ends.push(e.coords.len() as u32);
                }
                e.polys.push((bb, e.ring_ends.len() as u32));
            }
            e.feat_ends.push(e.polys.len() as u32);
        }
        self.eager = Some(e);
    }

    /// Accurate lookup: grid cell → interior zone (O(1)) or candidates → PIP.
    /// `(lon, lat)` order — x before y.
    ///
    /// Lazy by default (streaming PIP over the container bytes, zero alloc);
    /// eager after [`preload`](Finder::preload).
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
    pub fn lookup_coarse(&self, lon: f64, lat: f64) -> Option<&str> {
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

    /// Per-polygon test: bbox skip, then even-odd PIP at the width the header
    /// demands. Lazy path streams the arcs straight off the container bytes
    /// through the per-edge kernel (§14.7): junction vertices are shared by
    /// consecutive arcs and the ring closure is a shared junction too, so the
    /// ring's segment set is exactly the union of each arc's internal
    /// segments — every arc is walked FORWARD (orientation bit ignored) with
    /// O(1) state, and parity XORs across arcs order-independently.
    fn feature_contains(&self, fid: u16, px: i32, py: i32) -> bool {
        #[cfg(feature = "alloc")]
        if let Some(e) = &self.eager {
            return self.eager_contains(e, fid, px, py);
        }
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
            let mut poly_inside = false;
            for _ in 0..nrings {
                let (nrefs, mut p2) = read_varint(b, pos);
                let mut ring_inside = false;
                for _ in 0..nrefs {
                    let (r, p3) = read_varint(b, p2);
                    p2 = p3;
                    if inside_bb {
                        match self.scan_arc((r >> 1) as usize, px, py) {
                            pip::RingHit::Boundary => return true, // border points claimed
                            pip::RingHit::Inside => ring_inside = !ring_inside,
                            pip::RingHit::Outside => {}
                        }
                    }
                }
                pos = p2;
                if ring_inside {
                    poly_inside = !poly_inside;
                }
            }
            if poly_inside {
                return true;
            }
        }
        false
    }

    /// Fold one arc's internal segments through the edge kernel. `Inside` =
    /// this arc contributed an odd number of ray crossings.
    fn scan_arc(&self, id: usize, px: i32, py: i32) -> pip::RingHit {
        let (h, b) = (&self.hdr, self.payload.bytes());
        let wide = h.quant_bits == 32;
        let mut pos = h.arc_data + read_u32(b, h.arc_offsets + id * 4) as usize;
        let (vcount, p2) = read_varint(b, pos);
        pos = p2;
        let fb = fixed_bytes(h.quant_bits);
        let mut x = read_fixed(b, pos, h.quant_bits) as i64;
        let mut y = read_fixed(b, pos + fb, h.quant_bits) as i64;
        pos += 2 * fb;
        let mut inside = false;
        let (mut x0, mut y0) = (x as i32, y as i32);
        for _ in 1..vcount {
            let (dx, p3) = read_varint(b, pos);
            let (dy, p4) = read_varint(b, p3);
            pos = p4;
            x += unzigzag(dx);
            y += unzigzag(dy);
            let (x1, y1) = (x as i32, y as i32);
            let hit = if wide {
                pip::edge_i128((x0, y0), (x1, y1), px, py)
            } else {
                pip::edge_i64((x0, y0), (x1, y1), px, py)
            };
            match hit {
                pip::EdgeHit::Boundary => return pip::RingHit::Boundary,
                pip::EdgeHit::Cross => inside = !inside,
                pip::EdgeHit::Miss => {}
            }
            (x0, y0) = (x1, y1);
        }
        if inside {
            pip::RingHit::Inside
        } else {
            pip::RingHit::Outside
        }
    }

    /// Eager path: same even-odd fold over pre-decoded rings.
    #[cfg(feature = "alloc")]
    fn eager_contains(&self, e: &Eager, fid: u16, px: i32, py: i32) -> bool {
        let wide = self.hdr.quant_bits == 32;
        let pstart = if fid == 0 { 0 } else { e.feat_ends[fid as usize - 1] as usize };
        let pend = e.feat_ends[fid as usize] as usize;
        for pi in pstart..pend {
            let (bb, rend) = e.polys[pi];
            let rstart = if pi == 0 { 0 } else { e.polys[pi - 1].1 as usize };
            if !(px >= bb[0] && py >= bb[1] && px <= bb[2] && py <= bb[3]) {
                continue;
            }
            let mut inside = false;
            let mut cstart = if rstart == 0 { 0 } else { e.ring_ends[rstart - 1] as usize };
            for ri in rstart..rend as usize {
                let cend = e.ring_ends[ri] as usize;
                let ring = &e.coords[cstart..cend];
                cstart = cend;
                let hit = if wide {
                    pip::ring_hit_i128(ring, px, py)
                } else {
                    pip::ring_hit_i64(ring, px, py)
                };
                match hit {
                    pip::RingHit::Boundary => return true,
                    pip::RingHit::Inside => inside = !inside,
                    pip::RingHit::Outside => {}
                }
            }
            if inside {
                return true;
            }
        }
        false
    }

    /// Decode one signed arc ref onto the end of `coords` (join-deduplicated).
    /// Eager-mode decode only; the lazy path streams via `scan_arc` instead.
    #[cfg(feature = "alloc")]
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
