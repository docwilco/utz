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

// EagerImage casts payload bytes to coordinate pairs — pin the layout
const _: () = assert!(
    core::mem::size_of::<(i32, i32)>() == 8 && core::mem::align_of::<(i32, i32)>() == 4
);
const _: () = assert!(
    core::mem::size_of::<(i16, i16)>() == 4 && core::mem::align_of::<(i16, i16)>() == 2
);
#[cfg(feature = "geom-image")]
const _: () = assert!(
    core::mem::size_of::<crate::pip::Pack24>() == 6
        && core::mem::align_of::<crate::pip::Pack24>() == 1
);

/// `EagerImage` load-time check: the coords are read via typed slice casts,
/// so the payload must land them 4-aligned (static assets:
/// [`crate::include_bytes_aligned!`]`(4, ..)`). Endianness is a compile-time
/// refusal —
/// see the `geom-image` `compile_error` in lib.rs.
fn check_image(payload: &[u8], hdr: &Header) -> Result<(), Error> {
    #[cfg(feature = "geom-image")]
    if hdr.geom == 2 && !(payload.as_ptr() as usize + hdr.img_coords).is_multiple_of(4) {
        return Err(Error::Misaligned);
    }
    #[cfg(not(feature = "geom-image"))]
    let _ = (payload, hdr);
    Ok(())
}

/// Eager-mode storage: every ring decoded, flat (§9). Ranges are exclusive
/// ends; a range's start is the previous entry's end (global across
/// features, so no per-item start field).
#[cfg(feature = "alloc")]
struct Eager {
    coords: EagerCoords,
    /// exclusive end into `coords` per ring
    ring_ends: Vec<u32>,
    /// per polygon (indexed by poly id): bbox (read from the v5 record) +
    /// exclusive end into `ring_ends`. The bbox skips whole-ring folds for
    /// candidates that touch the cell but not the point.
    polys: Vec<([i32; 4], u32)>,
}

/// Per-polygon eager records: bbox + exclusive `ring_ends` end (see
/// [`Eager::polys`]).
#[cfg(feature = "alloc")]
type Polys = Vec<([i32; 4], u32)>;

/// The eager cache's coordinate store, at quant-nearest width (§14.11):
/// i16-quant assets keep i16 pairs — half the cache RAM — and PIP widens
/// per edge inside the kernel (still to i64: crosses of i16 coords reach
/// 2^33, see the pip module docs).
#[cfg(feature = "alloc")]
enum EagerCoords {
    Narrow(Vec<(i16, i16)>),
    Wide(Vec<(i32, i32)>),
}

