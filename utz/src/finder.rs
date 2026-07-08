//! `Finder`: grid prefilter → per-polygon integer PIP (PLAN.md §3, §9).
//!
//! Three memory modes, selected automatically by how the container is loaded
//! (only eager is an explicit request); availability falls out of the
//! environment rung (§9, §11):
//! - **zero-copy** (`from_static`, uncompressed): the payload is borrowed
//!   from static storage (flash partition, `include_bytes!`, …). No RAM
//!   payload at all; flash pays the uncompressed size. `core` rung.
//! - **lazy** (`from_slice`/`from_vec`/`from_reader`): the payload lives in
//!   owned RAM — typically because the asset is compressed and flash can't
//!   fit it uncompressed. No decoded-geometry cache: RAM = the decompressed
//!   payload, nothing more.
//! - **eager** ([`Finder::preload`], `alloc`): additionally decode all rings
//!   into RAM once; lookups then scan decoded slices. Most RAM, fastest
//!   repeat lookups.
//!
//! Zero-copy and lazy share the identical lookup mechanism — candidates are
//! PIP-tested by walking their arcs directly off the payload bytes through
//! the per-edge kernel (§14.7), O(1) state, no allocation — they differ only
//! in where the payload resides (borrowed static vs owned RAM), i.e. the
//! `Cow` variant of [`Payload`]. Interior cells touch zero geometry in every
//! mode, and [`Finder::lookup_coarse`] never touches geometry at all.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use crate::format::{self, fixed_bytes, read_fixed, read_u16, read_u32, read_varint, unzigzag, Header};
#[cfg(feature = "alloc")]
use crate::decompress;
use crate::{pip, Error};

const NO_ZONE: u16 = 0x7FFF;

/// A geographic position in degrees — **order-neutral by design** (§14.3):
/// construct with named fields, so there is no argument order to get wrong,
/// only values. `Position { lat: 51.5, lon: -0.13 }` and
/// `Position { lon: -0.13, lat: 51.5 }` are the same position.
///
/// Deliberately no positional constructor and no `From<(f64, f64)>` — either
/// would reintroduce the lon/lat-swap footgun this type exists to kill.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Position {
    /// longitude in degrees, −180..=180 (x)
    pub lon: f64,
    /// latitude in degrees, −90..=90 (y)
    pub lat: f64,
}

/// The container's payload section. `Cow::Borrowed` = zero-copy mode,
/// `Cow::Owned` = lazy/eager (§9). `Cow` itself lives in `alloc`; on the
/// bare-`core` rung borrowed is the only possible variant.
#[cfg(feature = "alloc")]
type Payload = alloc::borrow::Cow<'static, [u8]>;
#[cfg(not(feature = "alloc"))]
type Payload = &'static [u8];

/// Eager-mode storage: every ring decoded, flat (§9). Ranges are exclusive
/// ends; a range's start is the previous entry's end (global across
/// features, so no per-item start field).
#[cfg(feature = "alloc")]
struct Eager {
    coords: Vec<(i32, i32)>,
    /// exclusive end into `coords` per ring
    ring_ends: Vec<u32>,
    /// per polygon (indexed by poly id): bbox + exclusive end into
    /// `ring_ends`. The bbox is computed while flattening (v4 dropped it
    /// from the payload) — it still skips whole-ring folds for candidates
    /// that touch the cell but not the point.
    polys: Vec<([i32; 4], u32)>,
}

/// A loaded timezone index. Build once, query many.
///
/// Availability follows the environment ladder (§11): `core` gets
/// [`from_static`](Finder::from_static) (zero-copy mode),
/// [`lookup`](Finder::lookup) and [`lookup_coarse`](Finder::lookup_coarse);
/// `alloc` adds owned/compressed containers (lazy mode) and
/// [`preload`](Finder::preload) (eager mode); `std` adds
/// [`from_reader`](Finder::from_reader).
pub struct Finder {
    payload: Payload,
    hdr: Header,
    /// eager-mode geometry, populated by `preload`
    #[cfg(feature = "alloc")]
    eager: Option<Eager>,
}

