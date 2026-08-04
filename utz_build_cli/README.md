# utz_build_cli

μTZ asset-generation CLI: the `utz_build_cli` binary.

The command-line counterpart of a `build.rs` using
[`utz_build::Config`]: it writes `.utz` assets for flash partitions,
OTA images, and experiments, where a build script is the wrong shape.

```
utz_build_cli gen [ds] [eps_m] [--qbits 24] [--grid-deg 2]
    [--codec none|gzip|zstd|brotli|xz] [--algo rdp|vw|ii|none]
    [--geom varint-arcs|fixed-width-arcs|full-rings|coarse]
    [--w-min <mult>] [-o out.utz]
utz_build_cli gen-preset <tiny|tiny-static|compact|balanced|accurate>
```

[`cmd::encode`] is `gen`: every knob of
[`utz_build::Config`], one flag each. [`cmd::gen_preset`] drives the
canonical `Config::<preset>()` recipes, so preset assets regenerate
from a single source of truth. The repo-internal measurement and
viewer commands live in the (unpublished) `utz_dev_cli` crate.

[`utz_build::Config`]: https://docwilco.github.io/utz/docs/utz_build/config/struct.Config.html
[`cmd::encode`]: https://docwilco.github.io/utz/docs/utz_build_cli/cmd/encode/index.html
[`cmd::gen_preset`]: https://docwilco.github.io/utz/docs/utz_build_cli/cmd/gen_preset/index.html

License: MIT
