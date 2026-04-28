//! Keyword Rule → engine `Keyword` mapping.
//!
//! Phase 4a: every unit-variant keyword Rule and every keyword carrying
//! only a literal numeric payload (`i32` or `GameNumber::Integer(n)`)
//! converts to the engine's `Keyword` enum. Keywords carrying a `Cost`
//! or `Permanents` filter still fail with `ConversionGap` — they unlock
//! once Phase 6 (cost) and Phase 3 (filter) primitives land.
//!
//! `★` Why split here: every Phase 4a keyword closes a card with a single
//! type-system lookup; there is no semantic translation. Cost/filter
//! keywords need real conversion logic, so they land with their phase.

use engine::types::ability::QuantityExpr;
use engine::types::keywords::{
    BuybackCost, CyclingCost, FlashbackCost, HexproofFilter, ProtectionTarget, WardCost,
};
use engine::types::mana::ManaColor;
use engine::types::Keyword;

use crate::convert::cost as cost_conv;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{
    CardType, Cards, Color, Cost, CreatureType, GameNumber, Protectable, ProtectableColor, Rule,
};

/// If `rule` is a keyword Rule we can translate today, return the engine
/// `Keyword`. Otherwise return `Ok(None)` so the dispatcher continues
/// trying other arms (or eventually records `UnknownVariant`).
pub fn try_convert(rule: &Rule, path: &str) -> ConvResult<Option<Keyword>> {
    let kw = match rule {
        // === Unit variants — direct 1:1 map ===
        Rule::Flying => Keyword::Flying,
        Rule::FirstStrike => Keyword::FirstStrike,
        Rule::DoubleStrike => Keyword::DoubleStrike,
        Rule::Trample => Keyword::Trample,
        Rule::TrampleOverPlaneswalkers => Keyword::TrampleOverPlaneswalkers,
        Rule::Deathtouch => Keyword::Deathtouch,
        Rule::Lifelink => Keyword::Lifelink,
        Rule::Vigilance => Keyword::Vigilance,
        Rule::Haste => Keyword::Haste,
        Rule::Reach => Keyword::Reach,
        Rule::Defender => Keyword::Defender,
        Rule::Menace => Keyword::Menace,
        Rule::Indestructible => Keyword::Indestructible,
        Rule::Hexproof => Keyword::Hexproof,
        Rule::Shroud => Keyword::Shroud,
        Rule::Flash => Keyword::Flash,
        Rule::Fear => Keyword::Fear,
        Rule::Intimidate => Keyword::Intimidate,
        Rule::Skulk => Keyword::Skulk,
        Rule::Shadow => Keyword::Shadow,
        Rule::Horsemanship => Keyword::Horsemanship,
        Rule::Wither => Keyword::Wither,
        Rule::Infect => Keyword::Infect,
        Rule::Prowess => Keyword::Prowess,
        Rule::Undying => Keyword::Undying,
        Rule::Persist => Keyword::Persist,
        Rule::Cascade => Keyword::Cascade,
        Rule::Exalted => Keyword::Exalted,
        Rule::Flanking => Keyword::Flanking,
        Rule::Evolve => Keyword::Evolve,
        Rule::Extort => Keyword::Extort,
        Rule::Exploit => Keyword::Exploit,
        Rule::Ascend => Keyword::Ascend,
        Rule::StartYourEngines => Keyword::StartYourEngines,
        Rule::Soulbond => Keyword::Soulbond,
        Rule::Convoke => Keyword::Convoke,
        Rule::Improvise => Keyword::Improvise,
        Rule::Phasing => Keyword::Phasing,
        Rule::BattleCry => Keyword::Battlecry,
        Rule::Unleash => Keyword::Unleash,
        Rule::Riot => Keyword::Riot,
        Rule::LivingWeapon => Keyword::LivingWeapon,
        Rule::JobSelect => Keyword::JobSelect,
        Rule::Banding => Keyword::Banding,
        Rule::Fuse => Keyword::Fuse,
        Rule::Gravestorm => Keyword::Gravestorm,
        Rule::Haunt => Keyword::Haunt,
        Rule::Ingest => Keyword::Ingest,
        Rule::Melee => Keyword::Melee,
        Rule::Mentor => Keyword::Mentor,
        Rule::Myriad => Keyword::Myriad,
        Rule::Provoke => Keyword::Provoke,
        Rule::Rebound => Keyword::Rebound,
        Rule::Retrace => Keyword::Retrace,
        Rule::SplitSecond => Keyword::SplitSecond,
        Rule::Storm => Keyword::Storm,
        Rule::Sunburst => Keyword::Sunburst,
        Rule::Training => Keyword::Training,
        Rule::Undaunted => Keyword::Undaunted,
        Rule::Vanishing => Keyword::Vanishing(0),
        Rule::Demonstrate => Keyword::Demonstrate,
        Rule::Decayed => Keyword::Decayed,
        Rule::Dethrone => Keyword::Dethrone,
        Rule::DoubleTeam => Keyword::DoubleTeam,
        Rule::LivingMetal => Keyword::LivingMetal,
        Rule::Bargain => Keyword::Bargain,
        Rule::Compleated => Keyword::Compleated,
        Rule::Conspire => Keyword::Conspire,
        Rule::Daybound => Keyword::Daybound,
        Rule::Nightbound => Keyword::Nightbound,
        Rule::Enlist => Keyword::Enlist,
        Rule::JumpStart => Keyword::JumpStart,
        Rule::Assist => Keyword::Assist,
        Rule::ReadAhead => Keyword::ReadAhead,
        Rule::Ravenous => Keyword::Ravenous,

        // === Numeric payload — i32 → u32 ===
        Rule::Annihilator(n) => Keyword::Annihilator(non_negative(*n)?),
        Rule::Amplify(n) => Keyword::Amplify(non_negative(*n)?),
        Rule::Afterlife(n) => Keyword::Afterlife(non_negative(*n)?),
        Rule::Afflict(n) => Keyword::Afflict(non_negative(*n)?),
        Rule::Crew(n) => Keyword::Crew(non_negative(*n)?),
        Rule::Fabricate(n) => Keyword::Fabricate(non_negative(*n)?),
        Rule::Fading(n) => Keyword::Fading(non_negative(*n)?),
        Rule::Graft(n) => Keyword::Graft(non_negative(*n)?),
        Rule::Hideaway(n) => Keyword::Hideaway(non_negative(*n)?),
        Rule::Rampage(n) => Keyword::Rampage(non_negative(*n)?),
        Rule::Renown(n) => Keyword::Renown(non_negative(*n)?),
        Rule::Toxic(n) => Keyword::Toxic(non_negative(*n)?),
        Rule::Tribute(n) => Keyword::Tribute(non_negative(*n)?),
        Rule::Absorb(n) => Keyword::Absorb(non_negative(*n)?),
        Rule::VanishingEnters(n) => Keyword::Vanishing(non_negative(*n)?),

        // === GameNumber payload — only `Integer(n)` resolves today ===
        Rule::Bushido(g) => Keyword::Bushido(int_or_gap(g, "Rule::Bushido", path)?),
        Rule::Bloodthirst(g) => Keyword::Bloodthirst(int_or_gap(g, "Rule::Bloodthirst", path)?),
        Rule::Casualty(g) => Keyword::Casualty(int_or_gap(g, "Rule::Casualty", path)?),
        Rule::Dredge(g) => Keyword::Dredge(int_or_gap(g, "Rule::Dredge", path)?),
        Rule::Modular(g) => Keyword::Modular(int_or_gap(g, "Rule::Modular", path)?),
        Rule::Mobilize(g) => Keyword::Mobilize(QuantityExpr::Fixed {
            value: int_or_gap(g, "Rule::Mobilize", path)? as i32,
        }),
        // CR 702.60a: Ripple N. The engine keyword is currently the fixed
        // Oracle corpus shape (Ripple 4), so strict-fail any non-4 payload
        // instead of dropping semantic data.
        Rule::Ripple(g) => {
            let count = int_or_gap(g, "Rule::Ripple", path)?;
            if count != 4 {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "Keyword::Ripple",
                    needed_variant: format!("parameterized Ripple {count}"),
                });
            }
            Keyword::Ripple
        }
        Rule::Saddle(g) => Keyword::Saddle(int_or_gap(g, "Rule::Saddle", path)?),
        Rule::Soulshift(g) => Keyword::Soulshift(int_or_gap(g, "Rule::Soulshift", path)?),
        Rule::Poisonous(n) => Keyword::Poisonous(non_negative(*n)?),

        // === Phase 4b: ManaCost-payload keywords (Cost::PayMana only) ===
        Rule::Bestow(c) => Keyword::Bestow(pure_mana(c, "Rule::Bestow", path)?),
        Rule::Blitz(c) => Keyword::Blitz(pure_mana(c, "Rule::Blitz", path)?),
        Rule::Dash(c) => Keyword::Dash(pure_mana(c, "Rule::Dash", path)?),
        Rule::Disturb(c) => Keyword::Disturb(pure_mana(c, "Rule::Disturb", path)?),
        Rule::Disguise(c) => Keyword::Disguise(pure_mana(c, "Rule::Disguise", path)?),
        Rule::Echo(c) => Keyword::Echo(pure_mana(c, "Rule::Echo", path)?),
        Rule::Embalm(c) => Keyword::Embalm(pure_mana(c, "Rule::Embalm", path)?),
        Rule::Emerge(c) => Keyword::Emerge(pure_mana(c, "Rule::Emerge", path)?),
        Rule::Encore(c) => Keyword::Encore(pure_mana(c, "Rule::Encore", path)?),
        Rule::Eternalize(c) => Keyword::Eternalize(pure_mana(c, "Rule::Eternalize", path)?),
        Rule::Evoke(c) => Keyword::Evoke(pure_mana(c, "Rule::Evoke", path)?),
        Rule::Fortify(c) => Keyword::Fortify(pure_mana(c, "Rule::Fortify", path)?),
        Rule::Foretell(c) => Keyword::Foretell(pure_mana(c, "Rule::Foretell", path)?),
        Rule::Harmonize(c) => Keyword::Harmonize(pure_mana(c, "Rule::Harmonize", path)?),
        Rule::Kicker(c) => Keyword::Kicker(pure_mana(c, "Rule::Kicker", path)?),
        Rule::Madness(c) => Keyword::Madness(pure_mana(c, "Rule::Madness", path)?),
        Rule::Megamorph(c) => Keyword::Megamorph(pure_mana(c, "Rule::Megamorph", path)?),
        Rule::Miracle(c) => Keyword::Miracle(pure_mana(c, "Rule::Miracle", path)?),
        Rule::Morph(c) => Keyword::Morph(pure_mana(c, "Rule::Morph", path)?),
        Rule::Multikicker(c) => Keyword::Kicker(pure_mana(c, "Rule::Multikicker", path)?),
        Rule::Mutate(c) => Keyword::Mutate(pure_mana(c, "Rule::Mutate", path)?),
        Rule::Ninjutsu(c) => Keyword::Ninjutsu(pure_mana(c, "Rule::Ninjutsu", path)?),
        Rule::CommanderNinjutsu(c) => {
            Keyword::CommanderNinjutsu(pure_mana(c, "Rule::CommanderNinjutsu", path)?)
        }
        Rule::Offspring(c) => Keyword::Offspring(pure_mana(c, "Rule::Offspring", path)?),
        Rule::Outlast(c) => Keyword::Outlast(pure_mana(c, "Rule::Outlast", path)?),
        Rule::Plot(c) => Keyword::Plot(pure_mana(c, "Rule::Plot", path)?),
        Rule::Reconfigure(c) => Keyword::Reconfigure(pure_mana(c, "Rule::Reconfigure", path)?),
        Rule::Scavenge(c) => Keyword::Scavenge(pure_mana(c, "Rule::Scavenge", path)?),
        Rule::Spectacle(c) => Keyword::Spectacle(pure_mana(c, "Rule::Spectacle", path)?),
        Rule::Transmute(c) => Keyword::Transmute(pure_mana(c, "Rule::Transmute", path)?),
        Rule::Unearth(c) => Keyword::Unearth(pure_mana(c, "Rule::Unearth", path)?),
        Rule::Warp(c) => Keyword::Warp(pure_mana(c, "Rule::Warp", path)?),
        Rule::CumulativeUpkeep(c) => {
            // Engine stores the per-age cost as a string (the Oracle-text form).
            // Without the original Oracle text accessible here, render the mana
            // pips from the parsed cost.
            let mc = pure_mana(c, "Rule::CumulativeUpkeep", path)?;
            Keyword::CumulativeUpkeep(format!("{mc:?}"))
        }
        Rule::Surge(syms) => Keyword::Surge(crate::convert::mana::convert(syms)?),

        // ManaCost(...) — direct mana pip lists, not boxed in Cost::PayMana
        Rule::Prowl(syms) => Keyword::Prowl(crate::convert::mana::convert(syms)?),

        // CR 702.50: Delve — unit variant.
        Rule::Delve => Keyword::Delve,
        // CR 702.165: Backup N — i32 payload.
        Rule::Backup(n, _grants) => Keyword::Backup(non_negative(*n)?),
        // CR 702.190a: Sneak (Box<Cost>).
        Rule::Sneak(c) => Keyword::Sneak(pure_mana(c, "Rule::Sneak", path)?),
        // CR 702.87a: LevelUp (Box<Cost>, Vec<Level>) — drop level metadata for now,
        // engine's LevelUp keyword carries only the activation cost.
        Rule::LevelUp(c, _levels) => Keyword::LevelUp(pure_mana(c, "Rule::LevelUp", path)?),

        // Composite-cost wrappers: Mana variant only for now.
        Rule::Cycling(c) => {
            Keyword::Cycling(CyclingCost::Mana(pure_mana(c, "Rule::Cycling", path)?))
        }
        Rule::Flashback(c) => {
            Keyword::Flashback(FlashbackCost::Mana(pure_mana(c, "Rule::Flashback", path)?))
        }
        Rule::Buyback(c) => {
            Keyword::Buyback(BuybackCost::Mana(pure_mana(c, "Rule::Buyback", path)?))
        }

        // Suspend(GameNumber, Cost) — both must resolve.
        Rule::Suspend(g, c) => Keyword::Suspend {
            count: int_or_gap(g, "Rule::Suspend.count", path)?,
            cost: pure_mana(c, "Rule::Suspend.cost", path)?,
        },

        // === Phase 4b: Filter-payload keywords (narrow Permanents shapes) ===
        Rule::EnchantPermanent(perm) => Keyword::Enchant(crate::convert::filter::convert(perm)?),
        // CR 702.5a: Enchant player — auras attached to a player.
        Rule::EnchantPlayer(_players) => {
            Keyword::Enchant(engine::types::ability::TargetFilter::Player)
        }
        Rule::Landwalk(perm) => Keyword::Landwalk(crate::convert::filter::extract_subtype(perm)?),
        Rule::Champion(perm) => Keyword::Champion(crate::convert::filter::extract_subtype(perm)?),
        // Equip drops the filter — engine encodes "creature" implicitly in
        // attach semantics. Only the cost half is needed.
        Rule::Equip(_perm, c) => Keyword::Equip(pure_mana(c, "Rule::Equip", path)?),
        Rule::Affinity(perm) => {
            // Affinity needs a TypedFilter, not a TargetFilter. Extract from
            // the converted form when it's a simple typed filter.
            match crate::convert::filter::convert(perm)? {
                engine::types::ability::TargetFilter::Typed(tf) => Keyword::Affinity(tf),
                other => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Rule::Affinity",
                        path: path.to_string(),
                        detail: format!("expected typed filter, got: {other:?}"),
                    });
                }
            }
        }

        // CR 702.21: Ward — extra cost to target this permanent.
        Rule::Ward(c) => Keyword::Ward(convert_ward_cost(c, path)?),

        // CR 702.16: Protection from <quality>.
        Rule::Protection(p) => Keyword::Protection(convert_protectable(p, path)?),
        // CR 702.11h: Hexproof from <quality> — the same Protectable
        // dispatch maps to `HexproofFilter` (Color / CardType / Quality).
        Rule::HexproofFrom(p) => Keyword::HexproofFrom(convert_hexproof_filter(p, path)?),

        // CR 702.157: Squad N — additional-cost-driven token-doubling.
        // Engine carries only the mana cost.
        Rule::Squad(c) => Keyword::Squad(pure_mana(c, "Rule::Squad", path)?),
        // CR 702.184a: Station — fixed activated ability, no parameter.
        Rule::Station => Keyword::Station,
        // Spider-Man crossover: WebSlinging — alt-cast cost. mtgish stores
        // the cost as `Box<Cost>`; engine takes only the mana cost.
        Rule::WebSlinging(c) => Keyword::WebSlinging(pure_mana(c, "Rule::WebSlinging", path)?),

        // CR 702.47a: Splice onto [quality] [cost]. The engine keyword
        // stores the quality string only (matching the native parser);
        // splice-cost payment is not represented in the current keyword type.
        Rule::SpliceOnto(spells, _cost) => Keyword::Splice(splice_quality(spells, path)?),

        // CR 702.56a: Replicate {cost} — additional-cost-on-cast copy
        // mechanic. Engine carries only the mana cost.
        Rule::Replicate(c) => Keyword::Replicate(pure_mana(c, "Rule::Replicate", path)?),
        // CR 702.163a: For Mirrodin! — Equipment ETB triggered ability
        // (Rebel-token + auto-attach). Bare keyword.
        Rule::ForMirrodin => Keyword::ForMirrodin,
        // CR 702.162a: More Than Meets the Eye {cost} — Transformers
        // crossover alternate cast cost. Mana-only.
        Rule::MoreThanMeetsTheEye(c) => {
            Keyword::MoreThanMeetsTheEye(pure_mana(c, "Rule::MoreThanMeetsTheEye", path)?)
        }

        // CR 702.29: Typecycling — "[subtype]cycling [cost]". The Cards
        // filter carries the subtype, the Cost is the activation cost.
        Rule::TypeCycling(cards, cost) => {
            let subtype = extract_typecycling_subtype(cards, path)?;
            Keyword::Typecycling {
                cost: pure_mana(cost, "Rule::TypeCycling", path)?,
                subtype,
            }
        }
        // Firebending N — Avatar crossover. mtgish stores N as a GameNumber.
        Rule::Firebending(g) => Keyword::Firebending(int_or_gap(g, "Rule::Firebending", path)?),
        // CR 702.81: Devour N — engine encodes only N (the "creatures you
        // sacrifice" filter is implicit).
        Rule::Devour(_perm, g) => Keyword::Devour(int_or_gap(g, "Rule::Devour", path)?),
        // CR 702.163: Prototype — alt-cost cast for a smaller body. The
        // engine keyword carries only the alt mana cost; the alt-PT is
        // synthesized separately. CardManaCost is a Vec<ManaSymbolX>.
        // CR 702.176a: Impending N—{cost} — alternative cost. "You may
        // choose to pay [cost] rather than pay this spell's mana cost"
        // + "If you chose to pay this permanent's impending cost, it
        // enters with N time counters on it" + "As long as this
        // permanent's impending cost was paid and it has a time counter
        // on it, it's not a creature" + "At the beginning of your end
        // step, if this permanent's impending cost was paid and it has
        // a time counter on it, remove a time counter from it." Engine
        // stores only the mana cost; the N (time-counter count) is
        // dropped — the engine derives it from a separate metadata
        // pipeline.
        Rule::Impending(_n, c) => Keyword::Impending(pure_mana(c, "Rule::Impending", path)?),

        // CR 702.173a: Freerunning {cost} — alternative cost. Mana-only.
        Rule::Freerunning(c) => Keyword::Freerunning(pure_mana(c, "Rule::Freerunning", path)?),
        // CR 702.191a: Increment — bare triggered keyword (no payload).
        Rule::Increment => Keyword::Increment,
        // CR ???: Specialize {cost} — not in CR text. Engine carries
        // only the activation mana cost; the activation timing modifier
        // (`SpecializeWithModifiers`) and the from-graveyard variant
        // (`SpecializeFromGraveyard`) collapse to the same engine
        // keyword (the modifier and zone hint are dropped, mirroring
        // how `LevelUp` drops its `Vec<Level>` payload).
        Rule::Specialize(c) => Keyword::Specialize(pure_mana(c, "Rule::Specialize", path)?),
        Rule::SpecializeFromGraveyard(c) => {
            Keyword::Specialize(pure_mana(c, "Rule::SpecializeFromGraveyard", path)?)
        }
        Rule::SpecializeWithModifiers(c, _modifier) => {
            Keyword::Specialize(pure_mana(c, "Rule::SpecializeWithModifiers", path)?)
        }

        // CR 702.167a: Craft with [materials] [cost]. The engine keyword
        // carries only the activation mana cost, matching the native parser;
        // material requirements are not represented in `Keyword::Craft`.
        Rule::CraftWithACraftable(_, cost)
        | Rule::CraftWithCraftables(_, cost)
        | Rule::CraftWithANumberOfCraftables(_, _, cost)
        | Rule::CraftWithANumberOfGroupCraftables(_, _, _, cost) => {
            Keyword::Craft(crate::convert::mana::convert(cost)?)
        }

        // CR 702.48a: "[Quality] offering" — additional-cost-on-cast
        // sacrificing a permanent of the named quality. The schema's
        // `Cards` filter carries the quality (a creature subtype like
        // "Spirit" or "Dragon"); reuse `extract_typecycling_subtype`
        // since the shape is the same (a singular subtype filter).
        Rule::Offering(cards) => {
            // mtgish `Cards` here is e.g. `IsCreatureType(Spirit)`; reuse
            // `extract_offering_quality` to pull the canonical
            // capitalized subtype name (matching the engine convention
            // used by `Keyword::Champion`).
            let quality = extract_offering_quality(cards, path)?;
            Keyword::Offering(quality)
        }
        // CR 702.89b: "Umbra armor" — Oracle update of the legacy
        // "Totem armor" keyword. Engine retains the legacy variant name
        // (`Keyword::TotemArmor`) per the documented Oracle erratum.
        Rule::UmbraArmor => Keyword::TotemArmor,

        Rule::Prototype { mana_cost, .. } => {
            Keyword::Prototype(crate::convert::mana::convert_x(mana_cost)?)
        }

        // CR 702.138a: Escape — alternative casting cost from graveyard.
        // mtgish encodes the cost as `Cost::And([PayMana, ExileNumberGraveyardCards(N, ...)])`;
        // the engine's `Keyword::Escape { cost, exile_count }` carries the
        // mana payment and the literal exile count side-by-side.
        Rule::Escape(c) => extract_escape(c, path)?,

        // CR 702.106: Hidden Agenda — Conspiracy variant; deck-construction
        // and pre-game reveal mechanic. The engine has no slot for it today.
        Rule::HiddenAgenda => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Keyword",
                needed_variant: "HiddenAgenda (CR 702.106)".into(),
            });
        }
        // CR 702.77: Reinforce N—{cost} — discard this card to put N +1/+1
        // counters on a target creature. Engine has no Reinforce keyword;
        // strict-fail until added.
        Rule::Reinforce(_, _) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "Keyword",
                needed_variant: "Reinforce (CR 702.77)".into(),
            });
        }

        // Anything else: not a keyword we handle yet (could still be a
        // keyword Rule needing Phase 3/6/7 primitives, or a non-keyword Rule).
        _ => return Ok(None),
    };
    Ok(Some(kw))
}