impl Finder {
    /// Load the preset selected by the (single) enabled preset feature.
    /// Cfg'd out when several presets are in the tree — load explicitly with
    /// `from_slice`/`from_static` on the statics in [`crate::data`] instead.
    /// `tiny-static` is the zero-copy one (`from_static`, bare `core`); the
    /// rest are compressed and load lazy (`from_slice`).
    #[cfg(all(
        feature = "tiny",
        not(any(
            feature = "tiny-static",
            feature = "compact",
            feature = "balanced",
            feature = "accurate"
        ))
    ))]
    pub fn new() -> Result<Finder, Error> {
        Finder::from_slice(crate::data::TINY)
    }
    /// Load the preset selected by the (single) enabled preset feature.
    /// Cfg'd out when several presets are in the tree — load explicitly with
    /// `from_slice`/`from_static` on the statics in [`crate::data`] instead.
    #[cfg(all(
        feature = "tiny-static",
        not(any(
            feature = "tiny",
            feature = "compact",
            feature = "balanced",
            feature = "accurate"
        ))
    ))]
    pub fn new() -> Result<Finder, Error> {
        Finder::from_static(crate::data::TINY_STATIC)
    }
    /// Load the preset selected by the (single) enabled preset feature.
    /// Cfg'd out when several presets are in the tree — load explicitly with
    /// `from_slice`/`from_static` on the statics in [`crate::data`] instead.
    #[cfg(all(
        feature = "compact",
        not(any(
            feature = "tiny",
            feature = "tiny-static",
            feature = "balanced",
            feature = "accurate"
        ))
    ))]
    pub fn new() -> Result<Finder, Error> {
        Finder::from_slice(crate::data::COMPACT)
    }
    /// Load the preset selected by the (single) enabled preset feature.
    /// Cfg'd out when several presets are in the tree — load explicitly with
    /// `from_slice`/`from_static` on the statics in [`crate::data`] instead.
    #[cfg(all(
        feature = "balanced",
        not(any(
            feature = "tiny",
            feature = "tiny-static",
            feature = "compact",
            feature = "accurate"
        ))
    ))]
    pub fn new() -> Result<Finder, Error> {
        Finder::from_slice(crate::data::BALANCED)
    }
    /// Load the preset selected by the (single) enabled preset feature.
    /// Cfg'd out when several presets are in the tree — load explicitly with
    /// `from_slice`/`from_static` on the statics in [`crate::data`] instead.
    #[cfg(all(
        feature = "accurate",
        not(any(
            feature = "tiny",
            feature = "tiny-static",
            feature = "compact",
            feature = "balanced"
        ))
    ))]
    pub fn new() -> Result<Finder, Error> {
        Finder::from_slice(crate::data::ACCURATE)
    }

    /// Borrow a container from `&'static` bytes (flash partition,
    /// `include_bytes!`, …) — zero-copy mode: no RAM payload. Only the
    /// `uncompressed` codec is accepted here.
    pub fn from_static(bytes: &'static [u8]) -> Result<Finder, Error> {
        let (codec, _, start) = format::outer(bytes)?;
        if codec != 0 {
            return Err(Error::Decompress); // compressed containers need an owned buffer
        }
        let payload = &bytes[start..];
        let hdr = format::parse(payload)?;
        Ok(Finder {
            payload: payload.into(),
            hdr,
            #[cfg(feature = "alloc")]
            eager: None,
        })
    }

    /// Decode a borrowed container into an owned `Finder` (lazy mode),
    /// decompressing per the codec byte. For compressed assets already in
    /// memory/flash (preset statics, OTA blobs) — no copy of the compressed
    /// input is made.
    #[cfg(feature = "alloc")]
    pub fn from_slice(bytes: &[u8]) -> Result<Finder, Error> {
        let (codec, raw_len, start) = format::outer(bytes)?;
        let payload = if codec == 0 {
            bytes[start..].to_vec()
        } else {
            decompress::decompress(codec, raw_len, &bytes[start..])?
        };
        let hdr = format::parse(&payload)?;
        Ok(Finder { payload: payload.into(), hdr, eager: None })
    }

    /// Take ownership of a container buffer (e.g. an OTA blob), decompressing
    /// per the codec byte if a backend is compiled in. The `no_std` entry
    /// point for compressed containers. Lazy mode either way: even an
    /// uncompressed owned buffer keeps the payload in RAM — zero-copy needs
    /// [`from_static`](Finder::from_static).
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
        Ok(Finder { payload: payload.into(), hdr, eager: None })
    }

    /// Decode straight to eager mode, then drop the geometry sections
    /// (PLAN §14.10): steady-state RAM is the eager cache plus only the
    /// header/tzid/grid tables — less than `from_slice` + [`preload`]
    /// keeping the full decoded payload (−17% on the compact preset), with
    /// no separate preload pass. Peak RAM during construction is unchanged
    /// (decoded payload and cache briefly coexist; the arc store must be
    /// resident to flatten rings). For a compressed asset in flash this is
    /// the eager entry point; for uncompressed assets prefer
    /// [`from_static`](Finder::from_static) + [`preload`](Finder::preload),
    /// which keeps the payload in flash entirely.
    #[cfg(feature = "alloc")]
    pub fn eager_from_slice(bytes: &[u8]) -> Result<Finder, Error> {
        let mut f = Finder::from_slice(bytes)?;
        f.preload();
        // keep [header + zone strings], [parent table] and [grid] —
        // everything lookups still read after preload; the arc store and
        // per-poly ring records between them are shadowed by the eager cache
        let (h, b) = (&f.hdr, &f.payload[..]);
        let arcs_off = h.arc_offsets - 4; // n_arcs u32 heads the arc block
        let parent_len = h.eager_polys as usize * 2;
        let grid_off = h.primary - 4; // ncols/nrows u16s head the grid block
        let mut p = Vec::with_capacity(arcs_off + parent_len + (b.len() - grid_off));
        p.extend_from_slice(&b[..arcs_off]);
        p.extend_from_slice(&b[h.parent..h.parent + parent_len]);
        p.extend_from_slice(&b[grid_off..]);
        let parent = arcs_off;
        let shift = grid_off - (arcs_off + parent_len);
        f.hdr.parent = parent;
        f.hdr.primary -= shift;
        f.hdr.list_offsets -= shift;
        f.hdr.list_ids -= shift;
        // poison the dropped sections' offsets: any residual use panics
        // out-of-bounds instead of reading grid bytes as geometry
        f.hdr.arc_offsets = usize::MAX;
        f.hdr.arc_data = usize::MAX;
        f.hdr.poly_offsets = usize::MAX;
        f.hdr.ring_data = usize::MAX;
        f.payload = p.into();
        Ok(f)
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
        core::str::from_utf8(format::release(&self.payload[..])).unwrap_or("")
    }

    /// Heap bytes [`preload`](Finder::preload) will reserve — the eager-cache
    /// size, straight from the v2 header counts. O(1); lets a constrained
    /// caller check fit before committing.
    #[cfg(feature = "alloc")]
    pub fn preload_bytes(&self) -> usize {
        let h = &self.hdr;
        h.eager_coords as usize * core::mem::size_of::<(i32, i32)>()
            + h.eager_rings as usize * core::mem::size_of::<u32>()
            + h.eager_polys as usize * core::mem::size_of::<([i32; 4], u32)>()
    }

    /// Decode all polygons into RAM once (eager mode, §9): repeat lookups
    /// then skip the per-arc varint decode. Costs [`preload_bytes`]
    /// (≈ uncompressed geometry re-widened to i32) in heap, reserved exactly
    /// up front from the v2 header counts — peak = final, no growth
    /// doubling. A no-op if already preloaded.
    #[cfg(feature = "alloc")]
    pub fn preload(&mut self) {
        if self.eager.is_some() {
            return;
        }
        let (h, b) = (&self.hdr, &self.payload[..]);
        let mut e = Eager {
            coords: Vec::with_capacity(h.eager_coords as usize),
            ring_ends: Vec::with_capacity(h.eager_rings as usize),
            polys: Vec::with_capacity(h.eager_polys as usize),
        };
        for pid in 0..h.eager_polys {
            let mut pos = h.ring_data + read_u32(b, h.poly_offsets + pid as usize * 4) as usize;
            let nrings = read_u16(b, pos);
            pos += 2;
            let poly_start = e.coords.len();
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
            // bbox over the flattened poly (v4: not in the payload)
            let mut bb = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
            for &(x, y) in &e.coords[poly_start..] {
                bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
            }
            e.polys.push((bb, e.ring_ends.len() as u32));
        }
        self.eager = Some(e);
    }

    /// Accurate lookup: grid cell → interior zone (O(1)) or candidates → PIP.
    ///
    /// Zero-copy/lazy Finders test candidates directly off the payload bytes
    /// (zero alloc); eager ones (after [`preload`](Finder::preload)) scan
    /// pre-decoded rings.
    pub fn lookup(&self, pos: Position) -> Option<&str> {
        let (px, py) = self.quantize(pos);
        match self.cell_value(px, py) {
            v if v == NO_ZONE => None,
            v if v & 0x8000 == 0 => self.tzid(v),
            v => {
                // border cell: candidates are the POLYS whose rings touch it
                // (v4) — resolve the winner's feature via the parent table
                let (s, e) = self.list_bounds(v & 0x7FFF);
                let b = &self.payload[..];
                let mut first = None;
                for pos in (s..e).step_by(2) {
                    let pid = read_u16(b, pos);
                    first.get_or_insert(pid);
                    if self.poly_contains(pid, px, py) {
                        return self.tzid(self.parent_of(pid));
                    }
                }
                // quantization edge: no candidate claims the point — the
                // dominant-first head is the best answer (measured ~0/100k)
                first.and_then(|pid| self.tzid(self.parent_of(pid)))
            }
        }
    }

    /// Grid-only approximate lookup: no geometry decoded, ~cell-size border
    /// error. Border cells answer with the cell's dominant zone.
    pub fn lookup_coarse(&self, pos: Position) -> Option<&str> {
        let (px, py) = self.quantize(pos);
        match self.cell_value(px, py) {
            v if v == NO_ZONE => None,
            v if v & 0x8000 == 0 => self.tzid(v),
            v => {
                let (s, _) = self.list_bounds(v & 0x7FFF);
                // dominant-first head (a poly id in v4)
                self.tzid(self.parent_of(read_u16(&self.payload[..], s)))
            }
        }
    }

    fn qmax(&self) -> f64 {
        ((1u64 << (self.hdr.quant_bits - 1)) - 1) as f64
    }
    fn quantize(&self, pos: Position) -> (i32, i32) {
        // round-half-away like the encoder (f64::round is std-only)
        let r = |v: f64| (v + if v >= 0.0 { 0.5 } else { -0.5 }) as i32;
        let q = self.qmax();
        (r(pos.lon / 180.0 * q), r(pos.lat / 90.0 * q))
    }

    fn cell_value(&self, px: i32, py: i32) -> u16 {
        let (h, q) = (&self.hdr, self.qmax());
        let d = h.grid_deg as f64;
        let lon = px as f64 / q * 180.0;
        let lat = py as f64 / q * 90.0;
        let c = (((lon + 180.0) / d) as i64).clamp(0, h.ncols as i64 - 1) as usize;
        let r = (((lat + 90.0) / d) as i64).clamp(0, h.nrows as i64 - 1) as usize;
        read_u16(&self.payload[..], h.primary + (r * h.ncols as usize + c) * 2)
    }

    fn list_bounds(&self, li: u16) -> (usize, usize) {
        let (h, b) = (&self.hdr, &self.payload[..]);
        let s = read_u16(b, h.list_offsets + li as usize * 2) as usize;
        let e = read_u16(b, h.list_offsets + li as usize * 2 + 2) as usize;
        (h.list_ids + s * 2, h.list_ids + e * 2)
    }

    fn tzid(&self, fid: u16) -> Option<&str> {
        let (h, b) = (&self.hdr, &self.payload[..]);
        let s = read_u16(b, h.str_offsets + fid as usize * 2) as usize;
        let e = read_u16(b, h.str_offsets + fid as usize * 2 + 2) as usize;
        core::str::from_utf8(&b[h.pool + s..h.pool + e]).ok().filter(|t| !t.is_empty())
    }

    /// poly id → feature id (v4 parent table).
    fn parent_of(&self, pid: u16) -> u16 {
        read_u16(&self.payload[..], self.hdr.parent + pid as usize * 2)
    }

    /// Per-polygon even-odd PIP at the width the header demands. Grid
    /// candidates are polys (v4), already localized to the cell — no bbox
    /// pre-test. Lazy path streams the arcs straight off the container bytes
    /// through the per-edge kernel (§14.7): junction vertices are shared by
    /// consecutive arcs and the ring closure is a shared junction too, so the
    /// ring's segment set is exactly the union of each arc's internal
    /// segments — every arc is walked FORWARD (orientation bit ignored) with
    /// O(1) state, and parity XORs across arcs order-independently.
    fn poly_contains(&self, pid: u16, px: i32, py: i32) -> bool {
        #[cfg(feature = "alloc")]
        if let Some(e) = &self.eager {
            return self.eager_poly_contains(e, pid, px, py);
        }
        let (h, b) = (&self.hdr, &self.payload[..]);
        let mut pos = h.ring_data + read_u32(b, h.poly_offsets + pid as usize * 4) as usize;
        let nrings = read_u16(b, pos);
        pos += 2;
        let mut poly_inside = false;
        for _ in 0..nrings {
            let (nrefs, mut p2) = read_varint(b, pos);
            let mut ring_inside = false;
            for _ in 0..nrefs {
                let (r, p3) = read_varint(b, p2);
                p2 = p3;
                match self.scan_arc((r >> 1) as usize, px, py) {
                    pip::RingHit::Boundary => return true, // border points claimed
                    pip::RingHit::Inside => ring_inside = !ring_inside,
                    pip::RingHit::Outside => {}
                }
            }
            pos = p2;
            if ring_inside {
                poly_inside = !poly_inside;
            }
        }
        poly_inside
    }

    /// Fold one arc's internal segments through the edge kernel. `Inside` =
    /// this arc contributed an odd number of ray crossings.
    fn scan_arc(&self, id: usize, px: i32, py: i32) -> pip::RingHit {
        let (h, b) = (&self.hdr, &self.payload[..]);
        let wide = h.quant_bits == 32;
        let fixed = h.geom == 1;
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
            let (x1, y1) = if fixed {
                let x1 = read_fixed(b, pos, h.quant_bits);
                let y1 = read_fixed(b, pos + fb, h.quant_bits);
                pos += 2 * fb;
                (x1, y1)
            } else {
                let (dx, p3) = read_varint(b, pos);
                let (dy, p4) = read_varint(b, p3);
                pos = p4;
                x += unzigzag(dx);
                y += unzigzag(dy);
                (x as i32, y as i32)
            };
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

    /// Eager path: same even-odd fold over the pre-decoded poly (indexed
    /// directly by poly id). The preload-computed bbox still skips whole
    /// folds for candidates that touch the cell but not the point.
    #[cfg(feature = "alloc")]
    fn eager_poly_contains(&self, e: &Eager, pid: u16, px: i32, py: i32) -> bool {
        let wide = self.hdr.quant_bits == 32;
        let pi = pid as usize;
        let (bb, rend) = e.polys[pi];
        if !(px >= bb[0] && py >= bb[1] && px <= bb[2] && py <= bb[3]) {
            return false;
        }
        let rstart = if pi == 0 { 0 } else { e.polys[pi - 1].1 as usize };
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
        inside
    }

    /// Decode one signed arc ref onto the end of `coords` (join-deduplicated).
    /// Eager-mode decode only; the lazy path streams via `scan_arc` instead.
    #[cfg(feature = "alloc")]
    fn append_arc(&self, r: u32, coords: &mut Vec<(i32, i32)>) {
        let (h, b) = (&self.hdr, &self.payload[..]);
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
            if h.geom == 1 {
                coords.push((read_fixed(b, pos, h.quant_bits), read_fixed(b, pos + fb, h.quant_bits)));
                pos += 2 * fb;
            } else {
                let (dx, p3) = read_varint(b, pos);
                let (dy, p4) = read_varint(b, p3);
                pos = p4;
                x += unzigzag(dx);
                y += unzigzag(dy);
                coords.push((x as i32, y as i32));
            }
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
