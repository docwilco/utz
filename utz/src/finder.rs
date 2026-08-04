//! `Finder` answers lookups with a grid prefilter followed by
//! per-polygon integer PIP.
//!
//! There are three memory modes, selected automatically by how the asset
//! is loaded (only eager is an explicit request); availability falls out
//! of the environment rung:
//! - In **zero-copy** mode (`from_static()`, uncompressed) the payload is
//!   borrowed from static storage (flash partition, `include_bytes!`, …).
//!   There is no RAM payload at all; flash pays the uncompressed size.
//!   This is the `core` rung.
//! - In **lazy** mode (`from_slice()`/`from_vec()`/`from_reader()`) the
//!   payload lives in owned RAM, typically because the asset is
//!   compressed and flash can't fit it uncompressed. There is no
//!   decoded-geometry cache: RAM holds the decompressed payload, nothing
//!   more.
//! - In **eager** mode ([`Finder::preload()`], `alloc`) all rings are
//!   additionally decoded into RAM once; lookups then scan decoded
//!   slices. It uses the most RAM and gives the fastest repeat lookups.
//!
//! Zero-copy and lazy share the identical lookup mechanism: candidates
//! are PIP-tested by walking their arcs directly off the payload bytes
//! through the per-edge kernel, with O(1) state and no allocation. They
//! differ only in where the payload resides (borrowed static vs owned
//! RAM), that is, in the `Cow` variant of [`Payload`]. Interior cells
//! touch zero geometry in every mode, and [`Finder::lookup_coarse()`]
//! never touches geometry at all.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use crate::decompress;
use crate::format::{self, read_fixed, read_u16, read_u32, read_varint, unzigzag, PayloadLayout};
use crate::{caps, pip, Codec, Error, Result};
use utz_common::{GeomEncoding, QuantBits, NO_ZONE};

/// A geographic position in degrees, **order-neutral by design**: you
/// construct it with named fields, so there is no argument order to get
/// wrong, only values. `Position { lat: 51.5, lon: -0.13 }` and
/// `Position { lon: -0.13, lat: 51.5 }` are the same position.
///
/// There is deliberately no positional constructor and no
/// `From<(f64, f64)>`: either would reintroduce the lon/lat-swap footgun
/// this type exists to kill.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Position {
    /// The longitude in degrees, −180..=180 (x).
    pub lon: f64,
    /// The latitude in degrees, −90..=90 (y).
    pub lat: f64,
}

impl Position {
    /// Reports whether both coordinates are in range (lon −180..=180,
    /// lat −90..=90); NaN reports `false`. This is what
    /// [`Finder::lookup()`] checks before answering.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        (-180.0..=180.0).contains(&self.lon) && (-90.0..=90.0).contains(&self.lat)
    }
}

/// The asset's payload section. `Cow::Borrowed` is zero-copy mode and
/// `Cow::Owned` is lazy/eager. `Cow` itself lives in `alloc`; on the
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

/// Performs the `FullRings` load-time check: the coords are read via
/// typed slice casts, so the payload must land them 4-aligned (static
/// assets use [`crate::include_bytes_aligned!`]`(4, ..)`). Endianness is
/// a compile-time refusal (see the `geom-full-rings` `compile_error` in
/// lib.rs).
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

/// Eager-mode storage, which holds every ring decoded into flat arrays.
/// Ranges are stored as exclusive ends; a range's start is the previous
/// entry's end (global across features, so there is no per-item start
/// field).
#[cfg(feature = "alloc")]
struct Eager {
    coords: EagerCoords,
    /// The exclusive end into `coords` for each ring.
    ring_ends: Vec<u32>,
    /// One entry per polygon (indexed by poly id): the bbox (read from
    /// the poly record) plus the exclusive end into `ring_ends`. The bbox
    /// skips whole-ring folds for candidates that touch the cell but not
    /// the point.
    polys: Vec<([i32; 4], u32)>,
}

/// The per-polygon eager records, each a bbox plus an exclusive
/// `ring_ends` end (see [`Eager::polys`]).
#[cfg(feature = "alloc")]
type Polys = Vec<([i32; 4], u32)>;

/// The eager cache's coordinate store, kept at quant-nearest width:
/// i16-quant assets keep i16 pairs (half the cache RAM), and PIP widens
/// per edge inside the kernel (still to i64: crosses of i16 coords reach
/// 2^33; see the pip module docs).
#[cfg(feature = "alloc")]
enum EagerCoords {
    Narrow(Vec<(i16, i16)>),
    Wide(Vec<(i32, i32)>),
}

/// Constructs an eager-cache element by narrowing a decoded
/// (i32-accumulated) quant coordinate to the storage width. `PartialEq`
/// powers the arc-join and ring-closure vertex dedup during decode.
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