/// Resolve a `Box<Cost>` keyword payload to a pure `ManaCost`. If the
/// cost is anything other than `Cost::PayMana(...)` (e.g., Cycling with
/// "Pay 2 life", Buyback with "Sacrifice a land"), we surface a
/// MalformedIdiom gap — the NonMana variant of `CyclingCost`/`BuybackCost`/
/// `FlashbackCost` will land with Phase 7 (full cost converter).
fn pure_mana(cost: &Cost, idiom: &'static str, path: &str) -> ConvResult<engine::types::ManaCost> {
    match cost_conv::as_pure_mana(cost)? {
        Some(mc) => Ok(mc),
        None => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: path.to_string(),
            detail: "non-mana cost — needs Phase 7 cost converter".into(),
        }),
    }
}

/// Bridge `i32` → `u32` for keyword payloads. Negative payloads are a
/// schema bug or a misclassified rule; surface as a malformed-idiom gap
/// so the report flags it.
fn non_negative(n: i32) -> ConvResult<u32> {
    u32::try_from(n).map_err(|_| ConversionGap::MalformedIdiom {
        idiom: "Keyword/non_negative",
        path: String::new(),
        detail: format!("expected non-negative count, got {n}"),
    })
}

/// Resolve a `GameNumber` to a literal `u32` for keyword payloads that
/// the engine encodes as compile-time constants. Dynamic GameNumbers
/// (counts that depend on game state) cannot collapse here; they fall
/// through to a gap until the engine grows a dynamic-payload keyword
/// variant.
/// CR 702.21a: Translate a Ward cost. Currently maps mana, life, and
/// discard; sacrifice/compound ward costs land later.
fn convert_ward_cost(c: &Cost, path: &str) -> ConvResult<WardCost> {
    Ok(match c {
        Cost::PayMana(symbols) => WardCost::Mana(crate::convert::mana::convert(symbols)?),
        Cost::PayLife(g) => WardCost::PayLife(int_or_gap(g, "Rule::Ward.life", path)? as i32),
        Cost::DiscardACard => WardCost::DiscardCard,
        _ => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Rule::Ward",
                path: path.to_string(),
                detail: format!("unsupported Ward cost: {c:?}"),
            });
        }
    })
}

