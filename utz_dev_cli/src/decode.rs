//! Shared read-side decoding for the measurement commands, built on the
//! `utz::format` primitives. The runtime reader streams arcs privately
//! inside its lookup kernels; the tools here decode whole arcs into
//! memory instead, so they share this helper rather than the reader's.

use utz::format::{self, read_fixed, read_u32, read_varint, unzigzag};
use utz_common::GeomEncoding;

/// Decodes one arc (forward orientation) into (i32, i32) coords,
/// handling both arc-store encodings (delta+varint and fixed-width).
///
/// # Panics
/// Panics on a malformed arc store (a count or coordinate out of range).
#[must_use]
pub fn arc_coords(payload: &[u8], header: &format::PayloadLayout, id: usize) -> Vec<(i32, i32)> {
    let coord_bytes = header.quant_bits.bytes();
    let mut position = header.arc_data + read_u32(payload, header.arc_offsets + id * 4) as usize;
    let (vcount, after_vcount) = read_varint(payload, position);
    position = after_vcount;
    let mut coords = Vec::with_capacity(usize::try_from(vcount).expect("vcount fits usize"));
    if header.geom == GeomEncoding::FixedWidthArcs {
        for _ in 0..vcount {
            coords.push((
                read_fixed(payload, position, header.quant_bits),
                read_fixed(payload, position + coord_bytes, header.quant_bits),
            ));
            position += 2 * coord_bytes;
        }
        return coords;
    }
    let mut qlon = i64::from(read_fixed(payload, position, header.quant_bits));
    let mut qlat = i64::from(read_fixed(
        payload,
        position + coord_bytes,
        header.quant_bits,
    ));
    position += 2 * coord_bytes;
    let to_i32 = |coord: i64| i32::try_from(coord).expect("quantized coord fits i32");
    coords.push((to_i32(qlon), to_i32(qlat)));
    for _ in 1..vcount {
        let (dlon, after_dlon) = read_varint(payload, position);
        let (dlat, after_dlat) = read_varint(payload, after_dlon);
        position = after_dlat;
        qlon += unzigzag(dlon);
        qlat += unzigzag(dlat);
        coords.push((to_i32(qlon), to_i32(qlat)));
    }
    coords
}
