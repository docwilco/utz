// Exact memory of a 2deg grid: measure candidate (zone) counts per border cell,
// then size several layouts, showing 32- vs 64-bit differences.
// usage: utz-build grid2mem [ds] [deg]
use std::collections::HashSet;

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// grid cell size in degrees
    #[arg(default_value_t = 2.0)]
    deg: f64,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let (ds, d) = (a.ds, a.deg);

    // rings tagged with their feature (zone) id
    let feats = utz_build::load(&ds)?;
    let rings: Vec<(u16, Vec<(f64, f64)>)> = feats.iter().enumerate()
        .flat_map(|(fid, f)| f.polys.iter().flatten().map(move |r| (fid as u16, r.clone())))
        .collect();
    let nfeat = rings.iter().map(|(f, _)| *f).max().unwrap_or(0) as usize + 1;

    let ncols = (360.0 / d).ceil() as usize;
    let nrows = (180.0 / d).ceil() as usize;
    let total = ncols * nrows;
    let mut sets: Vec<HashSet<u16>> = vec![HashSet::new(); total];
    let cell = |lon: f64, lat: f64| -> usize {
        let c = (((lon + 180.0) / d) as isize).clamp(0, ncols as isize - 1) as usize;
        let r = (((lat + 90.0) / d) as isize).clamp(0, nrows as isize - 1) as usize;
        r * ncols + c
    };
    for (fid, ring) in &rings {
        let n = ring.len();
        for i in 0..n {
            let (x0, y0) = ring[i];
            let (x1, y1) = ring[(i + 1) % n];
            let steps = ((((x1 - x0).abs()).max((y1 - y0).abs()) / d * 2.0).ceil() as usize).max(1);
            for s in 0..=steps {
                let t = s as f64 / steps as f64;
                sets[cell(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)].insert(*fid);
            }
        }
    }

    let border: usize = sets.iter().filter(|s| s.len() > 1).count();
    let interior_or_empty = total - border;
    let multi_ids: usize = sets.iter().filter(|s| s.len() > 1).map(|s| s.len()).sum();
    let maxc = sets.iter().map(|s| s.len()).max().unwrap_or(0);

    println!("{} @ {d}deg  ({nfeat} zones)", ds.to_uppercase());
    println!("  grid: {ncols} x {nrows} = {total} cells");
    println!("  border cells (>1 zone): {border}   single/empty: {interior_or_empty}");
    println!("  candidate ids in border cells: {multi_ids}  (avg {:.2}/border, max {maxc})\n",
        multi_ids as f64 / border.max(1) as f64);

    // ---- layout A: flat CSR (fixed-width, platform-independent) ----
    // primary: u16 per cell (zone id, or spill index w/ high-bit flag)
    // offsets: u32 per border cell +1 ; ids: u16 per candidate entry
    let a = total * 2 + (border + 1) * 4 + multi_ids * 2;
    // ---- layout B: primary u16 + inline blob (count u8 + ids), offset u32 ----
    let b = total * 2 + (border + 1) * 4 + border + multi_ids * 2;
    // ---- layout C (naive): Vec<Vec<u16>> — platform dependent ----
    let vec_hdr32 = 12usize; let vec_hdr64 = 24usize;
    let alloc = 16usize; // rough per-allocation heap overhead
    // every non-empty cell heap-allocates its inner Vec
    let nonempty = total - sets.iter().filter(|s| s.is_empty()).count();
    let all_ids: usize = sets.iter().map(|s| s.len()).sum();
    let c32 = total * vec_hdr32 + nonempty * alloc + all_ids * 2;
    let c64 = total * vec_hdr64 + nonempty * alloc + all_ids * 2;

    // ---- interned CSR: dedup identical candidate lists (coastlines repeat {land,ocean}) ----
    let mut uniq: HashSet<Vec<u16>> = HashSet::new();
    for s in &sets {
        if s.len() > 1 { let mut v: Vec<u16> = s.iter().copied().collect(); v.sort_unstable(); uniq.insert(v); }
    }
    let uniq_lists = uniq.len();
    let uniq_ids: usize = uniq.iter().map(|v| v.len()).sum();
    // primary u16 + list_offsets u16[uniq+1] + list_ids u16[uniq_ids]
    let interned = total * 2 + (uniq_lists + 1) * 2 + uniq_ids * 2;

    let kb = |n: usize| format!("{:.1} KB", n as f64 / 1024.0);
    println!("  unique candidate lists among border cells: {uniq_lists}  ({uniq_ids} ids)");
    println!("  layout D  interned CSR (u16 everywhere): {}   <- dedup repeated lists", kb(interned));
    println!("  layout A  flat CSR (u16/u32 arrays):     {}   (32-bit == 64-bit, fixed width)", kb(a));
    println!("  layout B  flat + inline counts:          {}   (32-bit == 64-bit)", kb(b));
    println!("  layout C  naive Vec<Vec<u16>>  32-bit:   {}", kb(c32));
    println!("  layout C  naive Vec<Vec<u16>>  64-bit:   {}", kb(c64));
    Ok(())
}