/// CR 702.16: Translate a Protectable into engine `ProtectionTarget`.
/// CR 702.11h: Map `Protectable` to engine `HexproofFilter`. HexproofFilter
/// has a narrower codomain than ProtectionTarget (no Multicolored /
/// Everything / ChosenColor) — those shapes strict-fail. CardType (core
/// types and creature subtypes) flows through identically to Protection.
fn convert_hexproof_filter(p: &Protectable, path: &str) -> ConvResult<HexproofFilter> {
    match p {
        Protectable::FromColor(pc) => match pc {
            ProtectableColor::Colors(colors) if colors.len() == 1 => match map_color(&colors[0]) {
                Some(mc) => Ok(HexproofFilter::Color(mc)),
                None => Err(ConversionGap::MalformedIdiom {
                    idiom: "Rule::HexproofFrom.color",
                    path: path.to_string(),
                    detail: format!("non-mana color: {:?}", colors[0]),
                }),
            },
            // CR 702.11d: "hexproof from monocolored" / "from multicolored"
            // collapse to the engine `Quality` slot, matching how
            // `parse_hexproof_filter` does it (keywords.rs:1536).
            ProtectableColor::Multicolored => Ok(HexproofFilter::Quality("multicolored".into())),
            ProtectableColor::Monocolored => Ok(HexproofFilter::Quality("monocolored".into())),
            other => Err(ConversionGap::MalformedIdiom {
                idiom: "Rule::HexproofFrom.color",
                path: path.to_string(),
                detail: format!("unsupported ProtectableColor: {other:?}"),
            }),
        },
        // CR 702.11d: "hexproof from <type>" — the engine resolves these
        // through `HexproofFilter::CardType` against lowercase plural type
        // strings (matches the native parser at keywords.rs:1537 and the
        // runtime handler `source_matches_card_type`).
        Protectable::FromTypes(cards) => {
            let name = card_type_string(cards, "Rule::HexproofFrom", path)?;
            Ok(HexproofFilter::CardType(name))
        }
        other => Err(ConversionGap::MalformedIdiom {
            idiom: "Rule::HexproofFrom",
            path: path.to_string(),
            detail: format!("unsupported Protectable: {other:?}"),
        }),
    }
}

