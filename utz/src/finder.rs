//! `Finder`: grid prefilter → per-polygon integer PIP.
//!
//! Three memory modes, selected automatically by how the container is loaded
//! (only eager is an explicit request); availability falls out of the
//! environment rung:
//! - **zero-copy** (`from_static`, uncompressed): the payload is borrowed
//!   from static storage (flash partition, `include_bytes!`, …). No RAM
//!   payload at all; flash pays the uncompressed size. `core` rung.
//! - **lazy** (`from_slice`/`from_vec`/`from_reader`): the payload lives in
//!   owned RAM, typically because the asset is compressed and flash can't
//!   fit it uncompressed. No decoded-geometry cache: RAM = the decompressed
//!   payload, nothing more.
//! - **eager** ([`Finder::preload()`], `alloc`): additionally decode all rings
//!   into RAM once; lookups then scan decoded slices. Most RAM, fastest
//!   repeat lookups.
//!
//! Zero-copy and lazy share the identical lookup mechanism: candidates are
//! PIP-tested by walking their arcs directly off the payload bytes through
//! the per-edge kernel, O(1) state, no allocation. They differ only in
//! where the payload resides (borrowed static vs owned RAM), i.e. the
//! `Cow` variant of [`Payload`]. Interior cells touch zero geometry in every
//! mode, and [`Finder::lookup_coarse()`] never touches geometry at all.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use crate::decompress;
use crate::format::{self, read_fixed, read_u16, read_u32, read_varint, unzigzag, PayloadLayout};
use crate::{pip, Codec, Error, Result};
use utz_common::{GeomEncoding, QuantBits, NO_ZONE};

/// A geographic position in degrees, **order-neutral by design**:
/// construct with named fields, so there is no argument order to get wrong,
/// only values. `Position { lat: 51.5, lon: -0.13 }` and
/// `Position { lon: -0.13, lat: 51.5 }` are the same position.
///
/// Deliberately no positional constructor and no `From<(f64, f64)>`: either
/// would reintroduce the lon/lat-swap footgun this type exists to kill.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Position {
    /// longitude in degrees, −180..=180 (x)
    pub lon: f64,
    /// latitude in degrees, −90..=90 (y)
    pub lat: f64,
}

/// The container's payload section. `Cow::Borrowed` = zero-copy mode,
/// `Cow::Owned` = lazy/eager. `Cow` itself lives in `alloc`; on the
/// bare-`core` rung borrowed is the only possible variant.
#[cfg(feature = "alloc")]
type Payload = alloc::borrow::Cow<'static, [u8]>;
#[cfg(not(feature = "alloc"))]
type Payload = &'static [u8];

// FullRings casts payload bytes to coordinate pairs — pin the layout
const _: () =
    assert!(core::mem::size_of::<(i32, i32)>() == 8 && core::mem::align_of::<(i32, i32)>() == 4);
const _: () =
    assert!(core::mem::size_of::<(i16, i16)>() == 4 && core::mem::align_of::<(i16, i16)>() == 2);
#[cfg(feature = "geom-full-rings")]
const _: () = assert!(
    core::mem::size_of::<crate::pip::Pack24>() == 6
        && core::mem::align_of::<crate::pip::Pack24>() == 1
);

/// `FullRings` load-time check: the coords are read via typed slice casts,
/// so the payload must land them 4-aligned (static assets:
/// [`crate::include_bytes_aligned!`]`(4, ..)`). Endianness is a compile-time
/// refusal (see the `geom-full-rings` `compile_error` in lib.rs).
#[cfg_attr(
    not(feature = "geom-full-rings"),
    expect(
        clippy::unnecessary_wraps,
        reason = "the alignment check only exists on the geom-full-rings rung; the signature stays uniform"
    )
)]
fn check_full_rings(payload: &[u8], layout: &PayloadLayout) -> Result<()> {
    #[cfg(feature = "geom-full-rings")]
    if layout.geom == GeomEncoding::FullRings {
        if !(payload.as_ptr() as usize + layout.full_coords).is_multiple_of(4) {
            return Err(Error::Misaligned);
        }
        // the full-rings sections are self-delimiting — the counts must agree
        // (post-decompression: the header can't vouch for section bytes)
        if layout.eager_rings > 0
            && read_u32(
                payload,
                layout.full_ring_ends + (layout.eager_rings as usize - 1) * 4,
            ) != layout.eager_coords
        {
            return Err(Error::FullRingsCountsDisagree);
        }
    }
    #[cfg(not(feature = "geom-full-rings"))]
    let _ = (payload, layout);
    Ok(())
}