/// A loaded timezone index. Build it once and query it many times.
///
/// Availability follows the environment ladder; each row adds to the
/// ones above it:
///
/// | feature | adds mode    | API |
/// |---------|--------------|-----|
/// | `core`  | zero-copy    | [`from_static()`](Finder::from_static), [`lookup()`](Finder::lookup), [`lookup_coarse()`](Finder::lookup_coarse) (and their `_unchecked` variants) |
/// | `alloc` | lazy + eager | [`from_slice()`](Finder::from_slice), [`from_vec()`](Finder::from_vec), [`eager_from_slice()`](Finder::eager_from_slice), [`preload()`](Finder::preload), [`preload_bytes()`](Finder::preload_bytes) |
/// | `std`   |              | [`from_reader()`](Finder::from_reader) |
pub struct Finder {
    payload: Payload,
    layout: PayloadLayout,
    /// The eager-mode geometry, populated by `preload()`.
    #[cfg(feature = "alloc")]
    eager: Option<Eager>,
}

impl Finder {
    /// Loads the preset selected by the (single) enabled preset feature.
    /// It is documented here always, but cfg'd out of real builds when
    /// zero or several presets are in the tree; there, load explicitly
    /// with `from_slice()`/`from_static()` on the statics in
    /// [`crate::data`] instead. `tiny-static` is the zero-copy one
    /// (`from_static()`, bare `core`); the rest are compressed and load
    /// lazy (`from_slice()`).
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
    /// let tz = finder.lookup(utz::Position { lon: -0.1278, lat: 51.5074 })?;
    /// assert_eq!(tz, Some("Europe/London"));
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    /// The errors are as for [`Finder::from_slice()`] (or
    /// [`Finder::from_static()`] for `tiny-static`) on the baked preset
    /// asset.
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

    /// Borrows an asset from `&'static` bytes (flash partition,
    /// `include_bytes!`, …). This is zero-copy mode: there is no RAM
    /// payload. Only the `uncompressed` codec is accepted here.
    ///
    /// # Errors
    /// Returns [`Error::StaticAssetCompressed`] if the asset is
    /// compressed; [`Error::BadMagic`], [`Error::UnsupportedVersion`], or
    /// the other header-validation errors if the bytes are not a readable
    /// μTZ asset; and [`Error::Misaligned`] for unaligned `FullRings`
    /// coords.
    pub fn from_static(bytes: &'static [u8]) -> Result<Finder> {
        let start = format::outer(bytes)?;
        let layout = format::parse(&bytes[start..])?;
        if layout.codec != Codec::Uncompressed {
            return Err(Error::StaticAssetCompressed);
        }
        let sections_start = start + format::PAYLOAD_HEADER_LEN;
        // trailing bytes (e.g. flash-partition padding) are fine; running
        // out before the declared blob end is not
        let payload = bytes
            .get(sections_start..sections_start + layout.sections_len)
            .ok_or(Error::Truncated)?;
        check_full_rings(payload, &layout)?;
        format::check_tables(payload, &layout)?;
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

    /// Returns the payload bytes out of whichever representation this
    /// rung's [`Payload`] is.
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

    /// Decodes a borrowed asset into an owned `Finder` (lazy mode),
    /// decompressing as needed. For compressed assets already in
    /// memory/flash (preset statics, OTA blobs), no copy of the compressed
    /// input is made. An uncompressed asset is copied into owned RAM
    /// wholesale; if you own the buffer, [`Finder::from_vec()`] reuses its
    /// allocation instead, and for `&'static` data
    /// [`Finder::from_static()`] borrows it with no copy at all.
    ///
    /// # Errors
    /// Returns [`Error::BadMagic`], [`Error::UnsupportedVersion`], or the
    /// other header-validation errors if the bytes are not a readable μTZ
    /// asset; [`Error::CodecNotCompiledIn`] or [`Error::DecoderFailed`]
    /// if the payload can't be decoded; and [`Error::Misaligned`] for
    /// unaligned `FullRings` coords.
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
        format::check_tables(&payload, &layout)?;
        Ok(Finder {
            payload: payload.into(),
            layout,
            eager: None,
        })
    }