/// Eager-cache element construction: narrow a decoded (i32-accumulated)
/// quant coordinate to the storage width. `PartialEq` powers the arc-join
/// and ring-closure vertex dedup during decode.
#[cfg(feature = "alloc")]
trait EagerCoord: pip::CoordPair + PartialEq {
    fn from_q(x: i32, y: i32) -> Self;
}
#[cfg(feature = "alloc")]
impl EagerCoord for (i32, i32) {
    fn from_q(x: i32, y: i32) -> Self {
        (x, y)
    }
}
#[cfg(feature = "alloc")]
impl EagerCoord for (i16, i16) {
    #[expect(clippy::cast_possible_truncation, reason = "dispatched only for quant_bits==16 assets, whose coords fit i16 by format")]
    fn from_q(x: i32, y: i32) -> Self {
        (x as i16, y as i16)
    }
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
    ///
    /// # Errors
    /// As [`Finder::from_slice`] on the baked preset asset.
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
    ///
    /// # Errors
    /// As [`Finder::from_static`] on the baked preset asset.
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
    ///
    /// # Errors
    /// As [`Finder::from_slice`] on the baked preset asset.
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
    ///
    /// # Errors
    /// As [`Finder::from_slice`] on the baked preset asset.
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
    ///
    /// # Errors
    /// As [`Finder::from_slice`] on the baked preset asset.
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
    ///
    /// # Errors
    /// [`Error::Decompress`] if the container is compressed;
    /// [`Error::BadFormat`]/[`Error::Geometry`] for an invalid container or
    /// header; [`Error::Misaligned`] for unaligned `EagerImage` coords.
    pub fn from_static(bytes: &'static [u8]) -> Result<Finder, Error> {
        let (codec, _, start) = format::outer(bytes)?;
        if codec != 0 {
            return Err(Error::Decompress); // compressed containers need an owned buffer
        }
        let payload = &bytes[start..];
        let hdr = format::parse(payload)?;
        check_image(payload, &hdr)?;
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
    ///
    /// # Errors
    /// [`Error::BadFormat`]/[`Error::Geometry`] for an invalid container or
    /// header; [`Error::Decompress`] if the codec isn't compiled in or the
    /// payload fails to decode; [`Error::Misaligned`] for unaligned
    /// `EagerImage` coords.
    #[cfg(feature = "alloc")]
    pub fn from_slice(bytes: &[u8]) -> Result<Finder, Error> {
        let (codec, raw_len, start) = format::outer(bytes)?;
        let payload = if codec == 0 {
            bytes[start..].to_vec()
        } else {
            decompress::decompress(codec, raw_len, &bytes[start..])?
        };
        let hdr = format::parse(&payload)?;
        check_image(&payload, &hdr)?;
        Ok(Finder { payload: payload.into(), hdr, eager: None })
    }

    /// Take ownership of a container buffer (e.g. an OTA blob), decompressing
    /// per the codec byte if a backend is compiled in. The `no_std` entry
    /// point for compressed containers. Lazy mode either way: even an
    /// uncompressed owned buffer keeps the payload in RAM — zero-copy needs
    /// [`from_static`](Finder::from_static).
    ///
    /// # Errors
    /// As [`Finder::from_slice`].
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
        check_image(&payload, &hdr)?;
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
    ///
    /// # Errors
    /// As [`Finder::from_slice`], which performs the load.
    #[cfg(feature = "alloc")]
    pub fn eager_from_slice(bytes: &[u8]) -> Result<Finder, Error> {
        let mut f = Finder::from_slice(bytes)?;
        if f.hdr.geom >= 2 {
            return Ok(f); // EagerImage/coarse: nothing further to decode
        }
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
    ///
    /// # Errors
    /// [`Error::BadFormat`] if reading fails; otherwise as
    /// [`Finder::from_vec`].
    #[cfg(feature = "std")]
    pub fn from_reader(mut r: impl std::io::Read) -> Result<Finder, Error> {
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes).map_err(|_| Error::BadFormat)?;
        Finder::from_vec(bytes)
    }

    /// TZBB release recorded in the container header.
    #[must_use]
    pub fn tzbb_release(&self) -> &str {
        core::str::from_utf8(format::release(&self.payload[..])).unwrap_or("")
    }

    /// Heap bytes [`preload`](Finder::preload) will reserve — the eager-cache
    /// size, straight from the v2 header counts. O(1); lets a constrained
    /// caller check fit before committing.
    #[cfg(feature = "alloc")]
    #[must_use]
    pub fn preload_bytes(&self) -> usize {
        if self.hdr.geom >= 2 {
            return 0; // EagerImage / coarse: nothing to decode
        }
        let h = &self.hdr;
        // coords are cached at quant-nearest width (§14.11)
        let pair = if h.quant_bits == 16 {
            core::mem::size_of::<(i16, i16)>()
        } else {
            core::mem::size_of::<(i32, i32)>()
        };
        h.eager_coords as usize * pair
            + h.eager_rings as usize * core::mem::size_of::<u32>()
            + h.eager_polys as usize * core::mem::size_of::<([i32; 4], u32)>()
    }

    /// Decode all polygons into RAM once (eager mode, §9): repeat lookups
    /// then skip the per-arc varint decode. Costs [`preload_bytes`]
    /// (≈ uncompressed geometry at quant-nearest width: i16 pairs for
    /// i16-quant assets — half the cache — i32 otherwise, §14.11) in heap,
    /// reserved exactly up front from the v2 header counts — peak = final,
    /// no growth doubling. A no-op if already preloaded.
    #[cfg(feature = "alloc")]
    pub fn preload(&mut self) {
        if self.eager.is_some() || self.hdr.geom >= 2 {
            // geom=2 (EagerImage): the payload already IS the cache;
            // geom=3 (coarse): nothing to decode
            return;
        }
        self.eager = Some(if self.hdr.quant_bits == 16 {
            let (coords, ring_ends, polys) = self.decode_rings::<(i16, i16)>();
            Eager { coords: EagerCoords::Narrow(coords), ring_ends, polys }
        } else {
            let (coords, ring_ends, polys) = self.decode_rings::<(i32, i32)>();
            Eager { coords: EagerCoords::Wide(coords), ring_ends, polys }
        });
    }

    /// [`preload`](Finder::preload)'s decode pass, generic over the cache's
    /// coordinate width.
    #[cfg(feature = "alloc")]
    #[expect(clippy::cast_possible_truncation, reason = "counts bounded by the parse-validated u32 header reservations")]
    fn decode_rings<C: EagerCoord>(&self) -> (Vec<C>, Vec<u32>, Polys) {
        let (h, b) = (&self.hdr, &self.payload[..]);
        let mut coords = Vec::with_capacity(h.eager_coords as usize);
        let mut ring_ends = Vec::with_capacity(h.eager_rings as usize);
        let mut polys = Vec::with_capacity(h.eager_polys as usize);
        let fb = fixed_bytes(h.quant_bits);
        for pid in 0..h.eager_polys {
            let mut pos = h.ring_data + read_u32(b, h.poly_offsets + pid as usize * 4) as usize;
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
                let start = coords.len();
                for _ in 0..nrefs {
                    let (r, p3) = read_varint(b, p2);
                    p2 = p3;
                    self.append_arc(r as u32, &mut coords);
                }
                pos = p2;
                // drop the duplicated ring-closure vertex (ring_hit wraps)
                if coords.len() > start + 1 && coords.last() == coords.get(start) {
                    coords.pop();
                }
                ring_ends.push(coords.len() as u32);
            }
            polys.push((bb, ring_ends.len() as u32));
        }
        (coords, ring_ends, polys)
    }

