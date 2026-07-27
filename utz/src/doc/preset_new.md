Load the preset selected by the (single) enabled preset feature.
Documented here always, but cfg'd out of real builds when zero or
several presets are in the tree — there, load explicitly with
`from_slice`/`from_static` on the statics in [`crate::data`] instead.
`tiny-static` is the zero-copy one (`from_static`, bare `core`); the
rest are compressed and load lazy (`from_slice`).
