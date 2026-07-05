//! Quantization-artifact report: how badly does grid snapping mangle the
//! ring geometry (self-crossings, collinear self-overlaps, self-touches,
//! zero-area rings), and how much of that does the clean.rs pass remove.
//! Rings are assembled from the shared arcs exactly like the encoder does.
//!
//!     utz-build quant-clean [ds] [eps_m] [qbits...]

use utz_build::clean::{self, CleanStats};
use utz_build::topo;

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// quantization widths to report (16/24/32)
    #[arg(default_values_t = [16u32, 24])]
    qbits: Vec<u32>,
}

#[derive(Default, Clone, Copy)]
struct Bad {
    /// non-adjacent segment pairs that properly cross
    crossings: usize,
    /// non-adjacent collinear segment pairs overlapping in more than a point
    overlaps: usize,
    /// non-adjacent segment pairs touching in exactly one point
    touches: usize,
    /// rings with < 3 distinct vertices or zero area
    degenerate: usize,
    verts: usize,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let feats = utz_build::load(&a.ds)?;
    let t = topo::build_topology(&feats, a.eps_m / 111_320.0);
    println!("{} · RDP ε {} m · {} arcs, {} rings\n", a.ds, a.eps_m, t.arc_coords.len(), t.ring_refs.len());

    for &qbits in &a.qbits {
        anyhow::ensure!(matches!(qbits, 16 | 24 | 32), "qbits must be 16/24/32");
        let qmax = ((1u64 << (qbits - 1)) - 1) as f64;
        let quant = |a: &Vec<(f64, f64)>| -> Vec<(i32, i32)> {
            a.iter()
                .map(|&(x, y)| (((x / 180.0 * qmax).round()) as i32, ((y / 90.0 * qmax).round()) as i32))
                .collect()
        };

        // before: what the encoder used to ship (consecutive-dup collapse only)
        let raw: Vec<Vec<(i32, i32)>> = t.arc_coords.iter().map(|a| {
            let mut q = quant(a);
            q.dedup();
            q
        }).collect();
        let before = measure(t.ring_refs.iter().map(|r| clean::ring_coords_q(r, &raw)));

        // after: per-arc clean + degenerate-ring drop (what the encoder ships now)
        let mut cst = CleanStats::default();
        let cleaned: Vec<Vec<(i32, i32)>> = t.arc_coords.iter().map(|a| {
            let mut q = quant(a);
            let closed = a.len() > 1 && a.first() == a.last();
            clean::clean_arc(&mut q, closed, &mut cst);
            q
        }).collect();
        let (ring_refs, _, arcs) = clean::drop_degenerate_rings(&t.ring_refs, &t.structure, cleaned, &mut cst);
        let after = measure(ring_refs.iter().map(|r| clean::ring_coords_q(r, &arcs)));

        println!("i{qbits}");
        let row = |tag: &str, b: &Bad| {
            println!(
                "  {tag:<7} verts {:>8}  cross {:>5}  overlap {:>5}  touch {:>5}  degenerate rings {:>4}",
                b.verts, b.crossings, b.overlaps, b.touches, b.degenerate
            );
        };
        row("before:", &before);
        println!(
            "  clean:  dups {}  spikes {}  collinear {}  rings dropped {} (polys {}, arcs {})",
            cst.dups, cst.spikes, cst.collinear, cst.rings_dropped, cst.polys_dropped, cst.arcs_dropped
        );
        row("after:", &after);
        println!();
    }
    Ok(())
}

fn measure(rings: impl Iterator<Item = Vec<(i32, i32)>>) -> Bad {
    let mut b = Bad::default();
    for c in rings {
        b.verts += c.len();
        if clean::ring_degenerate(&c) {
            b.degenerate += 1;
            continue;
        }
        ring_bad(&c, &mut b);
    }
    b
}

