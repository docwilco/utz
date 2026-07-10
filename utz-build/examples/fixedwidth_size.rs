//! What does dropping delta+varint geometry cost in flash? (PLAN §13
//! fixed-width arcs / §14.10 streaming-eager discussion.)
//!
//! For a codec-*none* container, rebuilds the payload in two variants and
//! compresses all three with the preset encoders (`utz_encode::compress`):
//!
//! - **A — fixed-width arcs**: the interned arc store re-emitted as absolute
//!   fixed-width coords (no deltas, no varints). Streaming/XIP lookups would
//!   skip the per-vertex varint decode — near-eager speed, zero RAM cache.
//! - **B — eager layout**: geometry flattened per ring as i32 pairs — the
//!   exact `preload()` cache image, so after decompression the buffer IS the
//!   eager cache (shared arcs duplicated, like preload does).
//!
//! Section splicing only rewrites the geometry blocks; header offset fields
//! go stale, which is fine for a size measurement.
//!
//!     cargo run --release -p utz-build --example fixedwidth_size -- \
//!         utz-data-tiny-static/data/tiny-static.utz <compact-none.utz> ...

use utz::format::{self, fixed_bytes, read_fixed, read_u16, read_u32, read_varint, unzigzag};
use utz_build::encode::{compress, Codec};

fn write_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn write_fixed(v: i32, fb: usize, out: &mut Vec<u8>) {
    out.extend_from_slice(&(v as u32).to_le_bytes()[..fb]);
}

/// Decode one arc (forward orientation) into (i32, i32) coords.
fn arc_coords(p: &[u8], h: &format::Header, id: usize) -> Vec<(i32, i32)> {
    let fb = fixed_bytes(h.quant_bits);
    let mut pos = h.arc_data + read_u32(p, h.arc_offsets + id * 4) as usize;
    let (vcount, p2) = read_varint(p, pos);
    pos = p2;
    let mut x = i64::from(read_fixed(p, pos, h.quant_bits));
    let mut y = i64::from(read_fixed(p, pos + fb, h.quant_bits));
    pos += 2 * fb;
    let mut coords = Vec::with_capacity(usize::try_from(vcount).expect("vcount fits usize"));
    let c = |v: i64| i32::try_from(v).expect("quantized coord fits i32");
    coords.push((c(x), c(y)));
    for _ in 1..vcount {
        let (dx, p3) = read_varint(p, pos);
        let (dy, p4) = read_varint(p, p3);
        pos = p4;
        x += unzigzag(dx);
        y += unzigzag(dy);
        coords.push((c(x), c(y)));
    }
    coords
}