fn convert_protectable(p: &Protectable, path: &str) -> ConvResult<ProtectionTarget> {
    Ok(match p {
        // CR 702.16j: "protection from everything".
        Protectable::FromEverything => ProtectionTarget::Everything,
        Protectable::FromColor(pc) => match pc {
            ProtectableColor::Colors(colors) if colors.len() == 1 => match map_color(&colors[0]) {
                Some(mc) => ProtectionTarget::Color(mc),
                None => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "Rule::Protection.color",
                        path: path.to_string(),
                        detail: format!("non-mana color: {:?}", colors[0]),
                    });
                }
            },
            ProtectableColor::Multicolored => ProtectionTarget::Multicolored,
            // CR 702.16: "protection from monocolored" — engine encodes via
            // Quality (matches `source_matches_quality` in
            // game/keywords.rs:189).
            ProtectableColor::Monocolored => ProtectionTarget::Quality("monocolored".into()),
            // CR 702.16: "protection from the chosen color".
            ProtectableColor::TheChosenColor => ProtectionTarget::ChosenColor,
            // Multi-color list ("protection from black and from red") is
            // semantically two separate Protection keywords (the native
            // parser splits via `expand_protection_parts`). We can only
            // return one Keyword here, so strict-fail until the rule-level
            // dispatcher learns to fan out keyword-bearing rules.
            ProtectableColor::Colors(_) => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "Keyword::Protection",
                    needed_variant: "multi-color list (split into N keywords at rule level)".into(),
                });
            }
            // "Protection from each color in {commander}'s color identity"
            // — engine has no slot for commander-color-identity protection.
            ProtectableColor::NotAColorInCommanderColorIdentity => {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ProtectionTarget",
                    needed_variant: "NotAColorInCommanderColorIdentity".into(),
                });
            }
            _ => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Rule::Protection.color",
                    path: path.to_string(),
                    detail: format!("unsupported ProtectableColor: {pc:?}"),
                });
            }
        },
        // CR 702.16: "protection from <type>" — encoded as
        // `ProtectionTarget::CardType` (lowercase plural to match
        // `source_matches_card_type` in game/keywords.rs:171). Both core
        // types ("creatures", "artifacts") and creature subtypes
        // ("dragons", "cats") use this slot — the native parser does the
        // same at keywords.rs:1557.
        Protectable::FromTypes(cards) => {
            let name = card_type_string(cards, "Rule::Protection", path)?;
            ProtectionTarget::CardType(name)
        }
        _ => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Rule::Protection",
                path: path.to_string(),
                detail: format!("unsupported Protectable: {p:?}"),
            });
        }
    })
}