/// Eager-mode storage: every ring decoded, flat. Ranges are exclusive
/// ends; a range's start is the previous entry's end (global across
/// features, so no per-item start field).
#[cfg(feature = "alloc")]
struct Eager {
    coords: EagerCoords,
    /// exclusive end into `coords` per ring
    ring_ends: Vec<u32>,
    /// per polygon (indexed by poly id): bbox (read from the poly record) +
    /// exclusive end into `ring_ends`. The bbox skips whole-ring folds for
    /// candidates that touch the cell but not the point.
    polys: Vec<([i32; 4], u32)>,
}

/// Per-polygon eager records: bbox + exclusive `ring_ends` end (see
/// [`Eager::polys`]).
#[cfg(feature = "alloc")]
type Polys = Vec<([i32; 4], u32)>;

/// The eager cache's coordinate store, at quant-nearest width:
/// i16-quant assets keep i16 pairs (half the cache RAM) and PIP widens
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
    #[expect(
        clippy::cast_possible_truncation,
        reason = "dispatched only for quant_bits==16 assets, whose coords fit i16 by format"
    )]
    fn from_q(x: i32, y: i32) -> Self {
        (x as i16, y as i16)
    }
}

/// A loaded timezone index. Build once, query many.
///
/// Availability follows the environment ladder; each row adds to the
/// ones above it:
///
/// | feature | adds mode    | API |
/// |---------|--------------|-----|
/// | `core`  | zero-copy    | [`from_static`](Finder::from_static), [`lookup`](Finder::lookup), [`lookup_coarse`](Finder::lookup_coarse) |
/// | `alloc` | lazy + eager | [`from_slice`](Finder::from_slice), [`from_vec`](Finder::from_vec), [`eager_from_slice`](Finder::eager_from_slice), [`preload`](Finder::preload), [`preload_bytes`](Finder::preload_bytes) |
/// | `std`   |              | [`from_reader`](Finder::from_reader) |
pub struct Finder {
    payload: Payload,
    layout: PayloadLayout,
    /// eager-mode geometry, populated by `preload`
    #[cfg(feature = "alloc")]
    eager: Option<Eager>,
}

