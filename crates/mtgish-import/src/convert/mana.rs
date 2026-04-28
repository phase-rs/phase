//! Mana conversion: mtgish `Vec<ManaSymbol>` → engine `ManaCost`.
//!
//! Phase 3a foundation. Engine encodes a paid cost as
//! `ManaCost::Cost { shards: Vec<ManaCostShard>, generic: u32 }` where
//! generic mana is collapsed into a single counter and each colored /
//! hybrid / phyrexian / two-generic-hybrid pip is a discrete `Shard`.
//! mtgish flattens the same data as a sequence of `ManaSymbol` enums,
//! one per pip, with `ManaCostGeneric(n)` carrying the generic count.

use engine::types::ability::{ManaContribution, ManaProduction, QuantityExpr};
use engine::types::mana::ManaColor;
use engine::types::{ManaCost, ManaCostShard};

use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{ManaProduce, ManaSymbol, ManaSymbolX};

/// Convert a non-X mana cost (`Vec<ManaSymbol>`) — used by keyword
/// payloads like Bestow, Cycling, Foretell, Madness.
pub fn convert(symbols: &[ManaSymbol]) -> ConvResult<ManaCost> {
    if symbols.is_empty() {
        return Ok(ManaCost::zero());
    }
    let mut shards = Vec::with_capacity(symbols.len());
    let mut generic: u32 = 0;
    for sym in symbols {
        match shard_for(sym)? {
            ShardOrGeneric::Shard(s) => shards.push(s),
            ShardOrGeneric::Generic(n) => generic = generic.saturating_add(n),
        }
    }
    Ok(ManaCost::Cost { shards, generic })
}

/// CR 601.2f: Convert a `CostReduction` (Vec<CostReductionSymbol>) to a
/// `ManaCost`. `CostReduceGeneric(n)` becomes generic; coloured-pip
/// reductions become coloured shards. Negative generic reductions strict-
/// fail (mtgish never emits them, but be explicit).
pub fn convert_reduction(
    symbols: &[crate::schema::types::CostReductionSymbol],
) -> ConvResult<ManaCost> {
    use crate::schema::types::CostReductionSymbol as R;
    if symbols.is_empty() {
        return Ok(ManaCost::zero());
    }
    let mut shards = Vec::with_capacity(symbols.len());
    let mut generic: u32 = 0;
    for sym in symbols {
        match sym {
            R::CostReduceGeneric(n) => generic = generic.saturating_add(non_neg_generic(*n)?),
            R::CostReduceW => shards.push(ManaCostShard::White),
            R::CostReduceU => shards.push(ManaCostShard::Blue),
            R::CostReduceB => shards.push(ManaCostShard::Black),
            R::CostReduceR => shards.push(ManaCostShard::Red),
            R::CostReduceG => shards.push(ManaCostShard::Green),
        }
    }
    Ok(ManaCost::Cost { shards, generic })
}

/// Convert an X-bearing cost (`Vec<ManaSymbolX>`) — used for the card's
/// own mana cost (CardManaCost). `X` becomes a `ManaCostShard::X` shard.
pub fn convert_x(symbols: &[ManaSymbolX]) -> ConvResult<ManaCost> {
    if symbols.is_empty() {
        return Ok(ManaCost::zero());
    }
    let mut shards = Vec::with_capacity(symbols.len());
    let mut generic: u32 = 0;
    for sym in symbols {
        match shard_for_x(sym)? {
            ShardOrGeneric::Shard(s) => shards.push(s),
            ShardOrGeneric::Generic(n) => generic = generic.saturating_add(n),
        }
    }
    Ok(ManaCost::Cost { shards, generic })
}

enum ShardOrGeneric {
    Shard(ManaCostShard),
    Generic(u32),
}