/// Translate the `Cards` filter that `Protectable::FromTypes` carries into
/// the lowercase plural type-string the engine's `source_matches_card_type`
/// (game/keywords.rs:171) expects. Single core type (`IsCardtype`) and
/// single creature subtype (`IsCreatureType`) are supported. Compound
/// shapes (`Or`, `And`) would require the rule-level dispatcher to fan a
/// single keyword Rule into N engine keywords; until that lands they
/// strict-fail with `EnginePrerequisiteMissing`.
fn card_type_string(cards: &Cards, idiom: &'static str, path: &str) -> ConvResult<String> {
    match cards {
        Cards::IsCardtype(ct) => Ok(card_type_plural(ct).to_string()),
        Cards::IsCreatureType(ct) => Ok(creature_type_plural(ct)),
        Cards::Or(_) => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "Keyword::Protection / Keyword::HexproofFrom",
            needed_variant: "Or-list of types (split into N keywords at rule level)".into(),
        }),
        other => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: path.to_string(),
            detail: format!("unsupported Cards filter for FromTypes: {other:?}"),
        }),
    }
}

/// Map a core `CardType` to the lowercase plural Oracle string the engine
/// expects ("creatures", "artifacts", ...). Mirrors the casing handled in
/// `source_matches_card_type` (game/keywords.rs:175-184).
fn card_type_plural(ct: &CardType) -> &'static str {
    match ct {
        CardType::Artifact => "artifacts",
        CardType::Battle => "battles",
        CardType::Conspiracy => "conspiracies",
        CardType::Creature => "creatures",
        CardType::Dungeon => "dungeons",
        CardType::Enchantment => "enchantments",
        CardType::Instant => "instants",
        CardType::Kindred => "kindred",
        CardType::Land => "lands",
        CardType::Phenomenon => "phenomena",
        CardType::Plane => "planes",
        CardType::Planeswalker => "planeswalkers",
        CardType::Scheme => "schemes",
        CardType::Sorcery => "sorceries",
        CardType::Vanguard => "vanguards",
    }
}

