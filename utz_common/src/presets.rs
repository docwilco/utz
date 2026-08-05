//! The canonical preset recipe table. Every preset asset the workspace
//! ships is described here exactly once, as a [`Recipe`] constant; the
//! builder presets (`utz_build::Config`), the `gen-preset` command, and
//! the data crates' recipe guards all consume this table instead of
//! restating the numbers.
//!
//! A recipe stores each knob in the representation the builder takes
//! (`f64` tolerance and grid size, enum knobs), except the density
//! floor, which is stored in the header's fixed-point form because the
//! builder-side fraction derives from it exactly while the reverse
//! rounding is unavailable in `core`. The [`Provenance`] type bridges
//! the remaining gap to [`PayloadHeader`]: it owns the `f64` → `f32`
//! casts the encoder applies when stamping the header, so a guard can
//! compare `Provenance::from(&header)` against `Provenance::from(recipe)`
//! with no literals of its own.

use crate::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo};

/// One preset's complete build recipe: the name and every encoder knob
/// that determines the shipped asset's provenance stamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recipe {
    /// The preset name, e.g. `"tiny-static"`, as spelled in feature
    /// names, crate names (with `-` as `_`), and asset file names.
    pub name: &'static str,
    /// The dataset the asset is built from.
    pub dataset: Dataset,
    /// The simplification tolerance in meters, as the builder takes it
    /// (the header stores the `f32` cast).
    pub eps_m: f64,
    /// The coordinate quantization width.
    pub quant_bits: QuantBits,
    /// The grid cell size in degrees, as the builder takes it (the
    /// header stores the `f32` cast).
    pub grid_deg: f64,
    /// The simplification algorithm the encoder runs.
    pub simplify_algo: SimplifyAlgo,
    /// The geometry encoding.
    pub geom: GeomEncoding,
    /// The payload compression codec.
    pub codec: Codec,
    /// The population-density weight floor in fixed-point 1e-4, the
    /// header's representation; 0 means unweighted. The builder-side
    /// fraction is [`density_weight_floor()`](Recipe::density_weight_floor).
    pub density_weight_floor_e4: u16,
}

/// The `tiny` recipe: RDP ε=10 000 m with pop-density floor 0.001, i16,
/// a 2° grid, and gzip.
pub const TINY: Recipe = Recipe {
    name: "tiny",
    dataset: Dataset::Now,
    eps_m: 10_000.0,
    quant_bits: QuantBits::Bits16,
    grid_deg: 2.0,
    simplify_algo: SimplifyAlgo::Rdp,
    geom: GeomEncoding::VarintArcs,
    codec: Codec::Gzip,
    density_weight_floor_e4: 10,
};

/// The `tiny-static` recipe, which is [`TINY`] stored uncompressed so
/// the asset is readable in place with zero decode RAM.
pub const TINY_STATIC: Recipe = Recipe {
    name: "tiny-static",
    codec: Codec::Uncompressed,
    ..TINY
};

/// The `compact` recipe: RDP ε=1 000 m with pop-density floor 0.001,
/// i24, a 4/3° grid, and xz.
pub const COMPACT: Recipe = Recipe {
    name: "compact",
    eps_m: 1_000.0,
    quant_bits: QuantBits::Bits24,
    grid_deg: 4.0 / 3.0,
    codec: Codec::Xz,
    ..TINY
};

/// The `balanced` recipe: RDP ε=50 m with pop-density floor 0.020, i24,
/// a 2/3° grid, and brotli.
pub const BALANCED: Recipe = Recipe {
    name: "balanced",
    eps_m: 50.0,
    quant_bits: QuantBits::Bits24,
    grid_deg: 2.0 / 3.0,
    codec: Codec::Brotli,
    density_weight_floor_e4: 200,
    ..TINY
};

/// The `accurate` recipe: dataset `all` (the full Comprehensive zone
/// set; the other presets use `now`), RDP ε=10 m with pop-density floor
/// 0.10, i32, a 0.5° grid, and brotli.
pub const ACCURATE: Recipe = Recipe {
    name: "accurate",
    dataset: Dataset::All,
    eps_m: 10.0,
    quant_bits: QuantBits::Bits32,
    grid_deg: 0.5,
    codec: Codec::Brotli,
    density_weight_floor_e4: 1_000,
    ..TINY
};

/// Every preset recipe, in size order. Tools that need the preset name
/// list (CLI validation, generate-all loops) iterate this instead of
/// keeping their own copy.
pub static ALL: [Recipe; 5] = [TINY, TINY_STATIC, COMPACT, BALANCED, ACCURATE];

/// Looks a recipe up by its preset name; the result is `None` for a
/// name the table does not carry.
#[must_use]
pub fn by_name(name: &str) -> Option<&'static Recipe> {
    ALL.iter().find(|recipe| recipe.name == name)
}