fn shard_for(sym: &ManaSymbol) -> ConvResult<ShardOrGeneric> {
    use ManaSymbol as M;
    Ok(match sym {
        M::ManaCostGeneric(n) => ShardOrGeneric::Generic(non_neg_generic(*n)?),
        M::ManaCostW => ShardOrGeneric::Shard(ManaCostShard::White),
        M::ManaCostU => ShardOrGeneric::Shard(ManaCostShard::Blue),
        M::ManaCostB => ShardOrGeneric::Shard(ManaCostShard::Black),
        M::ManaCostR => ShardOrGeneric::Shard(ManaCostShard::Red),
        M::ManaCostG => ShardOrGeneric::Shard(ManaCostShard::Green),
        M::ManaCostC => ShardOrGeneric::Shard(ManaCostShard::Colorless),
        M::ManaCostS => ShardOrGeneric::Shard(ManaCostShard::Snow),
        M::ManaCostWP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianWhite),
        M::ManaCostUP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianBlue),
        M::ManaCostBP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianBlack),
        M::ManaCostRP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianRed),
        M::ManaCostGP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianGreen),
        M::ManaCost2U => ShardOrGeneric::Shard(ManaCostShard::TwoBlue),
        M::ManaCost2B => ShardOrGeneric::Shard(ManaCostShard::TwoBlack),
        M::ManaCost2R => ShardOrGeneric::Shard(ManaCostShard::TwoRed),
        M::ManaCost2G => ShardOrGeneric::Shard(ManaCostShard::TwoGreen),
        M::ManaCostWU => ShardOrGeneric::Shard(ManaCostShard::WhiteBlue),
        M::ManaCostUB => ShardOrGeneric::Shard(ManaCostShard::BlueBlack),
        M::ManaCostBR => ShardOrGeneric::Shard(ManaCostShard::BlackRed),
        M::ManaCostRG => ShardOrGeneric::Shard(ManaCostShard::RedGreen),
        M::ManaCostGW => ShardOrGeneric::Shard(ManaCostShard::GreenWhite),
        M::ManaCostWB => ShardOrGeneric::Shard(ManaCostShard::WhiteBlack),
        M::ManaCostUR => ShardOrGeneric::Shard(ManaCostShard::BlueRed),
        M::ManaCostBG => ShardOrGeneric::Shard(ManaCostShard::BlackGreen),
        M::ManaCostRW => ShardOrGeneric::Shard(ManaCostShard::RedWhite),
        M::ManaCostGU => ShardOrGeneric::Shard(ManaCostShard::GreenBlue),
    })
}