    /// Accurate lookup: grid cell → interior zone (O(1)) or candidates → PIP.
    ///
    /// Zero-copy/lazy Finders test candidates directly off the payload bytes
    /// (zero alloc); eager ones (after [`preload`](Finder::preload)) scan
    /// pre-decoded rings.
    #[must_use]
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
                // coarse assets carry no geometry: cell precision IS the
                // asset's precision — the dominant-first head is the answer
                if cfg!(feature = "geom-coarse") && self.hdr.geom == 3 {
                    return self.tzid(self.parent_of(read_u16(b, s)));
                }
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
    #[must_use]
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

    #[expect(clippy::cast_precision_loss, reason = "qmax = 2^(quant_bits-1)-1 ≤ 2^31-1, exact in f64")]
    fn qmax(&self) -> f64 {
        ((1u64 << (self.hdr.quant_bits - 1)) - 1) as f64
    }
    #[expect(clippy::cast_possible_truncation, reason = "|v*qmax| < i32::MAX for in-range lon/lat; float as saturates, wild input degrades to a miss")]
    fn quantize(&self, pos: Position) -> (i32, i32) {
        // round-half-away like the encoder (f64::round is std-only)
        let r = |v: f64| (v + if v >= 0.0 { 0.5 } else { -0.5 }) as i32;
        let q = self.qmax();
        (r(pos.lon / 180.0 * q), r(pos.lat / 90.0 * q))
    }

    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "cast saturates then clamped to grid range")]
    fn cell_value(&self, px: i32, py: i32) -> u16 {
        let (header, qmax) = (&self.hdr, self.qmax());
        let cell_deg = f64::from(header.grid_deg);
        let lon = f64::from(px) / qmax * 180.0;
        let lat = f64::from(py) / qmax * 90.0;
        let col = (((lon + 180.0) / cell_deg) as i64).clamp(0, i64::from(header.ncols) - 1) as usize;
        let row = (((lat + 90.0) / cell_deg) as i64).clamp(0, i64::from(header.nrows) - 1) as usize;
        read_u16(&self.payload[..], header.primary + (row * header.ncols as usize + col) * 2)
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

    /// Per-polygon test: bbox gate, then even-odd PIP at the width the
    /// header demands. Grid candidates are polys (v4) localized to the
    /// CELL; the record's bbox (v5) is the point-granular refinement — a
    /// miss returns before touching any arc. Lazy path streams the arcs
    /// straight off the container bytes through the per-edge kernel (§14.7):
    /// junction vertices are shared by consecutive arcs and the ring closure
    /// is a shared junction too, so the ring's segment set is exactly the
    /// union of each arc's internal segments — every arc is walked FORWARD
    /// (orientation bit ignored) with O(1) state, and parity XORs across
    /// arcs order-independently.
    fn poly_contains(&self, pid: u16, px: i32, py: i32) -> bool {
        #[cfg(feature = "geom-image")]
        if self.hdr.geom == 2 {
            return self.image_poly_contains(pid, px, py);
        }
        #[cfg(feature = "alloc")]
        if let Some(e) = &self.eager {
            return self.eager_poly_contains(e, pid, px, py);
        }
        let (h, b) = (&self.hdr, &self.payload[..]);
        let fb = fixed_bytes(h.quant_bits);
        let mut pos = h.ring_data + read_u32(b, h.poly_offsets + pid as usize * 4) as usize;
        let bb = [
            read_fixed(b, pos, h.quant_bits),
            read_fixed(b, pos + fb, h.quant_bits),
            read_fixed(b, pos + 2 * fb, h.quant_bits),
            read_fixed(b, pos + 3 * fb, h.quant_bits),
        ];
        if !(px >= bb[0] && py >= bb[1] && px <= bb[2] && py <= bb[3]) {
            return false;
        }
        pos += 4 * fb;
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
    #[expect(clippy::cast_possible_truncation, reason = "coords accumulate i16/i24/i32-width deltas; sums fit i32 by format")]
    fn scan_arc(&self, id: usize, px: i32, py: i32) -> pip::RingHit {
        let (h, b) = (&self.hdr, &self.payload[..]);
        let wide = h.quant_bits == 32;
        let fixed = cfg!(feature = "geom-fixed") && h.geom == 1;
        let mut pos = h.arc_data + read_u32(b, h.arc_offsets + id * 4) as usize;
        let (vcount, p2) = read_varint(b, pos);
        pos = p2;
        let fb = fixed_bytes(h.quant_bits);
        let mut x = i64::from(read_fixed(b, pos, h.quant_bits));
        let mut y = i64::from(read_fixed(b, pos + fb, h.quant_bits));
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
                pip::edge::<i128, _>((x0, y0), (x1, y1), px, py)
            } else {
                pip::edge::<i64, _>((x0, y0), (x1, y1), px, py)
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

    /// `EagerImage` path (geom=2): the payload geometry IS the eager cache —
    /// one generic slice kernel folds straight off the payload bytes (flash
    /// in zero-copy mode). Coord width follows the quant width (v7): i16 /
    /// i32 as typed pairs, i24 as [`pip::Pack24`] (align 1 — no alignment
    /// requirement). Works on the bare `core` rung.
    #[cfg(feature = "geom-image")]
    fn image_poly_contains(&self, pid: u16, px: i32, py: i32) -> bool {
        let (h, b) = (&self.hdr, &self.payload[..]);
        let pe = h.img_polys + pid as usize * 20;
        let bb = [
            read_u32(b, pe).cast_signed(),
            read_u32(b, pe + 4).cast_signed(),
            read_u32(b, pe + 8).cast_signed(),
            read_u32(b, pe + 12).cast_signed(),
        ];
        if !(px >= bb[0] && py >= bb[1] && px <= bb[2] && py <= bb[3]) {
            return false;
        }
        let rend = read_u32(b, pe + 16) as usize;
        let rstart = if pid == 0 { 0 } else { read_u32(b, pe - 4) as usize };
        match h.quant_bits {
            16 => {
                // an in-bbox point of a valid i16-quant asset always fits
                // i16; the fallthrough covers adversarial bboxes
                let (Ok(px), Ok(py)) = (i16::try_from(px), i16::try_from(py)) else {
                    return false;
                };
                self.image_rings(rstart, rend, px, py, ring_hit_narrow)
            }
            24 => self.image_rings(rstart, rend, px, py, pip::ring_hit::<i64, pip::Pack24>),
            _ => self.image_rings(rstart, rend, px, py, pip::ring_hit::<i128, (i32, i32)>),
        }
    }

    /// Even-odd fold over one image poly's rings `[rstart, rend)` at pair
    /// type `P` — `size_of::<P>()` IS the on-image coordinate stride; `scan`
    /// is the width-matched ring kernel ([`pip::ring_hit`] at the right wide
    /// type, or [`pip::ring_hit_i16`]).
    /// (No `cast_ptr_alignment` expect needed anymore: the cast target is
    /// the opaque `P`, so the lint can't see a concrete alignment — the
    /// invariant itself is stated in the SAFETY comment below.)
    #[cfg(feature = "geom-image")]
    fn image_rings<P: pip::CoordPair>(
        &self,
        rstart: usize,
        rend: usize,
        px: P::Narrow,
        py: P::Narrow,
        scan: impl Fn(&[P], P::Narrow, P::Narrow) -> pip::RingHit,
    ) -> bool {
        let (h, b) = (&self.hdr, &self.payload[..]);
        let mut inside = false;
        let mut cstart =
            if rstart == 0 { 0 } else { read_u32(b, h.img_ring_ends + (rstart - 1) * 4) as usize };
        for ri in rstart..rend {
            let cend = read_u32(b, h.img_ring_ends + ri * 4) as usize;
            let n = cend - cstart;
            // SAFETY (slice cast): pair layouts are asserted at the top of
            // this file (Pack24 is align 1; i16/i32 pairs land aligned
            // because img_coords is 4-aligned — checked at load — and their
            // strides are multiples of the element alignment); parse bounds
            // the image sections against the header counts.
            let ring = unsafe {
                core::slice::from_raw_parts(
                    b[h.img_coords + cstart * core::mem::size_of::<P>()..].as_ptr().cast::<P>(),
                    n,
                )
            };
            cstart = cend;
            match scan(ring, px, py) {
                pip::RingHit::Boundary => return true,
                pip::RingHit::Inside => inside = !inside,
                pip::RingHit::Outside => {}
            }
        }
        inside
    }

    /// Eager path: same even-odd fold over the pre-decoded poly (indexed
    /// directly by poly id). The preload-computed bbox still skips whole
    /// folds for candidates that touch the cell but not the point.
    #[cfg(feature = "alloc")]
    fn eager_poly_contains(&self, e: &Eager, pid: u16, px: i32, py: i32) -> bool {
        let pi = pid as usize;
        let (bb, rend) = e.polys[pi];
        if !(px >= bb[0] && py >= bb[1] && px <= bb[2] && py <= bb[3]) {
            return false;
        }
        let rstart = if pi == 0 { 0 } else { e.polys[pi - 1].1 as usize };
        match &e.coords {
            EagerCoords::Narrow(coords) => {
                // an in-bbox point of a valid i16-quant asset always fits
                // i16; the fallthrough covers adversarial bboxes
                let (Ok(px), Ok(py)) = (i16::try_from(px), i16::try_from(py)) else {
                    return false;
                };
                rings_hit(coords, &e.ring_ends, rstart, rend as usize, px, py, ring_hit_narrow)
            }
            EagerCoords::Wide(coords) if self.hdr.quant_bits == 32 => rings_hit(
                coords,
                &e.ring_ends,
                rstart,
                rend as usize,
                px,
                py,
                pip::ring_hit::<i128, (i32, i32)>,
            ),
            EagerCoords::Wide(coords) => rings_hit(
                coords,
                &e.ring_ends,
                rstart,
                rend as usize,
                px,
                py,
                pip::ring_hit::<i64, (i32, i32)>,
            ),
        }
    }

    /// Decode one signed arc ref onto the end of `coords` (join-deduplicated).
    /// Eager-mode decode only; the lazy path streams via `scan_arc` instead.
    #[cfg(feature = "alloc")]
    #[expect(clippy::cast_possible_truncation, reason = "coords accumulate i16/i24/i32-width deltas; sums fit i32 by format")]
    fn append_arc<C: EagerCoord>(&self, arc_ref: u32, coords: &mut Vec<C>) {
        let (header, payload) = (&self.hdr, &self.payload[..]);
        let (id, rev) = ((arc_ref >> 1) as usize, (arc_ref & 1) == 1);
        let mut pos = header.arc_data + read_u32(payload, header.arc_offsets + id * 4) as usize;
        let (vcount, after_vcount) = read_varint(payload, pos);
        pos = after_vcount;
        let coord_bytes = fixed_bytes(header.quant_bits);
        let mut qlon = i64::from(read_fixed(payload, pos, header.quant_bits));
        let mut qlat = i64::from(read_fixed(payload, pos + coord_bytes, header.quant_bits));
        pos += 2 * coord_bytes;
        let start = coords.len();
        coords.push(C::from_q(qlon as i32, qlat as i32));
        for _ in 1..vcount {
            if cfg!(feature = "geom-fixed") && header.geom == 1 {
                coords.push(C::from_q(
                    read_fixed(payload, pos, header.quant_bits),
                    read_fixed(payload, pos + coord_bytes, header.quant_bits),
                ));
                pos += 2 * coord_bytes;
            } else {
                let (dlon, after_dlon) = read_varint(payload, pos);
                let (dlat, after_dlat) = read_varint(payload, after_dlon);
                pos = after_dlat;
                qlon += unzigzag(dlon);
                qlat += unzigzag(dlat);
                coords.push(C::from_q(qlon as i32, qlat as i32));
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

/// The scan kernel for i16-quant rings (§14.11/§15): the u32 sign-split
/// kernel on 32-bit targets (0.75× the i64 kernel on the ESP32-S3 — wide
/// multiplies are instruction pairs there), the generic i64 kernel on
/// 64-bit ones (single-instruction wide multiplies; the sign-split
/// branches measured 2.3× SLOWER on `x86_64`). Ring verdicts are identical
/// either way, so answers stay platform-independent.
#[cfg(any(feature = "alloc", feature = "geom-image"))]
fn ring_hit_narrow(ring: &[(i16, i16)], px: i16, py: i16) -> pip::RingHit {
    #[cfg(target_pointer_width = "32")]
    return pip::ring_hit_i16(ring, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::ring_hit::<i64, _>(ring, px, py)
}

/// Even-odd fold over consecutive rings `[rstart, rend)` of a flat eager
/// cache — shared by both cache widths ([`EagerCoords`]); `scan` is the
/// width-matched ring kernel ([`pip::ring_hit`] at the right wide type, or
/// [`ring_hit_narrow`]).
#[cfg(feature = "alloc")]
fn rings_hit<P: pip::CoordPair>(
    coords: &[P],
    ring_ends: &[u32],
    rstart: usize,
    rend: usize,
    px: P::Narrow,
    py: P::Narrow,
    scan: impl Fn(&[P], P::Narrow, P::Narrow) -> pip::RingHit,
) -> bool {
    let mut inside = false;
    let mut cstart = if rstart == 0 { 0 } else { ring_ends[rstart - 1] as usize };
    for cend in &ring_ends[rstart..rend] {
        let cend = *cend as usize;
        match scan(&coords[cstart..cend], px, py) {
            pip::RingHit::Boundary => return true,
            pip::RingHit::Inside => inside = !inside,
            pip::RingHit::Outside => {}
        }
        cstart = cend;
    }
    inside
}