impl Recipe {
    /// Returns the density floor as the fraction the builder takes,
    /// `None` when the recipe is unweighted. The division is exact:
    /// both operands are exactly representable and every 4-decimal
    /// fraction rounds to the same nearest `f64` its literal parses to,
    /// so the derived value is bit-identical to the fraction the asset
    /// was generated with.
    #[must_use]
    pub fn density_weight_floor(&self) -> Option<f64> {
        (self.density_weight_floor_e4 != 0)
            .then(|| f64::from(self.density_weight_floor_e4) / 10_000.0)
    }
}

/// The subset of header fields a recipe determines, in the header's
/// representation (`f32` tolerance and grid size, fixed-point floor).
/// Converting both a [`Recipe`] and a [`PayloadHeader`] into this type
/// gives a recipe guard an exact, literal-free equality check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Provenance {
    /// The dataset the asset was built from.
    pub dataset: Dataset,
    /// The simplification tolerance in meters, as stamped in the header.
    pub eps_m: f32,
    /// The coordinate quantization width.
    pub quant_bits: QuantBits,
    /// The grid cell size in degrees, as stamped in the header.
    pub grid_deg: f32,
    /// The simplification algorithm the encoder ran.
    pub simplify_algo: SimplifyAlgo,
    /// The geometry encoding.
    pub geom: GeomEncoding,
    /// The payload compression codec.
    pub codec: Codec,
    /// The density floor in fixed-point 1e-4; 0 means unweighted.
    pub density_weight_floor_e4: u16,
}

impl From<&PayloadHeader> for Provenance {
    fn from(header: &PayloadHeader) -> Provenance {
        Provenance {
            dataset: header.dataset,
            eps_m: header.eps_m,
            quant_bits: header.quant_bits,
            grid_deg: header.grid_deg,
            simplify_algo: header.simplify_algo,
            geom: header.geom,
            codec: header.codec,
            density_weight_floor_e4: header.density_weight_floor_e4,
        }
    }
}

impl From<&Recipe> for Provenance {
    fn from(recipe: &Recipe) -> Provenance {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the encoder stamps eps_m and grid_deg through this same f64-to-f32 cast, so the provenance stamp matches it bit-exactly"
        )]
        Provenance {
            dataset: recipe.dataset,
            eps_m: recipe.eps_m as f32,
            quant_bits: recipe.quant_bits,
            grid_deg: recipe.grid_deg as f32,
            simplify_algo: recipe.simplify_algo,
            geom: recipe.geom,
            codec: recipe.codec,
            density_weight_floor_e4: recipe.density_weight_floor_e4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{by_name, Provenance, Recipe, ACCURATE, ALL, BALANCED, COMPACT, TINY, TINY_STATIC};
    use crate::Codec;

    #[test]
    fn names_are_unique_and_by_name_round_trips() {
        for (index, recipe) in ALL.iter().enumerate() {
            assert_eq!(
                by_name(recipe.name).map(|found| found.name),
                Some(recipe.name)
            );
            assert!(
                ALL[..index].iter().all(|other| other.name != recipe.name),
                "duplicate preset name {:?}",
                recipe.name
            );
        }
        assert!(by_name("no-such-preset").is_none());
    }

    #[test]
    fn tiny_static_differs_from_tiny_only_in_codec() {
        let restored = Recipe {
            codec: Codec::Gzip,
            name: "tiny",
            ..TINY_STATIC
        };
        assert_eq!(restored, TINY);
        assert_eq!(TINY_STATIC.codec, Codec::Uncompressed);
    }

    #[test]
    fn density_floor_fractions_match_the_builder_literals_exactly() {
        assert_eq!(TINY.density_weight_floor(), Some(0.001));
        assert_eq!(COMPACT.density_weight_floor(), Some(0.001));
        assert_eq!(BALANCED.density_weight_floor(), Some(0.020));
        assert_eq!(ACCURATE.density_weight_floor(), Some(0.10));
        let unweighted = Recipe {
            density_weight_floor_e4: 0,
            ..TINY
        };
        assert_eq!(unweighted.density_weight_floor(), None);
    }

    #[test]
    fn provenance_casts_match_the_encoder_stamps() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the assertion reproduces the encoder's f64-to-f32 stamp on purpose"
        )]
        for recipe in &ALL {
            let provenance = Provenance::from(recipe);
            assert_eq!(provenance.eps_m.to_bits(), (recipe.eps_m as f32).to_bits());
            assert_eq!(
                provenance.grid_deg.to_bits(),
                (recipe.grid_deg as f32).to_bits()
            );
            assert_eq!(provenance.dataset, recipe.dataset);
            assert_eq!(provenance.codec, recipe.codec);
        }
    }
}