fn shard_for_x(sym: &ManaSymbolX) -> ConvResult<ShardOrGeneric> {
    use ManaSymbolX as M;
    Ok(match sym {
        M::ManaCostGeneric(n) => ShardOrGeneric::Generic(non_neg_generic(*n)?),
        M::ManaCostW => ShardOrGeneric::Shard(ManaCostShard::White),
        M::ManaCostU => ShardOrGeneric::Shard(ManaCostShard::Blue),
        M::ManaCostB => ShardOrGeneric::Shard(ManaCostShard::Black),
        M::ManaCostR => ShardOrGeneric::Shard(ManaCostShard::Red),
        M::ManaCostG => ShardOrGeneric::Shard(ManaCostShard::Green),
        M::ManaCostC => ShardOrGeneric::Shard(ManaCostShard::Colorless),
        M::ManaCostS => ShardOrGeneric::Shard(ManaCostShard::Snow),
        M::ManaCostWP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianWhite),
        M::ManaCostUP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianBlue),
        M::ManaCostBP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianBlack),
        M::ManaCostRP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianRed),
        M::ManaCostGP => ShardOrGeneric::Shard(ManaCostShard::PhyrexianGreen),
        M::ManaCost2W => ShardOrGeneric::Shard(ManaCostShard::TwoWhite),
        M::ManaCost2U => ShardOrGeneric::Shard(ManaCostShard::TwoBlue),
        M::ManaCost2B => ShardOrGeneric::Shard(ManaCostShard::TwoBlack),
        M::ManaCost2R => ShardOrGeneric::Shard(ManaCostShard::TwoRed),
        M::ManaCost2G => ShardOrGeneric::Shard(ManaCostShard::TwoGreen),
        M::ManaCostWU => ShardOrGeneric::Shard(ManaCostShard::WhiteBlue),
        M::ManaCostUB => ShardOrGeneric::Shard(ManaCostShard::BlueBlack),
        M::ManaCostBR => ShardOrGeneric::Shard(ManaCostShard::BlackRed),
        M::ManaCostRG => ShardOrGeneric::Shard(ManaCostShard::RedGreen),
        M::ManaCostGW => ShardOrGeneric::Shard(ManaCostShard::GreenWhite),
        M::ManaCostWB => ShardOrGeneric::Shard(ManaCostShard::WhiteBlack),
        M::ManaCostUR => ShardOrGeneric::Shard(ManaCostShard::BlueRed),
        M::ManaCostBG => ShardOrGeneric::Shard(ManaCostShard::BlackGreen),
        M::ManaCostRW => ShardOrGeneric::Shard(ManaCostShard::RedWhite),
        M::ManaCostGU => ShardOrGeneric::Shard(ManaCostShard::GreenBlue),
        // Phyrexian hybrid pairs — not yet representable in the engine's
        // ManaCostShard. Surface as a missing-engine-primitive gap.
        M::ManaCostRWP | M::ManaCostRGP | M::ManaCostGWP | M::ManaCostGUP => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ManaCostShard",
                needed_variant: "PhyrexianHybrid (RW/P, RG/P, GW/P, GU/P)".into(),
            });
        }
        // Colorless hybrid pairs (CW/CU/CB/CR/CG) — same situation.
        M::ManaCostCW | M::ManaCostCU | M::ManaCostCB | M::ManaCostCR | M::ManaCostCG => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ManaCostShard",
                needed_variant: "ColorlessHybrid (C/W, C/U, C/B, C/R, C/G)".into(),
            });
        }
        M::ManaCostX => ShardOrGeneric::Shard(ManaCostShard::X),
    })
}

/// CR 605.1 + CR 106.1: Convert a mtgish `ManaProduce` (the right-hand side
/// of `Action::AddMana`) into an engine `ManaProduction`. Handles the common
/// shapes — single-color/colorless atoms, `And(...)` → fixed multi-color
/// sequence, `Or(...)` over single colors → `AnyOneColor` with the union as
/// the option set. Other dynamic shapes (color-of-permanent, opponent-land
/// colors as a typed query, etc.) strict-fail until a dedicated mapping lands.
pub fn convert_produce(p: &ManaProduce) -> ConvResult<ManaProduction> {
    if let Some(color) = single_color(p) {
        return Ok(ManaProduction::Fixed {
            colors: vec![color],
            contribution: ManaContribution::Base,
        });
    }
    Ok(match p {
        // CR 106.1b: Colorless mana atom.
        ManaProduce::ManaProduceC => ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        },
        // CR 106.1: A fixed multi-mana sequence ({W}{U}, {R}{R}, …).
        // `And` collapses to `Fixed` only when every leaf is a colored atom;
        // mixed colorless/colored sequences need `Mixed`.
        ManaProduce::And(parts) => fixed_or_mixed(parts)?,
        // CR 605.3b: Choice over single colors → AnyOneColor with that set.
        ManaProduce::Or(parts) => any_one_color_from_options(parts)?,
        // CR 106.7: "any color" is the WUBRG choice axis.
        ManaProduce::AnyManaColor => ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        },
        // CR 605.1a: Mana of a previously-chosen color (Utopia Sprawl etc.).
        ManaProduce::ManaOfAChosenColor | ManaProduce::ManaOfTheChosenColor => {
            ManaProduction::ChosenColor {
                count: QuantityExpr::Fixed { value: 1 },
                contribution: ManaContribution::Base,
            }
        }
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "ManaProduce/convert_produce",
                path: String::new(),
                detail: format!("unsupported ManaProduce: {}", produce_tag(other)),
            });
        }
    })
}

