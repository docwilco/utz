# μTZ

{{readme}}

## License

Code: MIT. Timezone data is derived from
[timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder)
(OpenStreetMap, **ODbL**). Preset assets are simplified with
population-density weighting derived from
[GHS-POP R2023A](https://human-settlement.emergency.copernicus.eu/ghs_pop2023.php)
(European Commission JRC, **CC BY 4.0**).

[`Finder`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html
[`Finder::new()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.new
[`Finder::from_vec()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_vec
[`Finder::eager_from_slice()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.eager_from_slice
[`Error`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html
[`Error::BadMagic`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.BadMagic
[`Error::Truncated`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.Truncated
[`Error::UnsupportedVersion`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.UnsupportedVersion
[`Error::CodecNotCompiledIn`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.CodecNotCompiledIn
[`Error::GeometryNotCompiledIn`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.GeometryNotCompiledIn
[`Error::StaticAssetCompressed`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.StaticAssetCompressed
[`Error::Misaligned`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.Misaligned
[`Finder::from_slice()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_slice
[`Finder::from_static()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_static
[`Finder::preload()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload
[`Finder::preload_bytes()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload_bytes
[`Finder::lookup_coarse()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.lookup_coarse
[`data`]: https://docwilco.github.io/utz/docs/utz/data/index.html
[`caps`]: https://docwilco.github.io/utz/docs/utz/caps/index.html
[`crate::format`]: https://docwilco.github.io/utz/docs/utz/format/index.html
[`decompress`]: https://docwilco.github.io/utz/docs/utz/decompress/index.html
[`GeomEncoding`]: https://docwilco.github.io/utz/docs/utz/enum.GeomEncoding.html
[`include_bytes_aligned!`]: https://docwilco.github.io/utz/docs/utz/macro.include_bytes_aligned.html
