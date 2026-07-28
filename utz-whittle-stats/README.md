# utz-whittle-stats

Size-reduction statistics of the whittling-down pipeline, in one
place: the stage ladder `utz-build whittle` prints and the wasm stats
surface the live viewer reads (`wasm` module) share these types and
counts instead of each computing their own.

The ladder mirrors the utz crate docs' "How it works" stages: parsed
f64 coordinates → shared-arc topology → simplification → quantized +
serialized sections → compressed container.

License: MIT