/// Lowercase plural of a `CreatureType` for use as the engine's
/// `ProtectionTarget::CardType` payload (e.g. `Dragon → "dragons"`,
/// `Wolf → "wolves"`). Falls back to a naive ASCII-lowercase + "s"
/// rule for the long tail; the engine's runtime handler is the consumer
/// of these strings, and there's no creature-subtype handler today, so
/// the exact spelling only needs to round-trip the future handler.
fn creature_type_plural(ct: &CreatureType) -> String {
    let raw = format!("{ct:?}");
    let lower = raw.to_ascii_lowercase();
    pluralise(&lower)
}

fn pluralise(s: &str) -> String {
    // Rough English plural rules sufficient for creature subtypes:
    //  - "wolf" → "wolves", "elf" → "elves"
    //  - "fungus" → "fungi" (handled by hard list below)
    //  - "fox" / "fish" / "bush" → "es" suffix
    //  - "berry" / "fairy" → "ies"
    //  - default: append "s".
    match s {
        "fungus" => "fungi".to_string(),
        "octopus" => "octopuses".to_string(),
        "djinn" => "djinn".to_string(),
        "fish" => "fish".to_string(),
        "dwarf" => "dwarves".to_string(),
        _ => {
            if let Some(stem) = s.strip_suffix('f') {
                return format!("{stem}ves");
            }
            if let Some(stem) = s.strip_suffix("fe") {
                return format!("{stem}ves");
            }
            if let Some(stem) = s.strip_suffix('y') {
                if !s
                    .chars()
                    .nth(s.len().saturating_sub(2))
                    .is_some_and(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
                {
                    return format!("{stem}ies");
                }
            }
            if s.ends_with('s')
                || s.ends_with('x')
                || s.ends_with("ch")
                || s.ends_with("sh")
                || s.ends_with('z')
            {
                return format!("{s}es");
            }
            format!("{s}s")
        }
    }
}

fn map_color(c: &Color) -> Option<ManaColor> {
    Some(match c {
        Color::White => ManaColor::White,
        Color::Blue => ManaColor::Blue,
        Color::Black => ManaColor::Black,
        Color::Red => ManaColor::Red,
        Color::Green => ManaColor::Green,
        _ => return None,
    })
}

/// CR 702.29: Pull the subtype name out of the `Cards` filter that
/// accompanies a `Rule::TypeCycling`. Recognises bare type filters
/// (`IsLandType(Plains)`, `IsCreatureType(Wizard)`) and the canonical
/// "basic land" composite (`And([IsSupertype(Basic), IsCardtype(Land)])`)
/// which becomes the engine's "basic" subtype label. Anything else
/// strict-fails so the report tracks unsupported cycling shapes.
fn extract_typecycling_subtype(
    cards: &crate::schema::types::Cards,
    path: &str,
) -> ConvResult<String> {
    use crate::schema::types::{CardType, Cards, SuperType};
    match cards {
        Cards::IsLandType(lt) => Ok(land_type_name(lt)),
        Cards::IsCreatureType(ct) => Ok(creature_type_name_for_kw(ct)),
        // CR 205.4a: "basic land" — Supertype Basic + Cardtype Land.
        Cards::And(parts) => {
            let has_basic = parts
                .iter()
                .any(|p| matches!(p, Cards::IsSupertype(SuperType::Basic)));
            let has_land = parts
                .iter()
                .any(|p| matches!(p, Cards::IsCardtype(CardType::Land)));
            if has_basic && has_land {
                Ok("basic".to_string())
            } else {
                Err(ConversionGap::MalformedIdiom {
                    idiom: "Rule::TypeCycling/Cards::And",
                    path: path.to_string(),
                    detail: "expected basic-land composite (Supertype::Basic + Cardtype::Land)"
                        .into(),
                })
            }
        }
        _ => Err(ConversionGap::MalformedIdiom {
            idiom: "Rule::TypeCycling/cards_shape",
            path: path.to_string(),
            detail: "unsupported Cards filter for TypeCycling".into(),
        }),
    }
}

fn land_type_name(lt: &crate::schema::types::LandType) -> String {
    serde_json::to_value(lt)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_lowercase()))
        .unwrap_or_else(|| format!("{lt:?}"))
}