    /// Takes ownership of an asset buffer. This is the entry point when
    /// the asset arrives at runtime (an OTA download, a network fetch) or
    /// when you bring your own decompression and hand over the result. A
    /// compressed asset is decompressed if its codec is compiled in; an
    /// uncompressed one is adopted in place, reusing the allocation. The
    /// result is lazy mode either way: even an uncompressed owned buffer
    /// keeps the payload in RAM. Zero-copy needs
    /// [`from_static()`](Finder::from_static).
    ///
    /// # Errors
    /// The errors are as for [`Finder::from_slice()`].
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
                let mut buffer = bytes;
                buffer.copy_within(sections_start..sections_start + layout.sections_len, 0);
                buffer.truncate(layout.sections_len); // reuse the allocation
                buffer
            }
            codec => {
                let sections = bytes.get(sections_start..).ok_or(Error::Truncated)?;
                decompress::decompress(codec, layout.sections_len, sections)?
            }
        };
        check_full_rings(&payload, &layout)?;
        format::check_tables(&payload, &layout)?;
        Ok(Finder {
            payload: payload.into(),
            layout,
            eager: None,
        })
    }

    /// Decodes straight to eager mode: all polygons are decoded up front
    /// into a flat in-RAM cache so lookups never touch the encoded
    /// geometry, which is the fastest mode (the mode
    /// [`preload()`](Finder::preload) switches a `Finder` into). The
    /// encoded geometry is then dropped: steady-state
    /// RAM is the eager cache plus the small lookup tables, less than
    /// [`from_slice()`](Finder::from_slice) + `preload()` keeping the full
    /// decoded payload (−17% on the compact preset). Peak RAM during
    /// construction is unchanged: the decoded payload and the cache
    /// briefly coexist while the cache is built. For
    /// uncompressed `&'static` assets,
    /// [`from_static()`](Finder::from_static) + `preload()` is better
    /// still: the payload stays in flash.
    ///
    /// # Errors
    /// The errors are as for [`Finder::from_slice()`], which performs the
    /// load.
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
    pub fn eager_from_slice(bytes: &[u8]) -> Result<Finder> {
        let mut finder = Finder::from_slice(bytes)?;
        if matches!(
            finder.layout.geom,
            GeomEncoding::FullRings | GeomEncoding::Coarse
        ) {
            return Ok(finder); // FullRings/coarse: nothing further to decode
        }
        finder.preload();
        // keep [header + zone strings], [parent table] and [grid] —
        // everything lookups still read after preload; the arc store and
        // per-poly ring records between them are shadowed by the eager cache
        let (layout, payload) = (&finder.layout, finder.payload_bytes());
        let arc_block_start = layout.arc_offsets; // the arc block starts at its offsets table
        let parent_len = layout.eager_polys as usize * 2;
        let grid_start = layout.primary; // the grid starts at its primary cell table
        let mut kept_payload =
            Vec::with_capacity(arc_block_start + parent_len + (payload.len() - grid_start));
        kept_payload.extend_from_slice(&payload[..arc_block_start]);
        kept_payload.extend_from_slice(&payload[layout.parent..layout.parent + parent_len]);
        kept_payload.extend_from_slice(&payload[grid_start..]); // grid tables + release tail
        let parent = arc_block_start;
        let shift = grid_start - (arc_block_start + parent_len);
        finder.layout.parent = parent;
        finder.layout.primary -= shift;
        finder.layout.list_offsets -= shift;
        finder.layout.list_ids -= shift;
        finder.layout.release_off -= shift;
        // poison the dropped sections' offsets: any residual use panics
        // out-of-bounds instead of reading grid bytes as geometry
        finder.layout.arc_offsets = usize::MAX;
        finder.layout.arc_data = usize::MAX;
        finder.layout.poly_offsets = usize::MAX;
        finder.layout.ring_data = usize::MAX;
        finder.payload = kept_payload.into();
        Ok(finder)
    }

    /// Reads an asset from any `Read` source into an owned buffer.
    ///
    /// # Errors
    /// Returns [`Error::ReadFailed`] if reading fails; otherwise the
    /// errors are as for [`Finder::from_vec()`].
    #[cfg(feature = "std")]
    pub fn from_reader(mut reader: impl std::io::Read) -> Result<Finder> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|source| Error::ReadFailed(source.to_string()))?;
        Finder::from_vec(bytes)
    }

    /// Returns the population-density weight floor the asset was
    /// simplified with (the `Config::density_weight_floor()` knob), as
    /// recorded in its header; `None` means uniform, unweighted
    /// simplification. It is provenance, like
    /// [`tzbb_release()`](Finder::tzbb_release), and does not affect
    /// lookups.
    #[must_use]
    pub fn density_weight_floor(&self) -> Option<f64> {
        match self.layout.density_weight_floor_e4 {
            0 => None,
            floor_e4 => Some(f64::from(floor_e4) / 1e4),
        }
    }

    /// Returns the [timezone-boundary-builder] (TZBB) release the asset
    /// was generated from, as recorded in its header; the result is an
    /// empty string if the recorded bytes are not valid UTF-8.
    ///
    /// [timezone-boundary-builder]: https://github.com/evansiroky/timezone-boundary-builder
    #[must_use]
    pub fn tzbb_release(&self) -> &str {
        core::str::from_utf8(format::release(&self.layout, self.payload_bytes())).unwrap_or("")
    }

    /// Returns the heap bytes [`preload()`](Finder::preload) will
    /// reserve, which is the exact eager-cache size. The asset records
    /// how many coordinates, rings, and polygons its geometry decodes to,
    /// so this is pure arithmetic (no trial decode); a constrained caller
    /// can check fit before committing.
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
        let layout = &self.layout;
        // coords are cached at quant-nearest width
        let pair_size = if layout.quant_bits == QuantBits::Bits16 {
            core::mem::size_of::<(i16, i16)>()
        } else {
            core::mem::size_of::<(i32, i32)>()
        };
        layout.eager_coords as usize * pair_size
            + layout.eager_rings as usize * core::mem::size_of::<u32>()
            + layout.eager_polys as usize * core::mem::size_of::<([i32; 4], u32)>()
    }

    /// Decodes all polygons into RAM once (eager mode): repeat lookups
    /// then skip decoding entirely, making this the fastest mode. It
    /// costs [`preload_bytes()`](Finder::preload_bytes)
    /// (≈ uncompressed geometry at quant-nearest width: i16 pairs for
    /// i16-quant assets, half the cache, and i32 otherwise) in heap. The
    /// whole cache is reserved exactly up front, so peak use equals the
    /// final size with no reallocation on the way. It is a no-op if
    /// already preloaded.
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

    /// Runs [`preload()`](Finder::preload)'s decode pass, generic over
    /// the cache's coordinate width.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "counts bounded by the parse-validated u32 header reservations"
    )]
    fn decode_rings<C: EagerCoord>(&self) -> (Vec<C>, Vec<u32>, Polys) {
        let (layout, payload) = (&self.layout, self.payload_bytes());
        let mut coords = Vec::with_capacity(layout.eager_coords as usize);
        let mut ring_ends = Vec::with_capacity(layout.eager_rings as usize);
        let mut polys = Vec::with_capacity(layout.eager_polys as usize);
        let coord_bytes = layout.quant_bits.bytes();
        for poly_id in 0..layout.eager_polys {
            let mut position = layout.ring_data
                + read_u32(payload, layout.poly_offsets + poly_id as usize * 4) as usize;
            let bbox = [
                read_fixed(payload, position, layout.quant_bits),
                read_fixed(payload, position + coord_bytes, layout.quant_bits),
                read_fixed(payload, position + 2 * coord_bytes, layout.quant_bits),
                read_fixed(payload, position + 3 * coord_bytes, layout.quant_bits),
            ];
            position += 4 * coord_bytes;
            let ring_count = read_u16(payload, position);
            position += 2;
            for _ in 0..ring_count {
                let (arc_ref_count, mut ref_position) = read_varint(payload, position);
                let start = coords.len();
                for _ in 0..arc_ref_count {
                    let (arc_ref, after_ref) = read_varint(payload, ref_position);
                    ref_position = after_ref;
                    self.append_arc(arc_ref as u32, &mut coords);
                }
                position = ref_position;
                // drop the duplicated ring-closure vertex (ring_hit wraps)
                if coords.len() > start + 1 && coords.last() == coords.get(start) {
                    coords.pop();
                }
                ring_ends.push(coords.len() as u32);
            }
            polys.push((bbox, ring_ends.len() as u32));
        }
        (coords, ring_ends, polys)
    }

    /// Performs the accurate lookup: the grid cell yields either an
    /// interior zone (O(1)) or candidates that go through PIP.
    ///
    /// `Ok(None)` means no zone claims the point (at sea on a `land-`
    /// dataset). With oceans covered (the default datasets) every valid
    /// position resolves to some zone. On a `Coarse` (grid-only) asset
    /// the answer is at cell precision, identical to
    /// [`lookup_coarse()`](Finder::lookup_coarse): cell precision is that
    /// asset's precision.
    ///
    /// Zero-copy/lazy Finders test candidates directly off the payload
    /// bytes (zero alloc); eager ones (after `preload()`) scan
    /// pre-decoded rings.
    ///
    /// # Errors
    /// Returns [`Error::InvalidPosition`] if `position` fails
    /// [`Position::is_valid()`]: lon beyond ±180, lat beyond ±90, or NaN.
    /// Pre-validated hot loops can skip the check with
    /// [`lookup_unchecked()`](Finder::lookup_unchecked).
    pub fn lookup(&self, position: Position) -> Result<Option<&str>> {
        if position.is_valid() {
            Ok(self.lookup_unchecked(position))
        } else {
            Err(Error::InvalidPosition)
        }
    }

    /// Runs [`lookup()`](Finder::lookup) without the position check, for
    /// hot loops whose inputs are valid by construction. It is
    /// memory-safe for any input, but an invalid position returns an
    /// arbitrary nearby answer instead of an error: out-of-range
    /// coordinates clamp to the nearest edge of the world, and NaN
    /// behaves as 0.
    #[must_use]
    pub fn lookup_unchecked(&self, position: Position) -> Option<&str> {
        let (px, py) = self.quantize(position);
        match self.cell_value(px, py) {
            cell if cell == NO_ZONE => None,
            cell if cell & 0x8000 == 0 => self.tzid(cell),
            cell => {
                // border cell: candidates are the POLYS whose rings touch it
                // — resolve the winner's feature via the parent table
                let (start, end) = self.list_bounds(cell & 0x7FFF);
                let payload = self.payload_bytes();
                // coarse assets carry no geometry: cell precision IS the
                // asset's precision — the dominant-first head is the answer
                if caps::GEOM_COARSE && self.layout.geom == GeomEncoding::Coarse {
                    return self.tzid(self.parent_of(read_u16(payload, start)));
                }
                let mut first = None;
                for offset in (start..end).step_by(2) {
                    let poly_id = read_u16(payload, offset);
                    first.get_or_insert(poly_id);
                    if self.poly_contains(poly_id, px, py) {
                        return self.tzid(self.parent_of(poly_id));
                    }
                }
                // quantization edge: no candidate claims the point — the
                // dominant-first head is the best answer (measured ~0/100k)
                first.and_then(|poly_id| self.tzid(self.parent_of(poly_id)))
            }
        }
    }

    /// Performs a grid-only approximate lookup on any asset: no geometry
    /// is decoded, and the border error is ~cell-size. Border cells
    /// answer with the cell's dominant zone; `Ok(None)` means no zone
    /// touches the cell (at sea on a `land-` dataset).
    ///
    /// # Errors
    /// Returns [`Error::InvalidPosition`], exactly as
    /// [`lookup()`](Finder::lookup) does.
    pub fn lookup_coarse(&self, position: Position) -> Result<Option<&str>> {
        if position.is_valid() {
            Ok(self.lookup_coarse_unchecked(position))
        } else {
            Err(Error::InvalidPosition)
        }
    }

    /// Runs [`lookup_coarse()`](Finder::lookup_coarse) without the
    /// position check, under the same contract as
    /// [`lookup_unchecked()`](Finder::lookup_unchecked).
    #[must_use]
    pub fn lookup_coarse_unchecked(&self, position: Position) -> Option<&str> {
        let (px, py) = self.quantize(position);
        match self.cell_value(px, py) {
            cell if cell == NO_ZONE => None,
            cell if cell & 0x8000 == 0 => self.tzid(cell),
            cell => {
                let (start, _) = self.list_bounds(cell & 0x7FFF);
                // dominant-first head (a poly id)
                self.tzid(self.parent_of(read_u16(self.payload_bytes(), start)))
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
        reason = "|v*qmax| < i32::MAX for in-range lon/lat; float as saturates, and the unchecked lookups document that wild input clamps"
    )]
    fn quantize(&self, position: Position) -> (i32, i32) {
        // round-half-away like the encoder (f64::round is std-only)
        let round = |value: f64| (value + if value >= 0.0 { 0.5 } else { -0.5 }) as i32;
        let qmax = self.qmax();
        (
            round(position.lon / 180.0 * qmax),
            round(position.lat / 90.0 * qmax),
        )
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

    fn list_bounds(&self, list_index: u16) -> (usize, usize) {
        let (layout, payload) = (&self.layout, self.payload_bytes());
        let start = read_u16(payload, layout.list_offsets + list_index as usize * 2) as usize;
        let end = read_u16(payload, layout.list_offsets + list_index as usize * 2 + 2) as usize;
        (layout.list_ids + start * 2, layout.list_ids + end * 2)
    }

    fn tzid(&self, feature_id: u16) -> Option<&str> {
        let (layout, payload) = (&self.layout, self.payload_bytes());
        let start = read_u16(payload, feature_id as usize * 2) as usize;
        let end = read_u16(payload, feature_id as usize * 2 + 2) as usize;
        core::str::from_utf8(&payload[layout.pool + start..layout.pool + end])
            .ok()
            .filter(|tzid| !tzid.is_empty())
    }

    /// Maps a poly id to its feature id via the parent table.
    fn parent_of(&self, poly_id: u16) -> u16 {
        read_u16(
            self.payload_bytes(),
            self.layout.parent + poly_id as usize * 2,
        )
    }

    /// Tests one polygon: a bbox gate, then even-odd PIP at the width the
    /// header demands. Grid candidates are polys localized to the CELL;
    /// the record's bbox is the point-granular refinement, so a miss
    /// returns before touching any arc. The lazy path streams the arcs
    /// straight off the asset bytes through the per-edge kernel: junction
    /// vertices are shared by consecutive arcs and the ring closure is a
    /// shared junction too, so the ring's segment set is exactly the
    /// union of each arc's internal segments. Every arc is walked FORWARD
    /// (the orientation bit is ignored) with O(1) state, and parity XORs
    /// across arcs order-independently.
    fn poly_contains(&self, poly_id: u16, px: i32, py: i32) -> bool {
        #[cfg(feature = "geom-full-rings")]
        if self.layout.geom == GeomEncoding::FullRings {
            return self.full_rings_poly_contains(poly_id, px, py);
        }
        #[cfg(feature = "alloc")]
        if let Some(eager) = &self.eager {
            return self.eager_poly_contains(eager, poly_id, px, py);
        }
        let (layout, payload) = (&self.layout, self.payload_bytes());
        let coord_bytes = layout.quant_bits.bytes();
        let mut position = layout.ring_data
            + read_u32(payload, layout.poly_offsets + poly_id as usize * 4) as usize;
        let bbox = [
            read_fixed(payload, position, layout.quant_bits),
            read_fixed(payload, position + coord_bytes, layout.quant_bits),
            read_fixed(payload, position + 2 * coord_bytes, layout.quant_bits),
            read_fixed(payload, position + 3 * coord_bytes, layout.quant_bits),
        ];
        if !(px >= bbox[0] && py >= bbox[1] && px <= bbox[2] && py <= bbox[3]) {
            return false;
        }
        position += 4 * coord_bytes;
        let ring_count = read_u16(payload, position);
        position += 2;
        let mut poly_inside = false;
        for _ in 0..ring_count {
            let (arc_ref_count, mut ref_position) = read_varint(payload, position);
            let mut ring_inside = false;
            for _ in 0..arc_ref_count {
                let (arc_ref, after_ref) = read_varint(payload, ref_position);
                ref_position = after_ref;
                match self.scan_arc((arc_ref >> 1) as usize, px, py) {
                    pip::RingHit::Boundary => return true, // border points claimed
                    pip::RingHit::Inside => ring_inside = !ring_inside,
                    pip::RingHit::Outside => {}
                }
            }
            position = ref_position;
            if ring_inside {
                poly_inside = !poly_inside;
            }
        }
        poly_inside
    }

    /// Folds one arc's internal segments through the edge kernel. An
    /// `Inside` result means this arc contributed an odd number of ray
    /// crossings.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "coords accumulate i16/i24/i32-width deltas; sums fit i32 by format"
    )]
    fn scan_arc(&self, id: usize, px: i32, py: i32) -> pip::RingHit {
        let (layout, payload) = (&self.layout, self.payload_bytes());
        let wide = layout.quant_bits == QuantBits::Bits32;
        let fixed_width =
            caps::GEOM_FIXED_WIDTH_ARCS && layout.geom == GeomEncoding::FixedWidthArcs;
        let mut position =
            layout.arc_data + read_u32(payload, layout.arc_offsets + id * 4) as usize;
        let (vertex_count, after_count) = read_varint(payload, position);
        position = after_count;
        let coord_bytes = layout.quant_bits.bytes();
        let mut x = i64::from(read_fixed(payload, position, layout.quant_bits));
        let mut y = i64::from(read_fixed(
            payload,
            position + coord_bytes,
            layout.quant_bits,
        ));
        position += 2 * coord_bytes;
        let mut inside = false;
        let (mut x0, mut y0) = (x as i32, y as i32);
        for _ in 1..vertex_count {
            let (x1, y1) = if fixed_width {
                let x1 = read_fixed(payload, position, layout.quant_bits);
                let y1 = read_fixed(payload, position + coord_bytes, layout.quant_bits);
                position += 2 * coord_bytes;
                (x1, y1)
            } else {
                let (dx, after_dx) = read_varint(payload, position);
                let (dy, after_deltas) = read_varint(payload, after_dx);
                position = after_deltas;
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

    /// Tests one polygon on the `FullRings` path (geom=2), where the
    /// payload geometry IS the eager cache. One generic slice kernel
    /// folds straight off the payload bytes (flash in zero-copy mode).
    /// The coord width follows the quant width: i16 and i32 are read as
    /// typed pairs, and i24 as [`pip::Pack24`] (align 1, so no alignment
    /// requirement). This path works on the bare `core` rung.
    #[cfg(feature = "geom-full-rings")]
    fn full_rings_poly_contains(&self, poly_id: u16, px: i32, py: i32) -> bool {
        let (layout, payload) = (&self.layout, self.payload_bytes());
        let poly_record = layout.full_polys + poly_id as usize * 20;
        let bbox = [
            read_u32(payload, poly_record).cast_signed(),
            read_u32(payload, poly_record + 4).cast_signed(),
            read_u32(payload, poly_record + 8).cast_signed(),
            read_u32(payload, poly_record + 12).cast_signed(),
        ];
        if !(px >= bbox[0] && py >= bbox[1] && px <= bbox[2] && py <= bbox[3]) {
            return false;
        }
        let ring_end = read_u32(payload, poly_record + 16) as usize;
        let ring_start = if poly_id == 0 {
            0
        } else {
            read_u32(payload, poly_record - 4) as usize
        };
        match layout.quant_bits {
            QuantBits::Bits16 => {
                // an in-bbox point of a valid i16-quant asset always fits
                // i16; the fallthrough covers adversarial bboxes
                let (Ok(px), Ok(py)) = (i16::try_from(px), i16::try_from(py)) else {
                    return false;
                };
                self.full_rings_fold(ring_start, ring_end, px, py, ring_hit_narrow::<(i16, i16)>)
            }
            QuantBits::Bits24 => {
                self.full_rings_fold(ring_start, ring_end, px, py, ring_hit_narrow::<pip::Pack24>)
            }
            QuantBits::Bits32 => self.full_rings_fold(ring_start, ring_end, px, py, ring_hit_wide),
        }
    }

    /// Runs the even-odd fold over one poly's rings
    /// `[ring_start, ring_end)` at pair type `P`: `size_of::<P>()` IS the
    /// stored coordinate stride, and `scan` is the width-matched
    /// per-target ring kernel ([`ring_hit_narrow()`] /
    /// [`ring_hit_wide()`]). (No `cast_ptr_alignment` expect is needed
    /// anymore: the cast target is the opaque `P`, so the lint can't see
    /// a concrete alignment; the invariant itself is stated in the SAFETY
    /// comment below.)
    #[cfg(feature = "geom-full-rings")]
    fn full_rings_fold<P: pip::CoordPair>(
        &self,
        ring_start: usize,
        ring_end: usize,
        px: P::Narrow,
        py: P::Narrow,
        scan: impl Fn(&[P], P::Narrow, P::Narrow) -> pip::RingHit,
    ) -> bool {
        let (layout, payload) = (&self.layout, self.payload_bytes());
        let mut inside = false;
        let mut coord_start = if ring_start == 0 {
            0
        } else {
            read_u32(payload, layout.full_ring_ends + (ring_start - 1) * 4) as usize
        };
        for ring_index in ring_start..ring_end {
            let coord_end = read_u32(payload, layout.full_ring_ends + ring_index * 4) as usize;
            let coord_count = coord_end - coord_start;
            // SAFETY (slice cast): pair layouts are asserted at the top of
            // this file (Pack24 is align 1; i16/i32 pairs land aligned
            // because full_coords is 4-aligned — checked at load — and their
            // strides are multiples of the element alignment); parse bounds
            // the full-rings sections against the header counts.
            let ring = unsafe {
                core::slice::from_raw_parts(
                    payload[layout.full_coords + coord_start * core::mem::size_of::<P>()..]
                        .as_ptr()
                        .cast::<P>(),
                    coord_count,
                )
            };
            coord_start = coord_end;
            match scan(ring, px, py) {
                pip::RingHit::Boundary => return true,
                pip::RingHit::Inside => inside = !inside,
                pip::RingHit::Outside => {}
            }
        }
        inside
    }

    /// Runs the eager path: the same even-odd fold over the pre-decoded
    /// poly (indexed directly by poly id). The preload-computed bbox
    /// still skips whole folds for candidates that touch the cell but not
    /// the point.
    #[cfg(feature = "alloc")]
    fn eager_poly_contains(&self, eager: &Eager, poly_id: u16, px: i32, py: i32) -> bool {
        let poly_index = poly_id as usize;
        let (bbox, ring_end) = eager.polys[poly_index];
        if !(px >= bbox[0] && py >= bbox[1] && px <= bbox[2] && py <= bbox[3]) {
            return false;
        }
        let ring_start = if poly_index == 0 {
            0
        } else {
            eager.polys[poly_index - 1].1 as usize
        };
        match &eager.coords {
            EagerCoords::Narrow(coords) => {
                // an in-bbox point of a valid i16-quant asset always fits
                // i16; the fallthrough covers adversarial bboxes
                let (Ok(px), Ok(py)) = (i16::try_from(px), i16::try_from(py)) else {
                    return false;
                };
                rings_hit(
                    coords,
                    &eager.ring_ends,
                    ring_start,
                    ring_end as usize,
                    px,
                    py,
                    ring_hit_narrow,
                )
            }
            EagerCoords::Wide(coords) if self.layout.quant_bits == QuantBits::Bits32 => rings_hit(
                coords,
                &eager.ring_ends,
                ring_start,
                ring_end as usize,
                px,
                py,
                ring_hit_wide,
            ),
            EagerCoords::Wide(coords) => rings_hit(
                coords,
                &eager.ring_ends,
                ring_start,
                ring_end as usize,
                px,
                py,
                ring_hit_narrow::<(i32, i32)>,
            ),
        }
    }

    /// Decodes one signed arc ref onto the end of `coords`
    /// (join-deduplicated). This is eager-mode decode only; the lazy path
    /// streams via `scan_arc()` instead.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "coords accumulate i16/i24/i32-width deltas; sums fit i32 by format"
    )]
    fn append_arc<C: EagerCoord>(&self, arc_ref: u32, coords: &mut Vec<C>) {
        let (header, payload) = (&self.layout, self.payload_bytes());
        let (id, reversed) = ((arc_ref >> 1) as usize, (arc_ref & 1) == 1);
        let mut position =
            header.arc_data + read_u32(payload, header.arc_offsets + id * 4) as usize;
        let (vertex_count, after_count) = read_varint(payload, position);
        position = after_count;
        let coord_bytes = header.quant_bits.bytes();
        let mut qlon = i64::from(read_fixed(payload, position, header.quant_bits));
        let mut qlat = i64::from(read_fixed(
            payload,
            position + coord_bytes,
            header.quant_bits,
        ));
        position += 2 * coord_bytes;
        let start = coords.len();
        coords.push(C::from_q(qlon as i32, qlat as i32));
        for _ in 1..vertex_count {
            if caps::GEOM_FIXED_WIDTH_ARCS && header.geom == GeomEncoding::FixedWidthArcs {
                coords.push(C::from_q(
                    read_fixed(payload, position, header.quant_bits),
                    read_fixed(payload, position + coord_bytes, header.quant_bits),
                ));
                position += 2 * coord_bytes;
            } else {
                let (dlon, after_dlon) = read_varint(payload, position);
                let (dlat, after_dlat) = read_varint(payload, after_dlon);
                position = after_dlat;
                qlon += unzigzag(dlon);
                qlat += unzigzag(dlat);
                coords.push(C::from_q(qlon as i32, qlat as i32));
            }
        }
        if reversed {
            coords[start..].reverse();
        }
        // drop the duplicated junction vertex where this arc joins the previous
        if start > 0 && coords.get(start - 1) == coords.get(start) {
            coords.remove(start);
        }
    }
}

