//! What does dropping delta+varint geometry cost in flash? (The
//! fixed-width-arcs / streaming-eager question.)
//!
//! For a codec-*none* asset, the command rebuilds the payload in two
//! variants and compresses all three with the preset encoders
//! (`utz_encode::compress()`):
//!
//! - Variant **A (fixed-width arcs)** re-emits the interned arc store as
//!   absolute fixed-width coords (no deltas, no varints). Streaming/XIP
//!   lookups would skip the per-vertex varint decode, giving near-eager
//!   speed with zero RAM cache.
//! - Variant **B (eager layout)** flattens geometry per ring as i32 pairs,
//!   the exact `preload()` cache image, so after decompression the buffer
//!   *is* the eager cache (shared arcs are duplicated, like preload does).
//!
//! Section splicing only rewrites the geometry blocks; header offset fields
//! go stale, which is fine for a size measurement.
//!
//! ```text
//! utz_dev_cli fixedwidth-size \
//!     utz_data_tiny_static/data/tiny-static.utz <compact-none.utz> ...
//! ```

use utz::format::{self, read_fixed, read_u16, read_u32, read_varint, unzigzag};
use utz_common::GeomEncoding;
use utz_encode::encode::{compress, Codec};

fn write_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn write_fixed(value: i32, coord_bytes: usize, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.cast_unsigned().to_le_bytes()[..coord_bytes]);
}

/// Decodes one arc (forward orientation) into (i32, i32) coords.
fn arc_coords(payload: &[u8], header: &format::PayloadLayout, id: usize) -> Vec<(i32, i32)> {
    let coord_bytes = header.quant_bits.bytes();
    let mut position = header.arc_data + read_u32(payload, header.arc_offsets + id * 4) as usize;
    let (vcount, after_vcount) = read_varint(payload, position);
    position = after_vcount;
    let mut qlon = i64::from(read_fixed(payload, position, header.quant_bits));
    let mut qlat = i64::from(read_fixed(
        payload,
        position + coord_bytes,
        header.quant_bits,
    ));
    position += 2 * coord_bytes;
    let mut coords = Vec::with_capacity(usize::try_from(vcount).expect("vcount fits usize"));
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

#[derive(clap::Args)]
pub struct Args {
    /// The codec-none .utz asset path(s).
    #[arg(required = true)]
    paths: Vec<String>,
}

/// # Errors
/// The command fails on an I/O error reading an input asset or on a
/// compression backend failure.
///
/// # Panics
/// The command panics if an input is not a codec-none arc-store .utz asset
/// (geom 0/1), or if its path has no file stem.
pub fn run(args: &Args) -> utz_build::Result<()> {
    println!(
        "{:<28} {:>9} {:>9} {:>9} {:>9}",
        "payload variant", "raw", "gzip", "xz", "brotli"
    );
    for path in &args.paths {
        let bytes = std::fs::read(path)?;
        let start = format::outer(&bytes).expect("not a utz container");
        let container_payload = &bytes[start + format::PAYLOAD_HEADER_LEN..];
        let header = format::parse(&bytes[start..]).unwrap();
        assert_eq!(
            header.codec,
            utz::Codec::Uncompressed,
            "{path}: need a codec-none container"
        );
        assert!(
            matches!(
                header.geom,
                GeomEncoding::VarintArcs | GeomEncoding::FixedWidthArcs
            ),
            "arc-store containers only (geom 0/1)"
        );
        let coord_bytes = header.quant_bits.bytes();
        let arcs_offset = header.arc_offsets; // the arc block starts at its offsets table
        let grid_block = header.primary; // the grid starts at its primary cell table

        let payload_a = variant_fixed_arcs(container_payload, &header, coord_bytes, arcs_offset);
        let payload_b = variant_eager_image(
            container_payload,
            &header,
            coord_bytes,
            arcs_offset,
            grid_block,
            path,
        );

        let name = std::path::Path::new(&path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for (label, payload) in [
            (format!("{name} varint (today)"), container_payload.to_vec()),
            (format!("{name} A fixed arcs"), payload_a),
            (format!("{name} B eager image"), payload_b),
        ] {
            #[expect(
                clippy::cast_precision_loss,
                reason = "payload byte counts ≪ 2^53; KiB display"
            )]
            let kib = |len: usize| format!("{:.1}K", len as f64 / 1024.0);
            println!(
                "{:<28} {:>9} {:>9} {:>9} {:>9}",
                label,
                kib(payload.len()),
                kib(compress(&payload, Codec::Gzip)?.len()),
                kib(compress(&payload, Codec::Xz)?.len()),
                kib(compress(&payload, Codec::Brotli)?.len()),
            );
        }
    }
    Ok(())
}