fn creature_type_name_for_kw(ct: &crate::schema::types::CreatureType) -> String {
    serde_json::to_value(ct)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_lowercase()))
        .unwrap_or_else(|| format!("{ct:?}"))
}

/// CR 702.48a: Extract the canonical capitalized subtype name from an
/// Offering rule's `Cards` filter. mtgish encodes "[quality] offering"
/// as `Cards::IsCreatureType(<creature subtype>)`; the engine convention
/// (matching `Keyword::Champion`) is the capitalized subtype string.
fn extract_offering_quality(cards: &crate::schema::types::Cards, path: &str) -> ConvResult<String> {
    use crate::schema::types::Cards as C;
    match cards {
        C::IsCreatureType(ct) => Ok(crate::convert::filter::creature_type_name(ct)),
        other => Err(ConversionGap::MalformedIdiom {
            idiom: "Rule::Offering/cards_shape",
            path: path.to_string(),
            detail: format!(
                "expected Cards::IsCreatureType, got {}",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v.get("_Cards").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_else(|| "<unknown>".into())
            ),
        }),
    }
}

fn splice_quality(spells: &crate::schema::types::Spells, path: &str) -> ConvResult<String> {
    use crate::schema::types::{SpellType, Spells};
    match spells {
        Spells::IsSpellType(SpellType::Arcane) => Ok("Arcane".to_string()),
        Spells::Or(parts) if is_instant_or_sorcery(parts) => Ok("Instant or Sorcery".to_string()),
        other => Err(ConversionGap::MalformedIdiom {
            idiom: "Rule::SpliceOnto/spells_shape",
            path: path.to_string(),
            detail: format!("unsupported Spells filter for splice: {other:?}"),
        }),
    }
}