fn single_color(p: &ManaProduce) -> Option<ManaColor> {
    Some(match p {
        ManaProduce::ManaProduceW => ManaColor::White,
        ManaProduce::ManaProduceU => ManaColor::Blue,
        ManaProduce::ManaProduceB => ManaColor::Black,
        ManaProduce::ManaProduceR => ManaColor::Red,
        ManaProduce::ManaProduceG => ManaColor::Green,
        _ => return None,
    })
}

fn fixed_or_mixed(parts: &[ManaProduce]) -> ConvResult<ManaProduction> {
    let mut colors = Vec::with_capacity(parts.len());
    let mut colorless: u32 = 0;
    for part in parts {
        if let Some(c) = single_color(part) {
            colors.push(c);
        } else if matches!(part, ManaProduce::ManaProduceC) {
            colorless = colorless.saturating_add(1);
        } else {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "ManaProduce/And",
                path: String::new(),
                detail: format!("non-atomic And leaf: {}", produce_tag(part)),
            });
        }
    }
    Ok(if colorless == 0 {
        ManaProduction::Fixed {
            colors,
            contribution: ManaContribution::Base,
        }
    } else {
        // CR 106.1: Mixed colorless+colored sequence (Karoo / Azorius Chancery).
        ManaProduction::Mixed {
            colorless_count: colorless,
            colors,
        }
    })
}

/// CR 605.1 + CR 106.1b: Dynamic-count mana production —
/// `Action::AddManaRepeated(ManaProduce, GameNumber)` lowers a single-color
/// or "any color" produce repeated `count` times. Mirrors the native parser's
/// "if colors.len() == 1" pattern (`oracle_effect/mana.rs`) which encodes
/// "N copies of {color}" as `AnyOneColor` with a single-element option set —
/// the engine resolver degenerates "choose one of {color}" to that color, so
/// the count semantics flow through naturally.
pub fn convert_repeated_produce(
    p: &ManaProduce,
    count: QuantityExpr,
) -> ConvResult<ManaProduction> {
    if let Some(color) = single_color(p) {
        return Ok(ManaProduction::AnyOneColor {
            count,
            color_options: vec![color],
            contribution: ManaContribution::Base,
        });
    }
    Ok(match p {
        ManaProduce::ManaProduceC => ManaProduction::Colorless { count },
        ManaProduce::AnyManaColor => ManaProduction::AnyOneColor {
            count,
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        },
        ManaProduce::ManaOfAChosenColor | ManaProduce::ManaOfTheChosenColor => {
            ManaProduction::ChosenColor {
                count,
                contribution: ManaContribution::Base,
            }
        }
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "ManaProduce/AddManaRepeated",
                path: String::new(),
                detail: format!("non-trivial repeated produce: {}", produce_tag(other)),
            });
        }
    })
}

fn any_one_color_from_options(parts: &[ManaProduce]) -> ConvResult<ManaProduction> {
    let mut color_options = Vec::with_capacity(parts.len());
    for part in parts {
        match single_color(part) {
            Some(c) => color_options.push(c),
            None => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "ManaProduce/Or",
                    path: String::new(),
                    detail: format!("non-color Or leaf: {}", produce_tag(part)),
                });
            }
        }
    }
    Ok(ManaProduction::AnyOneColor {
        count: QuantityExpr::Fixed { value: 1 },
        color_options,
        contribution: ManaContribution::Base,
    })
}

fn produce_tag(p: &ManaProduce) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| {
            v.get("_ManaProduce")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn non_neg_generic(n: i32) -> ConvResult<u32> {
    u32::try_from(n).map_err(|_| ConversionGap::MalformedIdiom {
        idiom: "Mana/generic_count",
        path: String::new(),
        detail: format!("expected non-negative generic mana, got {n}"),
    })
}