/// The ring-scan kernel for i16/i24-quant geometry. It dispatches to the
/// sign-split kernel on 32-bit targets (0.61–0.72× the i64 kernel there:
/// its magnitudes take single widening multiplies where the W kernels'
/// (b+1)-bit differences force full wide ones) and to the generic i64
/// kernel on 64-bit targets (one-instruction wide multiplies; sign-split
/// measured 2.3× SLOWER on a 64-bit host). Ring results are identical
/// either way, so answers stay platform-independent.
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

/// The i32-quant sibling of [`ring_hit_narrow()`]: it dispatches to
/// sign-split on 32-bit targets (0.24× the i128 kernel there) and to
/// i128 on 64-bit ones (where i128 measured 0.75× of even the i64
/// kernel).
#[cfg(any(feature = "alloc", feature = "geom-full-rings"))]
fn ring_hit_wide(ring: &[(i32, i32)], px: i32, py: i32) -> pip::RingHit {
    #[cfg(target_pointer_width = "32")]
    return pip::ring_hit_split(ring, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::ring_hit::<i128, _>(ring, px, py)
}

/// The streaming per-edge kernel for `scan_arc()`, which follows the same
/// per-target policy as the ring dispatchers above.
fn edge_narrow(a: (i32, i32), b: (i32, i32), px: i32, py: i32) -> pip::EdgeHit {
    #[cfg(target_pointer_width = "32")]
    return pip::edge_split(a, b, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::edge::<i64, _>(a, b, px, py)
}

/// The i32-quant sibling of [`edge_narrow()`].
fn edge_wide(a: (i32, i32), b: (i32, i32), px: i32, py: i32) -> pip::EdgeHit {
    #[cfg(target_pointer_width = "32")]
    return pip::edge_split(a, b, px, py);
    #[cfg(not(target_pointer_width = "32"))]
    pip::edge::<i128, _>(a, b, px, py)
}

/// Runs the even-odd fold over consecutive rings `[ring_start, ring_end)`
/// of a flat eager cache, shared by both cache widths ([`EagerCoords`]);
/// `scan` is the width-matched per-target ring kernel
/// ([`ring_hit_narrow()`] / [`ring_hit_wide()`]).
#[cfg(feature = "alloc")]
fn rings_hit<P: pip::CoordPair>(
    coords: &[P],
    ring_ends: &[u32],
    ring_start: usize,
    ring_end: usize,
    px: P::Narrow,
    py: P::Narrow,
    scan: impl Fn(&[P], P::Narrow, P::Narrow) -> pip::RingHit,
) -> bool {
    let mut inside = false;
    let mut coord_start = if ring_start == 0 {
        0
    } else {
        ring_ends[ring_start - 1] as usize
    };
    for coord_end in &ring_ends[ring_start..ring_end] {
        let coord_end = *coord_end as usize;
        match scan(&coords[coord_start..coord_end], px, py) {
            pip::RingHit::Boundary => return true,
            pip::RingHit::Inside => inside = !inside,
            pip::RingHit::Outside => {}
        }
        coord_start = coord_end;
    }
    inside
}