fn is_instant_or_sorcery(parts: &[crate::schema::types::Spells]) -> bool {
    use crate::schema::types::{CardType, Spells};
    parts.len() == 2
        && parts
            .iter()
            .any(|part| matches!(part, Spells::IsCardtype(CardType::Instant)))
        && parts
            .iter()
            .any(|part| matches!(part, Spells::IsCardtype(CardType::Sorcery)))
}

/// CR 702.138a: Escape's mtgish payload is `Cost::And([PayMana, ExileNumberGraveyardCards(N, ...)])`.
/// Pull out the mana cost and the literal exile count; anything else is a
/// gap (variable counts, alternate exile filters, sacrifice-as-additional,
/// etc.).
fn extract_escape(cost: &Cost, path: &str) -> ConvResult<Keyword> {
    let parts = match cost {
        Cost::And(parts) => parts.as_slice(),
        _ => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Rule::Escape",
                path: path.to_string(),
                detail: format!("expected Cost::And, got {cost:?}"),
            });
        }
    };
    let mut mana_cost: Option<engine::types::ManaCost> = None;
    let mut exile_count: Option<u32> = None;
    for part in parts {
        match part {
            Cost::PayMana(syms) => mana_cost = Some(crate::convert::mana::convert(syms)?),
            Cost::ExileNumberGraveyardCards(n, _filter) => {
                exile_count = Some(int_or_gap(n, "Rule::Escape.exile_count", path)?);
            }
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Rule::Escape",
                    path: path.to_string(),
                    detail: format!("unsupported sub-cost: {other:?}"),
                });
            }
        }
    }
    let cost = mana_cost.ok_or_else(|| ConversionGap::MalformedIdiom {
        idiom: "Rule::Escape",
        path: path.to_string(),
        detail: "missing PayMana sub-cost".into(),
    })?;
    let exile_count = exile_count.ok_or_else(|| ConversionGap::MalformedIdiom {
        idiom: "Rule::Escape",
        path: path.to_string(),
        detail: "missing ExileNumberGraveyardCards sub-cost".into(),
    })?;
    Ok(Keyword::Escape { cost, exile_count })
}

fn int_or_gap(g: &GameNumber, idiom: &'static str, path: &str) -> ConvResult<u32> {
    match g {
        GameNumber::Integer(n) => non_negative(*n),
        other => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: path.to_string(),
            detail: format!("non-literal GameNumber: {other:?}"),
        }),
    }
}
