//! The conditional-GET download cache stores `ETag` / `Last-Modified`
//! next to each cached file and revalidates with `If-None-Match` /
//! `If-Modified-Since`; a 304 reuses the cache untouched.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Fetches `url` into `cache_dir`, revalidating any cached copy, and
/// returns the cached file path. When offline with a cached copy present,
/// it warns and reuses the copy.
///
/// # Errors
/// Returns an error on a URL without a filename component, an HTTP
/// failure with no cached copy to fall back on, or an I/O failure writing
/// the cache.
pub fn fetch(url: &str, cache_dir: &Path) -> crate::Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let name = url
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .ok_or_else(|| crate::Error::NoFilename { url: url.into() })?;
    let file = cache_dir.join(name);
    let headers_path = cache_dir.join(format!("{name}.headers"));

    let mut request = ureq::get(url);
    if file.exists()
        && let Ok(cached_headers) = std::fs::read_to_string(&headers_path)
    {
        for line in cached_headers.lines() {
            if let Some(value) = line.strip_prefix("etag: ") {
                request = request.header("If-None-Match", value);
            } else if let Some(value) = line.strip_prefix("last-modified: ") {
                request = request.header("If-Modified-Since", value);
            }
        }
    }

    match request.call() {
        Ok(response) if response.status() == 304 => Ok(file),
        Ok(response) => {
            use std::fmt::Write as _;
            let header = |name: &str| {
                response
                    .headers()
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            let mut headers = String::new();
            if let Some(value) = header("etag") {
                let _ = writeln!(headers, "etag: {value}");
            }
            if let Some(value) = header("last-modified") {
                let _ = writeln!(headers, "last-modified: {value}");
            }
            let mut bytes = Vec::new();
            response.into_body().into_reader().read_to_end(&mut bytes)?;
            let partial_path = file.with_extension("part");
            std::fs::write(&partial_path, &bytes)?;
            std::fs::rename(&partial_path, &file)?;
            std::fs::write(&headers_path, headers)?;
            Ok(file)
        }
        Err(error) if file.exists() => {
            eprintln!("warning: revalidation of {url} failed ({error}); using cached copy");
            Ok(file)
        }
        Err(error) => Err(error.into()),
    }
}