impl Finder {
    /// Load the preset selected by the (single) enabled preset feature.
    /// Documented here always, but cfg'd out of real builds when zero or
    /// several presets are in the tree — there, load explicitly with
    /// `from_slice`/`from_static` on the statics in [`crate::data`] instead.
    /// `tiny-static` is the zero-copy one (`from_static`, bare `core`); the
    /// rest are compressed and load lazy (`from_slice`).
    ///
    /// # Example
    ///
    // The fence toggles so the example runs as a real doctest in the
    // exactly-one-preset std+tiny build; doctests link the regular
    // library build, so no other feature row can compile this call.
    #[cfg_attr(
        all(
            feature = "std",
            feature = "tiny",
            not(any(
                feature = "tiny-static",
                feature = "compact",
                feature = "balanced",
                feature = "accurate"
            ))
        ),
        doc = "```"
    )]
    #[cfg_attr(
        not(all(
            feature = "std",
            feature = "tiny",
            not(any(
                feature = "tiny-static",
                feature = "compact",
                feature = "balanced",
                feature = "accurate"
            ))
        )),
        doc = "```ignore"
    )]
    /// # fn main() -> Result<(), utz::Error> {
    /// let finder = utz::Finder::new()?;
    /// let tz = finder.lookup(utz::Position { lon: -0.1278, lat: 51.5074 });
    /// assert_eq!(tz, Some("Europe/London"));
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    /// As [`Finder::from_slice()`] (or [`Finder::from_static()`] for
    /// `tiny-static`) on the baked preset asset.
    // `doc` is in the cfg so the method appears in rustdoc output, which
    // is built with several presets unified (workspace docs) or none
    // (docs.rs); in real builds it exists only with exactly one preset.
    #[cfg(any(
        doc,
        all(
            feature = "tiny",
            not(any(
                feature = "tiny-static",
                feature = "compact",
                feature = "balanced",
                feature = "accurate"
            ))
        ),
        all(
            feature = "tiny-static",
            not(any(
                feature = "tiny",
                feature = "compact",
                feature = "balanced",
                feature = "accurate"
            ))
        ),
        all(
            feature = "compact",
            not(any(
                feature = "tiny",
                feature = "tiny-static",
                feature = "balanced",
                feature = "accurate"
            ))
        ),
        all(
            feature = "balanced",
            not(any(
                feature = "tiny",
                feature = "tiny-static",
                feature = "compact",
                feature = "accurate"
            ))
        ),
        all(
            feature = "accurate",
            not(any(
                feature = "tiny",
                feature = "tiny-static",
                feature = "compact",
                feature = "balanced"
            ))
        )
    ))]
    // override the inferred banner: the OR-of-exclusions above renders as
    // an unreadable wall; "any preset" is the readable truth, and the
    // exactly-one rule is documented prose
    #[cfg_attr(
        docsrs,
        doc(cfg(any(
            feature = "tiny",
            feature = "tiny-static",
            feature = "compact",
            feature = "balanced",
            feature = "accurate"
        )))
    )]
    pub fn new() -> Result<Finder> {
        // Exactly one arm is compiled in real builds. Under `doc` several
        // presets may be enabled at once, so the arms exclude each other;
        // the last arm covers doc builds with no preset at all.
        #[cfg(feature = "tiny")]
        let finder = Finder::from_slice(crate::data::TINY);
        #[cfg(all(feature = "tiny-static", not(feature = "tiny")))]
        let finder = Finder::from_static(crate::data::TINY_STATIC);
        #[cfg(all(
            feature = "compact",
            not(any(feature = "tiny", feature = "tiny-static"))
        ))]
        let finder = Finder::from_slice(crate::data::COMPACT);
        #[cfg(all(
            feature = "balanced",
            not(any(feature = "tiny", feature = "tiny-static", feature = "compact"))
        ))]
        let finder = Finder::from_slice(crate::data::BALANCED);
        #[cfg(all(
            feature = "accurate",
            not(any(
                feature = "tiny",
                feature = "tiny-static",
                feature = "compact",
                feature = "balanced"
            ))
        ))]
        let finder = Finder::from_slice(crate::data::ACCURATE);
        #[cfg(not(any(
            feature = "tiny",
            feature = "tiny-static",
            feature = "compact",
            feature = "balanced",
            feature = "accurate"
        )))]
        let finder = Finder::from_static(&[]);
        finder
    }

    /// Borrow a container from `&'static` bytes (flash partition,
    /// `include_bytes!`, …). Zero-copy mode: no RAM payload. Only the
    /// `uncompressed` codec is accepted here.
    ///
    /// # Errors
    /// [`Error::StaticContainerCompressed`] if the container is compressed;
    /// the header-validation errors of [`format::outer()`]/[`format::parse()`]
    /// for an invalid container; [`Error::Misaligned`] for unaligned
    /// `FullRings` coords.
    pub fn from_static(bytes: &'static [u8]) -> Result<Finder> {
        let start = format::outer(bytes)?;
        let layout = format::parse(&bytes[start..])?;
        if layout.codec != Codec::Uncompressed {
            return Err(Error::StaticContainerCompressed);
        }
        let sections_start = start + format::PAYLOAD_HEADER_LEN;
        // trailing bytes (e.g. flash-partition padding) are fine; running
        // out before the declared blob end is not
        let payload = bytes
            .get(sections_start..sections_start + layout.sections_len)
            .ok_or(Error::Truncated)?;
        check_full_rings(payload, &layout)?;
        Ok(Finder {
            #[cfg(feature = "alloc")]
            payload: alloc::borrow::Cow::Borrowed(payload),
            #[cfg(not(feature = "alloc"))]
            payload,
            layout,
            #[cfg(feature = "alloc")]
            eager: None,
        })
    }

    /// The payload bytes, out of whichever representation this rung's
    /// [`Payload`] is.
    fn payload_bytes(&self) -> &[u8] {
        #[cfg(feature = "alloc")]
        {
            &self.payload
        }
        #[cfg(not(feature = "alloc"))]
        {
            self.payload
        }
    }

    /// Decode a borrowed container into an owned `Finder` (lazy mode),
    /// decompressing as needed. For compressed assets already in
    /// memory/flash (preset statics, OTA blobs): no copy of the compressed
    /// input is made. An uncompressed container is copied into owned RAM
    /// wholesale; if you own the buffer, [`Finder::from_vec()`] reuses its
    /// allocation instead, and for `&'static` data
    /// [`Finder::from_static()`] borrows it with no copy at all.
    ///
    /// # Errors
    /// The header-validation errors of [`format::outer()`]/[`format::parse()`]
    /// for an invalid container; [`Error::CodecNotCompiledIn`] /
    /// [`Error::DecoderFailed`] if the payload can't be decoded;
    /// [`Error::Misaligned`] for unaligned `FullRings` coords.
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
    pub fn from_slice(bytes: &[u8]) -> Result<Finder> {
        let start = format::outer(bytes)?;
        // the header is plaintext: validate it BEFORE any decompression
        let layout = format::parse(&bytes[start..])?;
        let sections = bytes
            .get(start + format::PAYLOAD_HEADER_LEN..)
            .ok_or(Error::Truncated)?;
        let payload = match layout.codec {
            Codec::Uncompressed => sections
                .get(..layout.sections_len)
                .ok_or(Error::Truncated)?
                .to_vec(),
            codec => decompress::decompress(codec, layout.sections_len, sections)?,
        };
        check_full_rings(&payload, &layout)?;
        Ok(Finder {
            payload: payload.into(),
            layout,
            eager: None,
        })
    }

    /// Take ownership of a container buffer: the entry point when the asset
    /// arrives at runtime (an OTA download, a network fetch) or when you
    /// bring your own decompression and hand over the result. A compressed
    /// container is decompressed if its codec is compiled in; an
    /// uncompressed one is adopted in place, reusing the allocation. Lazy
    /// mode either way: even an uncompressed owned buffer keeps the payload
    /// in RAM. Zero-copy needs [`from_static`](Finder::from_static).
    ///
    /// # Errors
    /// As [`Finder::from_slice()`].
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
    pub fn from_vec(bytes: Vec<u8>) -> Result<Finder> {
        let start = format::outer(&bytes)?;
        // the header is plaintext: validate it BEFORE any decompression
        let layout = format::parse(&bytes[start..])?;
        let sections_start = start + format::PAYLOAD_HEADER_LEN;
        if bytes.len() < sections_start + layout.sections_len && layout.codec == Codec::Uncompressed
        {
            return Err(Error::Truncated);
        }
        let payload = match layout.codec {
            Codec::Uncompressed => {
                let mut p = bytes;
                p.copy_within(sections_start..sections_start + layout.sections_len, 0);
                p.truncate(layout.sections_len); // reuse the allocation
                p
            }
            codec => {
                let sections = bytes.get(sections_start..).ok_or(Error::Truncated)?;
                decompress::decompress(codec, layout.sections_len, sections)?
            }
        };
        check_full_rings(&payload, &layout)?;
        Ok(Finder {
            payload: payload.into(),
            layout,
            eager: None,
        })
    }

    /// Decode straight to eager mode: all polygons decoded up front into
    /// a flat in-RAM cache so lookups never touch the encoded geometry,
    /// the fastest mode (what [`preload()`](Finder::preload) switches a
    /// `Finder` into). The geometry sections are then dropped:
    /// steady-state RAM is the eager cache plus only the
    /// header/tzid/grid tables, less than
    /// [`from_slice()`](Finder::from_slice) + `preload()` keeping the full
    /// decoded payload (−17% on the compact preset). Peak RAM during
    /// construction is unchanged (decoded payload and cache briefly
    /// coexist; the arc store must be resident to flatten rings). For
    /// uncompressed `&'static` assets,
    /// [`from_static()`](Finder::from_static) + `preload()` is better
    /// still: the payload stays in flash.
    ///
    /// # Errors
    /// As [`Finder::from_slice()`], which performs the load.
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
    pub fn eager_from_slice(bytes: &[u8]) -> Result<Finder> {
        let mut f = Finder::from_slice(bytes)?;
        if matches!(
            f.layout.geom,
            GeomEncoding::FullRings | GeomEncoding::Coarse
        ) {
            return Ok(f); // FullRings/coarse: nothing further to decode
        }
        f.preload();
        // keep [header + zone strings], [parent table] and [grid] —
        // everything lookups still read after preload; the arc store and
        // per-poly ring records between them are shadowed by the eager cache
        let (h, b) = (&f.layout, f.payload_bytes());
        let arcs_off = h.arc_offsets; // the arc block starts at its offsets table
        let parent_len = h.eager_polys as usize * 2;
        let grid_off = h.primary; // the grid starts at its primary cell table
        let mut p = Vec::with_capacity(arcs_off + parent_len + (b.len() - grid_off));
        p.extend_from_slice(&b[..arcs_off]);
        p.extend_from_slice(&b[h.parent..h.parent + parent_len]);
        p.extend_from_slice(&b[grid_off..]); // grid tables + release tail
        let parent = arcs_off;
        let shift = grid_off - (arcs_off + parent_len);
        f.layout.parent = parent;
        f.layout.primary -= shift;
        f.layout.list_offsets -= shift;
        f.layout.list_ids -= shift;
        f.layout.release_off -= shift;
        // poison the dropped sections' offsets: any residual use panics
        // out-of-bounds instead of reading grid bytes as geometry
        f.layout.arc_offsets = usize::MAX;
        f.layout.arc_data = usize::MAX;
        f.layout.poly_offsets = usize::MAX;
        f.layout.ring_data = usize::MAX;
        f.payload = p.into();
        Ok(f)
    }

    /// Read a container from any `Read` source into an owned buffer.
    ///
    /// # Errors
    /// [`Error::ReadFailed`] if reading fails; otherwise as
    /// [`Finder::from_vec()`].
    #[cfg(feature = "std")]
    pub fn from_reader(mut r: impl std::io::Read) -> Result<Finder> {
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes)
            .map_err(|source| Error::ReadFailed(source.to_string()))?;
        Finder::from_vec(bytes)
    }

    /// TZBB release recorded in the container header.
    #[must_use]
    pub fn tzbb_release(&self) -> &str {
        core::str::from_utf8(format::release(&self.layout, self.payload_bytes())).unwrap_or("")
    }

    /// Heap bytes [`preload`](Finder::preload) will reserve: the exact
    /// eager-cache size. The asset records how many coordinates, rings,
    /// and polygons its geometry decodes to, so this is pure arithmetic
    /// (no trial decode); a constrained caller can check fit before
    /// committing.
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
    #[must_use]
    pub fn preload_bytes(&self) -> usize {
        if matches!(
            self.layout.geom,
            GeomEncoding::FullRings | GeomEncoding::Coarse
        ) {
            return 0; // FullRings / coarse: nothing to decode
        }
        let h = &self.layout;
        // coords are cached at quant-nearest width
        let pair = if h.quant_bits == QuantBits::Bits16 {
            core::mem::size_of::<(i16, i16)>()
        } else {
            core::mem::size_of::<(i32, i32)>()
        };
        h.eager_coords as usize * pair
            + h.eager_rings as usize * core::mem::size_of::<u32>()
            + h.eager_polys as usize * core::mem::size_of::<([i32; 4], u32)>()
    }

    /// Decode all polygons into RAM once (eager mode): repeat lookups
    /// then skip the per-arc varint decode. Costs
    /// [`preload_bytes`](Finder::preload_bytes)
    /// (≈ uncompressed geometry at quant-nearest width: i16 pairs for
    /// i16-quant assets — half the cache — i32 otherwise) in heap. The
    /// whole cache is reserved exactly up front, so peak use equals the
    /// final size: no reallocation on the way. A no-op if already
    /// preloaded.
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
    pub fn preload(&mut self) {
        if self.eager.is_some()
            || matches!(
                self.layout.geom,
                GeomEncoding::FullRings | GeomEncoding::Coarse
            )
        {
            // geom=2 (FullRings): the payload already IS the cache;
            // geom=3 (coarse): nothing to decode
            return;
        }
        self.eager = Some(if self.layout.quant_bits == QuantBits::Bits16 {
            let (coords, ring_ends, polys) = self.decode_rings::<(i16, i16)>();
            Eager {
                coords: EagerCoords::Narrow(coords),
                ring_ends,
                polys,
            }
        } else {
            let (coords, ring_ends, polys) = self.decode_rings::<(i32, i32)>();
            Eager {
                coords: EagerCoords::Wide(coords),
                ring_ends,
                polys,
            }
        });
    }

    /// [`preload`](Finder::preload)'s decode pass, generic over the cache's
    /// coordinate width.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "counts bounded by the parse-validated u32 header reservations"
    )]
    fn decode_rings<C: EagerCoord>(&self) -> (Vec<C>, Vec<u32>, Polys) {
        let (h, b) = (&self.layout, self.payload_bytes());
        let mut coords = Vec::with_capacity(h.eager_coords as usize);
        let mut ring_ends = Vec::with_capacity(h.eager_rings as usize);
        let mut polys = Vec::with_capacity(h.eager_polys as usize);
        let fb = h.quant_bits.bytes();
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
    /// (zero alloc); eager ones (after `preload`) scan
    /// pre-decoded rings.
    #[must_use]
    pub fn lookup(&self, pos: Position) -> Option<&str> {
        let (px, py) = self.quantize(pos);
        match self.cell_value(px, py) {
            v if v == NO_ZONE => None,
            v if v & 0x8000 == 0 => self.tzid(v),
            v => {
                // border cell: candidates are the POLYS whose rings touch it
                // — resolve the winner's feature via the parent table
                let (s, e) = self.list_bounds(v & 0x7FFF);
                let b = self.payload_bytes();
                // coarse assets carry no geometry: cell precision IS the
                // asset's precision — the dominant-first head is the answer
                if cfg!(feature = "geom-coarse") && self.layout.geom == GeomEncoding::Coarse {
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
                // dominant-first head (a poly id)
                self.tzid(self.parent_of(read_u16(self.payload_bytes(), s)))
            }
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "qmax = 2^(quant_bits-1)-1 ≤ 2^31-1, exact in f64"
    )]
    fn qmax(&self) -> f64 {
        ((1u64 << (self.layout.quant_bits.bits() - 1)) - 1) as f64
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "|v*qmax| < i32::MAX for in-range lon/lat; float as saturates, wild input degrades to a miss"
    )]
    fn quantize(&self, pos: Position) -> (i32, i32) {
        // round-half-away like the encoder (f64::round is std-only)
        let r = |v: f64| (v + if v >= 0.0 { 0.5 } else { -0.5 }) as i32;
        let q = self.qmax();
        (r(pos.lon / 180.0 * q), r(pos.lat / 90.0 * q))
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "cast saturates then clamped to grid range"
    )]
    fn cell_value(&self, px: i32, py: i32) -> u16 {
        let (header, qmax) = (&self.layout, self.qmax());
        let cell_deg = f64::from(header.grid_deg);
        let lon = f64::from(px) / qmax * 180.0;
        let lat = f64::from(py) / qmax * 90.0;
        let col =
            (((lon + 180.0) / cell_deg) as i64).clamp(0, i64::from(header.ncols) - 1) as usize;
        let row = (((lat + 90.0) / cell_deg) as i64).clamp(0, i64::from(header.nrows) - 1) as usize;
        read_u16(
            self.payload_bytes(),
            header.primary + (row * header.ncols as usize + col) * 2,
        )
    }

    fn list_bounds(&self, li: u16) -> (usize, usize) {
        let (h, b) = (&self.layout, self.payload_bytes());
        let s = read_u16(b, h.list_offsets + li as usize * 2) as usize;
        let e = read_u16(b, h.list_offsets + li as usize * 2 + 2) as usize;
        (h.list_ids + s * 2, h.list_ids + e * 2)
    }

    fn tzid(&self, fid: u16) -> Option<&str> {
        let (h, b) = (&self.layout, self.payload_bytes());
        let s = read_u16(b, fid as usize * 2) as usize;
        let e = read_u16(b, fid as usize * 2 + 2) as usize;
        core::str::from_utf8(&b[h.pool + s..h.pool + e])
            .ok()
            .filter(|t| !t.is_empty())
    }

    /// poly id → feature id (parent table).
    fn parent_of(&self, pid: u16) -> u16 {
        read_u16(self.payload_bytes(), self.layout.parent + pid as usize * 2)
    }

    /// Per-polygon test: bbox gate, then even-odd PIP at the width the
    /// header demands. Grid candidates are polys localized to the
    /// CELL; the record's bbox is the point-granular refinement: a
    /// miss returns before touching any arc. Lazy path streams the arcs
    /// straight off the container bytes through the per-edge kernel:
    /// junction vertices are shared by consecutive arcs and the ring closure
    /// is a shared junction too, so the ring's segment set is exactly the
    /// union of each arc's internal segments. Every arc is walked FORWARD
    /// (orientation bit ignored) with O(1) state, and parity XORs across
    /// arcs order-independently.
    fn poly_contains(&self, pid: u16, px: i32, py: i32) -> bool {
        #[cfg(feature = "geom-full-rings")]
        if self.layout.geom == GeomEncoding::FullRings {
            return self.full_rings_poly_contains(pid, px, py);
        }
        #[cfg(feature = "alloc")]
        if let Some(e) = &self.eager {
            return self.eager_poly_contains(e, pid, px, py);
        }
        let (h, b) = (&self.layout, self.payload_bytes());
        let fb = h.quant_bits.bytes();
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
    #[expect(
        clippy::cast_possible_truncation,
        reason = "coords accumulate i16/i24/i32-width deltas; sums fit i32 by format"
    )]
    fn scan_arc(&self, id: usize, px: i32, py: i32) -> pip::RingHit {
        let (h, b) = (&self.layout, self.payload_bytes());
        let wide = h.quant_bits == QuantBits::Bits32;
        let fixed =
            cfg!(feature = "geom-fixed-width-arcs") && h.geom == GeomEncoding::FixedWidthArcs;
        let mut pos = h.arc_data + read_u32(b, h.arc_offsets + id * 4) as usize;
        let (vcount, p2) = read_varint(b, pos);
        pos = p2;
        let fb = h.quant_bits.bytes();
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
                edge_wide((x0, y0), (x1, y1), px, py)
            } else {
                edge_narrow((x0, y0), (x1, y1), px, py)
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

    /// `FullRings` path (geom=2): the payload geometry IS the eager cache.
    /// One generic slice kernel folds straight off the payload bytes (flash
    /// in zero-copy mode). Coord width follows the quant width: i16 /
    /// i32 as typed pairs, i24 as [`pip::Pack24`] (align 1, no alignment
    /// requirement). Works on the bare `core` rung.
    #[cfg(feature = "geom-full-rings")]
    fn full_rings_poly_contains(&self, pid: u16, px: i32, py: i32) -> bool {
        let (h, b) = (&self.layout, self.payload_bytes());
        let pe = h.full_polys + pid as usize * 20;
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
        let rstart = if pid == 0 {
            0
        } else {
            read_u32(b, pe - 4) as usize
        };
        match h.quant_bits {
            QuantBits::Bits16 => {
                // an in-bbox point of a valid i16-quant asset always fits
                // i16; the fallthrough covers adversarial bboxes
                let (Ok(px), Ok(py)) = (i16::try_from(px), i16::try_from(py)) else {
                    return false;
                };
                self.full_rings_fold(rstart, rend, px, py, ring_hit_narrow::<(i16, i16)>)
            }
            QuantBits::Bits24 => {
                self.full_rings_fold(rstart, rend, px, py, ring_hit_narrow::<pip::Pack24>)
            }
            QuantBits::Bits32 => self.full_rings_fold(rstart, rend, px, py, ring_hit_wide),
        }
    }

    /// Even-odd fold over one poly's rings `[rstart, rend)` at pair
    /// type `P`: `size_of::<P>()` IS the stored coordinate stride; `scan`
    /// is the width-matched per-target ring kernel ([`ring_hit_narrow`] /
    /// [`ring_hit_wide`]).
    /// (No `cast_ptr_alignment` expect needed anymore: the cast target is
    /// the opaque `P`, so the lint can't see a concrete alignment; the
    /// invariant itself is stated in the SAFETY comment below.)
    #[cfg(feature = "geom-full-rings")]
    fn full_rings_fold<P: pip::CoordPair>(
        &self,
        rstart: usize,
        rend: usize,
        px: P::Narrow,
        py: P::Narrow,
        scan: impl Fn(&[P], P::Narrow, P::Narrow) -> pip::RingHit,
    ) -> bool {
        let (h, b) = (&self.layout, self.payload_bytes());
        let mut inside = false;
        let mut cstart = if rstart == 0 {
            0
        } else {
            read_u32(b, h.full_ring_ends + (rstart - 1) * 4) as usize
        };
        for ri in rstart..rend {
            let cend = read_u32(b, h.full_ring_ends + ri * 4) as usize;
            let n = cend - cstart;
            // SAFETY (slice cast): pair layouts are asserted at the top of
            // this file (Pack24 is align 1; i16/i32 pairs land aligned
            // because full_coords is 4-aligned — checked at load — and their
            // strides are multiples of the element alignment); parse bounds
            // the full-rings sections against the header counts.
            let ring = unsafe {
                core::slice::from_raw_parts(
                    b[h.full_coords + cstart * core::mem::size_of::<P>()..]
                        .as_ptr()
                        .cast::<P>(),
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
        let rstart = if pi == 0 {
            0
        } else {
            e.polys[pi - 1].1 as usize
        };
        match &e.coords {
            EagerCoords::Narrow(coords) => {
                // an in-bbox point of a valid i16-quant asset always fits
                // i16; the fallthrough covers adversarial bboxes
                let (Ok(px), Ok(py)) = (i16::try_from(px), i16::try_from(py)) else {
                    return false;
                };
                rings_hit(
                    coords,
                    &e.ring_ends,
                    rstart,
                    rend as usize,
                    px,
                    py,
                    ring_hit_narrow,
                )
            }
            EagerCoords::Wide(coords) if self.layout.quant_bits == QuantBits::Bits32 => rings_hit(
                coords,
                &e.ring_ends,
                rstart,
                rend as usize,
                px,
                py,
                ring_hit_wide,
            ),
            EagerCoords::Wide(coords) => rings_hit(
                coords,
                &e.ring_ends,
                rstart,
                rend as usize,
                px,
                py,
                ring_hit_narrow::<(i32, i32)>,
            ),
        }
    }

    /// Decode one signed arc ref onto the end of `coords` (join-deduplicated).
    /// Eager-mode decode only; the lazy path streams via `scan_arc` instead.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "coords accumulate i16/i24/i32-width deltas; sums fit i32 by format"
    )]
    fn append_arc<C: EagerCoord>(&self, arc_ref: u32, coords: &mut Vec<C>) {
        let (header, payload) = (&self.layout, self.payload_bytes());
        let (id, rev) = ((arc_ref >> 1) as usize, (arc_ref & 1) == 1);
        let mut pos = header.arc_data + read_u32(payload, header.arc_offsets + id * 4) as usize;
        let (vcount, after_vcount) = read_varint(payload, pos);
        pos = after_vcount;
        let coord_bytes = header.quant_bits.bytes();
        let mut qlon = i64::from(read_fixed(payload, pos, header.quant_bits));
        let mut qlat = i64::from(read_fixed(payload, pos + coord_bytes, header.quant_bits));
        pos += 2 * coord_bytes;
        let start = coords.len();
        coords.push(C::from_q(qlon as i32, qlat as i32));
        for _ in 1..vcount {
            if cfg!(feature = "geom-fixed-width-arcs")
                && header.geom == GeomEncoding::FixedWidthArcs
            {
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

/// The ring-scan kernel for i16/i24-quant geometry: the
/// sign-split kernel on 32-bit targets (0.61–0.72× the i64 kernel there:
/// its magnitudes take single widening multiplies where the W kernels'
/// (b+1)-bit differences force full wide ones), the generic i64 kernel on
/// 64-bit targets (one-instruction wide multiplies; sign-split measured
/// 2.3× SLOWER on a 64-bit host). Ring verdicts are identical either way,
/// so answers stay platform-independent.
#[cfg(any(feature = "alloc", feature = "geom-full-rings"))]
fn ring_hit_narrow<P>(ring: &[P], px: P::Narrow, py: P::Narrow) -> pip::RingHit
where
    P: pip::CoordPair,
    P::Narrow: pip::Narrow,
    i64: pip::Wide<P::Narrow>,
{
    #[cfg(target_pointer_width = "32")]
    return pip::ring_hit_split(ring, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::ring_hit::<i64, _>(ring, px, py)
}

/// [`ring_hit_narrow`]'s i32-quant sibling: sign-split on 32-bit targets
/// (0.24× the i128 kernel there), i128 on 64-bit ones (where i128
/// measured 0.75× of even the i64 kernel).
#[cfg(any(feature = "alloc", feature = "geom-full-rings"))]
fn ring_hit_wide(ring: &[(i32, i32)], px: i32, py: i32) -> pip::RingHit {
    #[cfg(target_pointer_width = "32")]
    return pip::ring_hit_split(ring, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::ring_hit::<i128, _>(ring, px, py)
}

/// Streaming per-edge kernels (`scan_arc`), same per-target policy as the
/// ring dispatchers above.
fn edge_narrow(a: (i32, i32), b: (i32, i32), px: i32, py: i32) -> pip::EdgeHit {
    #[cfg(target_pointer_width = "32")]
    return pip::edge_split(a, b, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::edge::<i64, _>(a, b, px, py)
}

/// [`edge_narrow`]'s i32-quant sibling.
fn edge_wide(a: (i32, i32), b: (i32, i32), px: i32, py: i32) -> pip::EdgeHit {
    #[cfg(target_pointer_width = "32")]
    return pip::edge_split(a, b, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::edge::<i128, _>(a, b, px, py)
}

/// Even-odd fold over consecutive rings `[rstart, rend)` of a flat eager
/// cache, shared by both cache widths ([`EagerCoords`]); `scan` is the
/// width-matched per-target ring kernel ([`ring_hit_narrow`] /
/// [`ring_hit_wide`]).
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
    let mut cstart = if rstart == 0 {
        0
    } else {
        ring_ends[rstart - 1] as usize
    };
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