fn main() -> utz_build::Result<()> {
    println!(
        "{:<28} {:>9} {:>9} {:>9} {:>9}",
        "payload variant", "raw", "gzip", "xz", "brotli"
    );
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (codec, _, start) = format::outer(&bytes).expect("not a utz container");
        assert_eq!(codec, 0, "{path}: need a codec-none container");
        let p = &bytes[start..];
        let h = format::parse(p).unwrap();
    assert!(h.geom <= 1, "arc-store containers only (geom 0/1)");
        let fb = fixed_bytes(h.quant_bits);
        let arcs_off = h.arc_offsets - 4; // n_arcs u32 sits before the table
        let grid_block = h.primary - 4; // ncols/nrows u16s before primary

        // --- A: arc store as absolute fixed-width coords, rest unchanged ---
        let mut a_offsets: Vec<u32> = Vec::with_capacity(h.n_arcs as usize + 1);
        let mut a_data: Vec<u8> = Vec::new();
        for id in 0..h.n_arcs as usize {
            a_offsets.push(u32::try_from(a_data.len()).expect("arc data fits u32"));
            let coords = arc_coords(p, &h, id);
            write_varint(coords.len() as u64, &mut a_data);
            for (x, y) in coords {
                write_fixed(x, fb, &mut a_data);
                write_fixed(y, fb, &mut a_data);
            }
        }
        a_offsets.push(u32::try_from(a_data.len()).expect("arc data fits u32"));
        let mut pa = p[..arcs_off].to_vec();
        pa.extend_from_slice(&h.n_arcs.to_le_bytes());
        for o in &a_offsets {
            pa.extend_from_slice(&o.to_le_bytes());
        }
        pa.extend_from_slice(&a_data);
        pa.extend_from_slice(&p[h.parent..]);

        // --- B: per-ring flattened i32 pairs (the preload() cache image) ---
        let n_polys = h.eager_polys as usize;
        let mut coords: Vec<u8> = Vec::new(); // (i32, i32) pairs
        let mut ring_ends: Vec<u8> = Vec::new(); // u32
        let mut polys: Vec<u8> = Vec::new(); // [i32; 4] bbox + u32 ring_end
        let (mut ncoords, mut nrings) = (0u32, 0u32);
        for pid in 0..n_polys {
            let mut pos = h.ring_data + read_u32(p, h.poly_offsets + pid * 4) as usize;
            let bb: Vec<i32> =
                (0..4).map(|i| read_fixed(p, pos + i * fb, h.quant_bits)).collect();
            pos += 4 * fb;
            let nr = read_u16(p, pos);
            pos += 2;
            for _ in 0..nr {
                let (nrefs, mut p2) = read_varint(p, pos);
                let start_n = ncoords;
                let mut ring: Vec<(i32, i32)> = Vec::new();
                for _ in 0..nrefs {
                    let (r, p3) = read_varint(p, p2);
                    p2 = p3;
                    let (id, rev) = ((r >> 1) as usize, (r & 1) == 1);
                    let mut c = arc_coords(p, &h, id);
                    if rev {
                        c.reverse();
                    }
                    ring.extend_from_slice(&c);
                }
                pos = p2;
                if ring.len() > 1 && ring.first() == ring.last() {
                    ring.pop();
                }
                for &(x, y) in &ring {
                    coords.extend_from_slice(&x.to_le_bytes());
                    coords.extend_from_slice(&y.to_le_bytes());
                }
                ncoords = start_n + u32::try_from(ring.len()).expect("ring len fits u32");
                nrings += 1;
                ring_ends.extend_from_slice(&ncoords.to_le_bytes());
            }
            for v in bb {
                polys.extend_from_slice(&v.to_le_bytes());
            }
            polys.extend_from_slice(&nrings.to_le_bytes());
        }
        // header eager_coords counts the ring-closure vertex preload() pops
        // (one per closed ring), so it may exceed the flattened image
        assert!(ncoords <= h.eager_coords, "{path}: coord count mismatch");
        assert!(ncoords + nrings >= h.eager_coords, "{path}: coord count mismatch");
        assert_eq!(nrings, h.eager_rings);
        let mut pb = p[..arcs_off].to_vec(); // header + zone strings
        pb.extend_from_slice(&p[h.parent..h.parent + n_polys * 2]); // parent table
        pb.extend_from_slice(&coords);
        pb.extend_from_slice(&ring_ends);
        pb.extend_from_slice(&polys);
        pb.extend_from_slice(&p[grid_block..]); // grid unchanged

        let name = std::path::Path::new(&path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for (label, payload) in [
            (format!("{name} varint (today)"), p.to_vec()),
            (format!("{name} A fixed arcs"), pa),
            (format!("{name} B eager image"), pb),
        ] {
            let k = |n: usize| format!("{:.1}K", n as f64 / 1024.0);
            println!(
                "{:<28} {:>9} {:>9} {:>9} {:>9}",
                label,
                k(payload.len()),
                k(compress(&payload, Codec::Gzip)?.len()),
                k(compress(&payload, Codec::Xz)?.len()),
                k(compress(&payload, Codec::Brotli)?.len()),
            );
        }
    }
    Ok(())
}