/// Count non-adjacent segment pairs of one ring that intersect, split by
/// kind. Sweep over min-x-sorted segments — O(n log n + pairs-in-x-overlap),
/// fine at report scale.
fn ring_bad(c: &[(i32, i32)], b: &mut Bad) {
    let n = c.len();
    if n < 4 {
        return;
    }
    let seg = |i: usize| (c[i], c[(i + 1) % n]);
    let minx = |i: usize| { let (p, q) = seg(i); p.0.min(q.0) };
    let mut idx: Vec<u32> = (0..n as u32).collect();
    idx.sort_unstable_by_key(|&i| minx(i as usize));
    for ai in 0..n {
        let i = idx[ai] as usize;
        let (p1, p2) = seg(i);
        let (maxx, ymin, ymax) = (p1.0.max(p2.0), p1.1.min(p2.1), p1.1.max(p2.1));
        for &jj in &idx[ai + 1..] {
            let j = jj as usize;
            let (q1, q2) = seg(j);
            if q1.0.min(q2.0) > maxx {
                break;
            }
            if i.abs_diff(j) == 1 || i.abs_diff(j) == n - 1 {
                continue; // adjacent segments share a vertex by construction
            }
            if q1.1.max(q2.1) < ymin || q1.1.min(q2.1) > ymax {
                continue;
            }
            match seg_class((p1, p2), (q1, q2)) {
                Class::Cross => b.crossings += 1,
                Class::Overlap => b.overlaps += 1,
                Class::Touch => b.touches += 1,
                Class::None => {}
            }
        }
    }
}

enum Class {
    None,
    Touch,
    Cross,
    Overlap,
}

fn orient(a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> i128 {
    (b.0 as i128 - a.0 as i128) * (c.1 as i128 - a.1 as i128)
        - (b.1 as i128 - a.1 as i128) * (c.0 as i128 - a.0 as i128)
}

fn sgn(v: i128) -> i8 {
    (v > 0) as i8 - (v < 0) as i8
}

fn in_bbox(p: (i32, i32), a: (i32, i32), b: (i32, i32)) -> bool {
    p.0 >= a.0.min(b.0) && p.0 <= a.0.max(b.0) && p.1 >= a.1.min(b.1) && p.1 <= a.1.max(b.1)
}

fn seg_class((p1, p2): ((i32, i32), (i32, i32)), (q1, q2): ((i32, i32), (i32, i32))) -> Class {
    let (o1, o2) = (sgn(orient(p1, p2, q1)), sgn(orient(p1, p2, q2)));
    let (o3, o4) = (sgn(orient(q1, q2, p1)), sgn(orient(q1, q2, p2)));
    if o1 == 0 && o2 == 0 && o3 == 0 && o4 == 0 {
        // collinear: project on the dominant axis and compare 1D ranges
        let flat = p1.0 == p2.0 && q1.0 == q2.0;
        let val = |p: (i32, i32)| if flat { p.1 } else { p.0 };
        let (a0, a1) = (val(p1).min(val(p2)), val(p1).max(val(p2)));
        let (b0, b1) = (val(q1).min(val(q2)), val(q1).max(val(q2)));
        let (lo, hi) = (a0.max(b0), a1.min(b1));
        return match lo.cmp(&hi) {
            std::cmp::Ordering::Less => Class::Overlap,
            std::cmp::Ordering::Equal => Class::Touch,
            std::cmp::Ordering::Greater => Class::None,
        };
    }
    if o1 != o2 && o3 != o4 && o1 != 0 && o2 != 0 && o3 != 0 && o4 != 0 {
        return Class::Cross;
    }
    // an endpoint of one segment lying on the other = touch
    if (o1 == 0 && in_bbox(q1, p1, p2))
        || (o2 == 0 && in_bbox(q2, p1, p2))
        || (o3 == 0 && in_bbox(p1, q1, q2))
        || (o4 == 0 && in_bbox(p2, q1, q2))
    {
        return Class::Touch;
    }
    Class::None
}
