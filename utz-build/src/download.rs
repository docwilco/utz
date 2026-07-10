//! Conditional-GET download cache (PLAN.md §5 step 1): store `ETag` /
//! `Last-Modified` next to each cached file and revalidate with
//! `If-None-Match` / `If-Modified-Since`; a 304 reuses the cache untouched.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Fetch `url` into `cache_dir`, revalidating any cached copy. Returns the
/// cached file path. Offline with a cached copy present → warn + reuse.
///
/// # Errors
/// URL without a filename component, HTTP failure with no cached copy to
/// fall back on, or I/O failure writing the cache.
pub fn fetch(url: &str, cache_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let name = url.rsplit('/').next().filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no filename in url {url}"))?;
    let file = cache_dir.join(name);
    let meta = cache_dir.join(format!("{name}.headers"));

    let mut req = ureq::get(url);
    if file.exists() {
        if let Ok(m) = std::fs::read_to_string(&meta) {
            for line in m.lines() {
                if let Some(v) = line.strip_prefix("etag: ") {
                    req = req.set("If-None-Match", v);
                } else if let Some(v) = line.strip_prefix("last-modified: ") {
                    req = req.set("If-Modified-Since", v);
                }
            }
        }
    }

    match req.call() {
        Ok(resp) if resp.status() == 304 => Ok(file),
        Ok(resp) => {
            use std::fmt::Write as _;
            let mut hdrs = String::new();
            if let Some(v) = resp.header("etag") {
                let _ = writeln!(hdrs, "etag: {v}");
            }
            if let Some(v) = resp.header("last-modified") {
                let _ = writeln!(hdrs, "last-modified: {v}");
            }
            let mut bytes = Vec::new();
            resp.into_reader().read_to_end(&mut bytes)?;
            let tmp = file.with_extension("part");
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &file)?;
            std::fs::write(&meta, hdrs)?;
            Ok(file)
        }
        Err(e) if file.exists() => {
            eprintln!("warning: revalidation of {url} failed ({e}); using cached copy");
            Ok(file)
        }
        Err(e) => Err(e.into()),
    }
}