/// Variant A: the arc store rewritten as absolute fixed-width coords,
/// everything else unchanged.
fn variant_fixed_arcs(
    payload: &[u8],
    header: &format::PayloadLayout,
    coord_bytes: usize,
    arcs_offset: usize,
) -> Vec<u8> {
    let mut a_offsets: Vec<u32> = Vec::with_capacity(header.n_arcs as usize + 1);
    let mut a_data: Vec<u8> = Vec::new();
    for id in 0..header.n_arcs as usize {
        a_offsets.push(u32::try_from(a_data.len()).expect("arc data fits u32"));
        let coords = arc_coords(payload, header, id);
        write_varint(coords.len() as u64, &mut a_data);
        for (x, y) in coords {
            write_fixed(x, coord_bytes, &mut a_data);
            write_fixed(y, coord_bytes, &mut a_data);
        }
    }
    a_offsets.push(u32::try_from(a_data.len()).expect("arc data fits u32"));
    let mut payload_a = payload[..arcs_offset].to_vec();
    payload_a.extend_from_slice(&header.n_arcs.to_le_bytes());
    for offset in &a_offsets {
        payload_a.extend_from_slice(&offset.to_le_bytes());
    }
    payload_a.extend_from_slice(&a_data);
    payload_a.extend_from_slice(&payload[header.parent..]);
    payload_a
}

/// Variant B: geometry replaced by per-ring flattened i32 pairs (the
/// `preload()` cache image), grid unchanged.
fn variant_eager_image(
    payload: &[u8],
    header: &format::PayloadLayout,
    coord_bytes: usize,
    arcs_offset: usize,
    grid_block: usize,
    path: &str,
) -> Vec<u8> {
    let n_polys = header.eager_polys as usize;
    let mut coords: Vec<u8> = Vec::new(); // (i32, i32) pairs
    let mut ring_ends: Vec<u8> = Vec::new(); // u32
    let mut polys: Vec<u8> = Vec::new(); // [i32; 4] bbox + u32 ring_end
    let (mut ncoords, mut nrings) = (0u32, 0u32);
    for pid in 0..n_polys {
        let mut position =
            header.ring_data + read_u32(payload, header.poly_offsets + pid * 4) as usize;
        let bbox: Vec<i32> = (0..4)
            .map(|i| read_fixed(payload, position + i * coord_bytes, header.quant_bits))
            .collect();
        position += 4 * coord_bytes;
        let ring_count = read_u16(payload, position);
        position += 2;
        for _ in 0..ring_count {
            let (nrefs, mut ref_position) = read_varint(payload, position);
            let ring_start = ncoords;
            let mut ring: Vec<(i32, i32)> = Vec::new();
            for _ in 0..nrefs {
                let (arc_ref, next_position) = read_varint(payload, ref_position);
                ref_position = next_position;
                let (id, reversed) = ((arc_ref >> 1) as usize, (arc_ref & 1) == 1);
                let mut arc = arc_coords(payload, header, id);
                if reversed {
                    arc.reverse();
                }
                ring.extend_from_slice(&arc);
            }
            position = ref_position;
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            for &(x, y) in &ring {
                coords.extend_from_slice(&x.to_le_bytes());
                coords.extend_from_slice(&y.to_le_bytes());
            }
            ncoords = ring_start + u32::try_from(ring.len()).expect("ring len fits u32");
            nrings += 1;
            ring_ends.extend_from_slice(&ncoords.to_le_bytes());
        }
        for value in bbox {
            polys.extend_from_slice(&value.to_le_bytes());
        }
        polys.extend_from_slice(&nrings.to_le_bytes());
    }
    // header eager_coords counts the ring-closure vertex preload() pops
    // (one per closed ring), so it may exceed the flattened image
    assert!(
        ncoords <= header.eager_coords,
        "{path}: coord count mismatch"
    );
    assert!(
        ncoords + nrings >= header.eager_coords,
        "{path}: coord count mismatch"
    );
    assert_eq!(nrings, header.eager_rings);
    let mut payload_b = payload[..arcs_offset].to_vec(); // header + zone strings
    payload_b.extend_from_slice(&payload[header.parent..header.parent + n_polys * 2]); // parent table
    payload_b.extend_from_slice(&coords);
    payload_b.extend_from_slice(&ring_ends);
    payload_b.extend_from_slice(&polys);
    payload_b.extend_from_slice(&payload[grid_block..]); // grid unchanged
    payload_b
}
