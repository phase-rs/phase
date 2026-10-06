//! Runtime coverage for the LIFETIME of `ChosenAttribute::Color` on one source
//! (CR 607.2d + CR 400.7 + CR 608.2d + CR 608.2h + CR 611.3a).
//!
//! Two seams, one file, because they only make sense together:
//!
//! * **U4 — the chosen-colour storage split.** `apply_choice_attributes`
//!   (`game/effects/choose.rs`) used to REPLACE a source's prior
//!   `ChosenAttribute::Color` on every re-choice, so three rules concepts
//!   that read this list — CR 607.2d's linked read, CR 608.2d's "this
//!   resolution" read, and "the current answer" — all collapsed onto
//!   whichever single answer the `retain` left standing. Colours now
//!   ACCUMULATE, and `GameObject::chosen_color` (oldest), `choose::
//!   resolution_chosen_color` (this resolution) and `GameObject::
//!   current_chosen_color` (newest) each read the end of that list their
//!   rule entitles them to. CR 607.2d links "choose a [value]" to "the
//!   chosen [value]" per choice; CR 400.7 makes a recast spell a new object
//!   that nonetheless keeps the storage object's attributes, because
//!   `chosen_attributes` is cleared only by `reset_for_battlefield_entry`,
//!   which a spell never reaches.
//!
//! * **U1 — the resolution-time latch.** A resolution-generated continuous
//!   effect (`effects/effect.rs::snapshot_transient_modifications`) now
//!   latches its granting source's chosen colour into the `AddKeyword`
//!   payload ONCE, when the effect is applied (CR 608.2h), so
//!   TWO SIMULTANEOUSLY LIVE grants from the same source each keep their own
//!   colour instead of both re-reading whichever answer is CURRENT at layer-
//!   apply time. CR 611.3a scopes this deliberately: a printed STATIC
//!   ability's grant (Floating Shield's own "Enchanted creature has
//!   protection from the chosen color") is never latched — it stays live,
//!   read every layer evaluation from `game/layers.rs`.
//!
//! Floating Shield's linked as-enters/sacrifice pair is the card the storage
//! split (U4) would otherwise break: its "Sacrifice this Aura:" ability reads
//! the colour chosen by its LINKED as-enters replacement and must make no
//! independent choice of its own (the parser's `LinkedColorChoice` relation
//! suppresses that chooser). This file pins both directions.
//!
//! Built via the `/card-test` recipe: `GameScenario` +
//! `GameRunner::cast(..)/activate(..).resolve()` + `CastOutcome`/`Outcome` zone
//! deltas, on VERBATIM Oracle text. Every negative assertion is paired with a
//! positive reach-guard in the same test.
//!
//! REVERT DISCRIMINATORS:
//! * `knight_of_dawn_two_live_grants_keep_their_own_colors` /
//!   `armored_guardian_two_recipients_keep_their_own_colors` /
//!   `armored_guardian_grants_gate_aura_attachment_per_grant_color` — revert
//!   the resolution-time latch in `effects/effect.rs::snapshot_transient_modifications`
//!   and a later grant retroactively rewrites an earlier one's colour.
//! * `wash_out_recast_on_the_same_object_uses_its_own_color` /
//!   `wash_out_recast_choosing_an_absent_color_moves_nothing` /
//!   `knight_of_dawn_second_activation_uses_its_own_color` — revert the
//!   `ChoiceType::Color` `retain` deletion in `apply_choice_attributes`
//!   (charter Rule R-3's forbidden candidate) and a second choice replaces
//!   the first instead of accumulating behind it.
//! * `prismatic_strands_recast_on_the_same_object_prevents_its_own_color` —
//!   revert `prevent_damage.rs`'s `current_chosen_color()` read back to
//!   `chosen_color()` and the second shield wrongly filters the FIRST cast's
//!   colour.
//! * `floating_shield_sacrifice_grant_reads_the_as_enters_color` — revert the
//!   `LinkedColorChoice` relation and the activation raises a second colour
//!   prompt (and, with the storage split in place, the grant then reads that
//!   second answer).

use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{ChoiceType, ChosenAttribute, FilterProp, TargetFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Wash Out {3}{U} Sorcery — verbatim Oracle text (MTGJSON `AtomicCards.json`).
const WASH_OUT: &str = "Return all permanents of the color of your choice to their owners' hands.";

/// Knight of Dawn {1}{W}{W} Creature — Human Knight 2/2 — verbatim.
const KNIGHT_OF_DAWN: &str =
    "First strike\n{W}{W}: This creature gains protection from the color of your choice until end \
     of turn.";

/// Hall of Triumph {3} Legendary Artifact — verbatim.
const HALL_OF_TRIUMPH: &str =
    "As Hall of Triumph enters, choose a color.\nCreatures you control of the chosen color get \
     +1/+1.";

/// Floating Shield {2}{W} Enchantment — Aura — verbatim.
const FLOATING_SHIELD: &str =
    "Enchant creature\nAs this Aura enters, choose a color.\nEnchanted creature has protection \
     from the chosen color. This effect doesn't remove this Aura.\nSacrifice this Aura: Target \
     creature gains protection from the chosen color until end of turn.";

/// Armored Guardian {3}{W}{U} Creature — verbatim, reminder text INCLUDED
/// (`/card-test` requires verbatim text; MTGJSON's printed shroud reminder is
/// part of it).
const ARMORED_GUARDIAN: &str =
    "{1}{W}{W}: Target creature you control gains protection from the color of your choice until \
     end of turn.\n{1}{U}{U}: This creature gains shroud until end of turn. (It can't be the \
     target of spells or abilities.)";

/// Firebreathing {R} Enchantment — Aura — verbatim. A second copy built with a
/// `{U}` cost is the blue control Aura for T3 — its colour comes from its
/// PRINTED MANA COST (`printed_cards::derive_colors_from_mana_cost`), never a
/// hand-set `color` field.
const FIREBREATHING: &str =
    "Enchant creature\n{R}: Enchanted creature gets +1/+0 until end of turn.";

/// Prismatic Strands {2}{W} Instant — verbatim, reminder text INCLUDED.
const PRISMATIC_STRANDS: &str =
    "Prevent all damage that sources of the color of your choice would deal this turn.\nFlashback\
     —Tap an untapped white creature you control. (You may cast this card from your graveyard for \
     its flashback cost. Then exile it.)";

/// A one-colour mana cost, so `with_mana_cost` derives exactly that colour
/// (CR 202.2 / CR 105.2).
fn one_color(shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic: 1,
    }
}

/// `n` units of white mana with no producing source and no spend restrictions —
/// the plainest pool contents that can pay a printed activation cost. Generic
/// costs are paid from this pool too, so it also covers `{1}{W}{W}`-shaped
/// activations without a separate colourless helper.
fn white_mana(n: usize) -> Vec<engine::types::mana::ManaUnit> {
    vec![
        engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::White,
            ObjectId(0),
            false,
            vec![]
        );
        n
    ]
}

/// Whether an object carries protection from EXACTLY this colour.
///
/// Deliberately not `game::keywords::has_keyword`: that helper matches on the
/// `Keyword` discriminant alone and so answers `true` for protection from ANY
/// colour, which would make every assertion below vacuous. The layer applier
/// bakes `Protection(ChosenColor)` into `Protection(Color(c))` on the recipient
/// (CR 702.16 + CR 613.1), so the concrete colour is what these tests read.
fn has_protection_from(obj: &engine::game::game_object::GameObject, color: ManaColor) -> bool {
    obj.keywords
        .iter()
        .any(|keyword| keyword == &Keyword::Protection(ProtectionTarget::Color(color)))
}

/// Every `ChosenAttribute::Color` currently recorded on an object, in order
/// (oldest first). The whole point of U4's storage split is that this can now
/// hold more than one element, and that three different readers
/// (`chosen_color`, `resolution_chosen_color`, `current_chosen_color`) each
/// take a different end of it.
fn chosen_colors(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> Vec<ManaColor> {
    runner.state().objects[&id]
        .chosen_attributes
        .iter()
        .filter_map(|attribute| match attribute {
            ChosenAttribute::Color(color) => Some(*color),
            _ => None,
        })
        .collect()
}

/// Whether a resolved prevention shield's `damage_source_filter` carries a
/// concrete `FilterProp::HasColor` for `color` — the shape
/// `prevent_damage.rs::resolve_source_filter` produces once `IsChosenColor`
/// is resolved at shield-creation time (CR 608.2d, U4d).
fn shield_has_color(
    shield: &engine::types::ability::ReplacementDefinition,
    color: ManaColor,
) -> bool {
    match &shield.damage_source_filter {
        Some(TargetFilter::Typed(tf)) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::HasColor { color: c } if *c == color)),
        _ => false,
    }
}

/// Every `FilterProp::HasColor` colour recorded on a resolved prevention
/// shield's `damage_source_filter`, in property order — the discriminating
/// counterpart to `shield_has_color`. Unlike the plain boolean, a failed
/// `assert_eq!` against this prints the actual colour set alongside the
/// expected one instead of a bare `false`.
fn shield_colors(shield: &engine::types::ability::ReplacementDefinition) -> Vec<ManaColor> {
    match &shield.damage_source_filter {
        Some(TargetFilter::Typed(tf)) => tf
            .properties
            .iter()
            .filter_map(|p| match p {
                FilterProp::HasColor { color } => Some(*color),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// S-3 — drive a run parked on repeated `NamedChoice` windows to completion,
/// answering each one with `choice` and returning the number of windows it
/// actually answered.
///
/// `SpellCast::choose_option` / `AbilityActivation::choose_option` set a
/// SINGLE answer slot which `drive_resolution` CLONES at every `NamedChoice`
/// it re-enters — so a plain `.resolve()` run cannot COUNT prompts: an effect
/// that asked twice would be silently answered twice from that one slot and
/// still reach `Priority`. This drives the run by hand instead, answering
/// each open `NamedChoice` with a fresh `GameAction::ChooseOption` and passing
/// priority toward the next one, so the number of windows actually answered
/// is observable.
///
/// This proves "N prompts were raised and answered". It does NOT by itself
/// prove the ability that raised them fully resolved to completion — pair it
/// with a zone or state assertion for that.
fn drain_named_choices(runner: &mut engine::game::scenario::GameRunner, choice: &str) -> u32 {
    let mut answered = 0;
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
            runner
                .act(engine::types::actions::GameAction::ChooseOption {
                    choice: choice.to_string(),
                })
                .expect("answer the open colour prompt");
            answered += 1;
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(engine::types::actions::GameAction::PassPriority)
            .expect("pass priority toward resolution");
    }
    answered
}

// ---------------------------------------------------------------------------
// T1 — TWO SIMULTANEOUSLY LIVE grants from the SAME source.
// ---------------------------------------------------------------------------

/// T1 (RUNTIME) — CR 608.2h. TWO SIMULTANEOUSLY LIVE grants from
/// the SAME source each keep their OWN colour.
///
/// The sibling test below, `knight_of_dawn_second_activation_uses_its_own_color`,
/// crosses a turn boundary, so grant 1 has EXPIRED (CR 514.2) before grant 2
/// is created — it pins the storage split (U4) but cannot discriminate the
/// per-effect LATCH (U1), because only one grant is ever live at a time. This
/// test activates TWICE IN THE SAME TURN, so both grants are live
/// simultaneously and the per-effect latch is the ONLY mechanism that can
/// keep them apart.
///
/// THE ASSERTIONS THAT FLIP if the latch (U1) is reverted: after activation 2,
/// grant 1 (`Protection(Color(Red))`) is retroactively rewritten to Blue,
/// because the layer applier's `chosen_color()` pre-read reads whichever
/// answer is CURRENT at layer-evaluation time rather than the answer that was
/// current when grant 1 was created.
#[test]
fn knight_of_dawn_two_live_grants_keep_their_own_colors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let knight = scenario
        .add_creature_from_oracle(P0, "Knight of Dawn", 2, 2, KNIGHT_OF_DAWN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 1,
        })
        .id();
    // Exactly the two activations' cost — an unbounded pool would let an
    // unaffordable activation pass unnoticed.
    scenario.with_mana_pool(P0, white_mana(4));

    let mut runner = scenario.build();

    // ACTIVATION 1 — Red.
    let first = runner.activate(knight, 0).choose_option("Red").resolve();
    // POSITIVE REACH-GUARD (a): activation 1 resolves.
    assert!(
        matches!(first.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 1 must resolve, got {:?}",
        first.final_waiting_for()
    );
    // POSITIVE REACH-GUARD (b): grant 1 genuinely landed, so the later
    // assertion about it surviving is not vacuous.
    assert!(
        has_protection_from(&runner.state().objects[&knight], ManaColor::Red),
        "activation 1 must grant protection from RED: {:?}",
        runner.state().objects[&knight].keywords
    );
    let tce_after_first = runner.state().transient_continuous_effects.len();

    // ACTIVATION 2 — Blue, SAME TURN, so grant 1 is still live.
    let second = runner.activate(knight, 0).choose_option("Blue").resolve();
    assert!(
        matches!(second.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 2 must resolve, got {:?}",
        second.final_waiting_for()
    );
    // POSITIVE REACH-GUARD (c): the two assertions below are about TWO
    // grants, not one.
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        tce_after_first + 1,
        "activation 2 must create a SECOND transient continuous effect"
    );

    // THE ASSERTIONS THAT FLIP.
    assert!(
        has_protection_from(&runner.state().objects[&knight], ManaColor::Red),
        "grant 1 must KEEP red — CR 608.2h fixes its colour when it was applied: {:?}",
        runner.state().objects[&knight].keywords
    );
    assert!(
        has_protection_from(&runner.state().objects[&knight], ManaColor::Blue),
        "grant 2 must bind its OWN colour, blue: {:?}",
        runner.state().objects[&knight].keywords
    );
    assert_eq!(
        chosen_colors(&runner, knight),
        vec![ManaColor::Red, ManaColor::Blue],
        "both answers are recorded, oldest first"
    );
}

/// T9d (RUNTIME) — CR 607.2d + CR 514.2. A permanent that activates its colour
/// choice on two different turns grants protection from its SECOND answer.
///
/// The two activations are on DIFFERENT turns on purpose: the first grant has
/// expired (CR 514.2) before the second is created, so this asserts only the
/// storage-split seam (U4) and not U1's per-effect latch, which
/// `knight_of_dawn_two_live_grants_keep_their_own_colors` (T1) covers.
#[test]
fn knight_of_dawn_second_activation_uses_its_own_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let knight = scenario
        .add_creature_from_oracle(P0, "Knight of Dawn", 2, 2, KNIGHT_OF_DAWN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 1,
        })
        .id();
    // Exactly the `{W}{W}` the first activation costs — an unbounded pool would
    // let an unaffordable activation pass unnoticed.
    scenario.with_mana_pool(P0, white_mana(2));

    let mut runner = scenario.build();

    // ACTIVATION 1 — Red. POSITIVE REACH-GUARD: the grant genuinely lands.
    let first = runner.activate(knight, 0).choose_option("Red").resolve();
    assert!(
        matches!(first.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 1 must resolve, got {:?}",
        first.final_waiting_for()
    );
    assert_eq!(
        chosen_colors(&runner, knight),
        vec![ManaColor::Red],
        "activation 1 records its own answer"
    );
    assert!(
        has_protection_from(&runner.state().objects[&knight], ManaColor::Red),
        "activation 1 must actually grant protection from RED: {:?}",
        runner.state().objects[&knight].keywords
    );

    // CR 514.2: cross the turn boundary (through cleanup) so the first grant
    // expires. The ability has no timing restriction, so the next turn's upkeep
    // is a legal activation window (CR 602.2).
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("declare no attackers (CR 508.1)");
    runner.advance_to_upkeep();
    // REACH-GUARD: the turn really turned over and the first grant really
    // expired, so the "protection from red is gone" assertion at the end is
    // about the SECOND activation's answer and not about a leftover first one.
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the run must cross the turn boundary to activate again, got {:?}",
        runner.state().phase
    );
    assert!(
        !has_protection_from(&runner.state().objects[&knight], ManaColor::Red),
        "CR 514.2: the first grant must have expired at cleanup: {:?}",
        runner.state().objects[&knight].keywords
    );
    // CR 117.3a: the new turn's active player receives priority first; pass it
    // to the Knight's controller.
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .expect("pass priority to the Knight's controller");
    // CR 500.5 + CR 703.4q: unspent mana empties as a step or phase ENDS, as a
    // turn-based action — refill exactly the second activation's cost. (CR 500.4
    // is the step/phase BEGINS rule and covers effect expiry, not the pool.)
    for unit in white_mana(2) {
        let _ = runner.state_mut().add_mana_to_pool(P0, unit);
    }

    // ACTIVATION 2 — Blue.
    let second = runner.activate(knight, 0).choose_option("Blue").resolve();
    assert!(
        matches!(second.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 2 must resolve, got {:?}",
        second.final_waiting_for()
    );

    // SHAPE ROW — CR 400.7: the answers ACCUMULATE; oldest first.
    assert_eq!(
        chosen_colors(&runner, knight),
        vec![ManaColor::Red, ManaColor::Blue],
        "T9d: CR 607.2d — the answers ACCUMULATE, oldest first"
    );
    // THE ASSERTIONS THAT FLIP if the storage split (U4) is reverted: without
    // it the source holds `[Red]` after the second choice replaces the first
    // in place, or — under the FORBIDDEN candidate that deletes only the
    // `retain` without the resolution-scoped read — the layer applier's
    // pre-read reads the OLDEST answer and bakes red again.
    assert!(
        has_protection_from(&runner.state().objects[&knight], ManaColor::Blue),
        "activation 2 must grant protection from its OWN colour: {:?}",
        runner.state().objects[&knight].keywords
    );
    assert!(
        !has_protection_from(&runner.state().objects[&knight], ManaColor::Red),
        "the expired first grant must not survive into turn 2: {:?}",
        runner.state().objects[&knight].keywords
    );
}

// ---------------------------------------------------------------------------
// T2 — U1 hostile, multi-authority, TWO RECIPIENTS.
// ---------------------------------------------------------------------------

/// T2 (RUNTIME) — CR 608.2h, hostile multi-authority form. TWO DIFFERENT
/// RECIPIENTS, each granted by the SAME source's ability, each keep their
/// OWN grant's colour.
///
/// T1's grants land on ONE object, where a wrong colour could in principle be
/// masked by keyword dedup. This test separates the recipients, so the
/// per-effect latch is the only mechanism that can produce the split.
#[test]
fn armored_guardian_two_recipients_keep_their_own_colors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let guardian = scenario
        .add_creature_from_oracle(P0, "Armored Guardian", 4, 4, ARMORED_GUARDIAN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    // Sized to exactly the two activations' cost (2 x {1}{W}{W}); white mana
    // pays both the coloured and the generic portion.
    scenario.with_mana_pool(P0, white_mana(6));

    let mut runner = scenario.build();

    // ACTIVATION 1 — bear A gains protection from Red.
    let first = runner
        .activate(guardian, 0)
        .target_object(bear_a)
        .choose_option("Red")
        .resolve();
    assert!(
        matches!(first.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 1 must resolve, got {:?}",
        first.final_waiting_for()
    );
    let tce_after_first = runner.state().transient_continuous_effects.len();
    assert!(
        has_protection_from(&runner.state().objects[&bear_a], ManaColor::Red),
        "bear A must gain protection from RED after activation 1: {:?}",
        runner.state().objects[&bear_a].keywords
    );

    // ACTIVATION 2 — bear B gains protection from Blue.
    let second = runner
        .activate(guardian, 0)
        .target_object(bear_b)
        .choose_option("Blue")
        .resolve();
    assert!(
        matches!(second.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation 2 must resolve, got {:?}",
        second.final_waiting_for()
    );
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        tce_after_first + 1,
        "activation 2 must create a SECOND transient continuous effect"
    );
    // POSITIVE REACH-GUARD: each bear carries SOME protection keyword, so
    // "not Blue" / "not Red" below cannot pass vacuously on an empty list.
    assert!(
        runner.state().objects[&bear_a]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Protection(_))),
        "bear A must carry a protection keyword"
    );
    assert!(
        runner.state().objects[&bear_b]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Protection(_))),
        "bear B must carry a protection keyword"
    );

    // THE ASSERTIONS THAT FLIP: each bear keeps its OWN grant's colour.
    assert!(
        has_protection_from(&runner.state().objects[&bear_a], ManaColor::Red),
        "bear A must keep grant 1's colour, RED: {:?}",
        runner.state().objects[&bear_a].keywords
    );
    assert!(
        !has_protection_from(&runner.state().objects[&bear_a], ManaColor::Blue),
        "bear A must NOT carry grant 2's colour: {:?}",
        runner.state().objects[&bear_a].keywords
    );
    assert!(
        has_protection_from(&runner.state().objects[&bear_b], ManaColor::Blue),
        "bear B must bind grant 2's OWN colour, BLUE: {:?}",
        runner.state().objects[&bear_b].keywords
    );
    assert!(
        !has_protection_from(&runner.state().objects[&bear_b], ManaColor::Red),
        "bear B must NOT carry grant 1's colour: {:?}",
        runner.state().objects[&bear_b].keywords
    );
}

// ---------------------------------------------------------------------------
// T3 — U1's SECOND consumer (CR 702.16c attach legality), standalone.
// ---------------------------------------------------------------------------

/// T3 (RUNTIME) — CR 702.16c: U1's SECOND consumer. A creature's protection
/// GATES Aura attachment, and each Armored Guardian grant gates it by that
/// grant's OWN colour.
///
/// Standalone `#[test]`, deliberately NOT folded into T2's scenario: under a
/// revert that removes the per-effect latch (U1) but keeps the storage split
/// (U4), a version folded into T2's own scenario would panic at T2's colour
/// assertions and never reach the attach-legality rows below at all.
///
/// THE OBSERVABLE IS `attached_to`, NEVER `attach_to`'s return value:
/// `attach_to` returns `None` on an illegal attach AND on a no-op alike, so it
/// carries no legality information by itself.
///
/// `/card-test` check 9 waiver: this test drives `game::effects::attach::attach_to`
/// (a `pub` fn) directly rather than a cast — the repo's own idiom for
/// attach-legality tests (`aura_on_player.rs`, `aura_token_attach_guard.rs`,
/// `archnemesis_you_attack_enchanted_player.rs`, `aspect_of_wolf_per_axis_xy.rs`,
/// `aura_graft_enchant_restriction.rs`). U1's PRODUCTION half is covered by
/// T1/T2, which drive real activations; this consumer has no cast-shaped
/// entry point of its own.
#[test]
fn armored_guardian_grants_gate_aura_attachment_per_grant_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let guardian = scenario
        .add_creature_from_oracle(P0, "Armored Guardian", 4, 4, ARMORED_GUARDIAN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    // Grant-free: neither activation targets this bear.
    let bear_c = scenario.add_creature(P0, "Bear C", 2, 2).id();
    scenario.with_mana_pool(P0, white_mana(6));

    // Firebreathing's colour comes from its PRINTED MANA COST, never a
    // hand-set `color` field.
    let red_aura = scenario
        .add_enchantment_from_oracle(P0, "Firebreathing", FIREBREATHING)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(one_color(ManaCostShard::Red))
        .from_oracle_text_with_keywords(&["Enchant"], FIREBREATHING)
        .id();
    let blue_aura = scenario
        .add_enchantment_from_oracle(P0, "Firebreathing", FIREBREATHING)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .from_oracle_text_with_keywords(&["Enchant"], FIREBREATHING)
        .id();

    let mut runner = scenario.build();
    // Build-time attach to the GRANT-FREE bear, via the PRODUCTION `attach_to`
    // (not a raw field write) — this is also reach-guard 3, the instrument
    // proving `attach_to` itself works on this fixture. Without it, CR 704.5m
    // would put an unattached Aura into the graveyard before the activations
    // below even resolve.
    attach_to(runner.state_mut(), red_aura, bear_c);
    assert_eq!(
        runner.state().objects[&red_aura].attached_to,
        Some(bear_c.into()),
        "reach-guard 3: the red Aura must attach to the grant-free bear"
    );
    attach_to(runner.state_mut(), blue_aura, bear_c);
    assert_eq!(
        runner.state().objects[&blue_aura].attached_to,
        Some(bear_c.into()),
        "reach-guard 3: the blue Aura must attach to the grant-free bear"
    );

    // Grant 1 — bear A, red. Grant 2 — bear B, blue.
    let first = runner
        .activate(guardian, 0)
        .target_object(bear_a)
        .choose_option("Red")
        .resolve();
    assert!(matches!(
        first.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let second = runner
        .activate(guardian, 0)
        .target_object(bear_b)
        .choose_option("Blue")
        .resolve();
    assert!(matches!(
        second.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    // POSITIVE REACH-GUARD, deliberately colour-agnostic: a colour-specific
    // guard would itself fail on every U1-absent tree, before ever reaching
    // the attach rows below (that assertion is T2's job, not this test's).
    assert!(
        runner.state().objects[&bear_a]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Protection(_))),
        "bear A must carry SOME protection keyword"
    );
    assert!(
        runner.state().objects[&bear_b]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Protection(_))),
        "bear B must carry SOME protection keyword"
    );

    // THE ASSERTION THAT FLIPS: CR 702.16c — the red Aura must be REFUSED by
    // a creature with protection from red; its host must be unchanged.
    let before = runner.state().objects[&red_aura].attached_to;
    attach_to(runner.state_mut(), red_aura, bear_a);
    assert_eq!(
        runner.state().objects[&red_aura].attached_to,
        before,
        "CR 702.16c: the red Aura must be REFUSED by a creature with protection from red; its \
         host must be unchanged"
    );

    // REACH-GUARD 1: the SAME red Aura DOES attach to bear B (protection from
    // blue does not stop it).
    attach_to(runner.state_mut(), red_aura, bear_b);
    assert_eq!(
        runner.state().objects[&red_aura].attached_to,
        Some(bear_b.into()),
        "the red Aura must be ACCEPTED by a creature whose protection is from blue"
    );

    // REACH-GUARD 2 — mirror: blue Aura refused by bear B.
    let before = runner.state().objects[&blue_aura].attached_to;
    attach_to(runner.state_mut(), blue_aura, bear_b);
    assert_eq!(
        runner.state().objects[&blue_aura].attached_to,
        before,
        "CR 702.16c: the blue Aura must be REFUSED by a creature with protection from blue"
    );
    attach_to(runner.state_mut(), blue_aura, bear_a);
    assert_eq!(
        runner.state().objects[&blue_aura].attached_to,
        Some(bear_a.into()),
        "the blue Aura must be ACCEPTED by a creature whose protection is from red"
    );

    // Zone guard.
    assert_eq!(runner.state().objects[&bear_a].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bear_b].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bear_c].zone, Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// T9b/c — Wash Out recast on the same storage object.
// ---------------------------------------------------------------------------

/// T9b (RUNTIME) — CR 607.2d + CR 400.7 + CR 608.2d. A source that resolves a
/// colour choice TWICE binds its OWN answer the second time.
///
/// Wash Out is a sorcery, so CR 608.2n puts it into the graveyard on
/// resolution. The recast therefore has to reuse the SAME storage object —
/// `move_to_zone(.., Zone::Hand, ..)` on the same `ObjectId`, the shape a
/// Regrowth / Yawgmoth's Will recursion produces — rather than casting a second
/// copy, which would be two objects and would not reach this seam at all.
///
/// THE ASSERTION THAT FLIPS on revert: `red_bear` in `Zone::Hand` after the
/// second cast. Before the fix the source held `[Color(Blue), Color(Red)]` (or,
/// under the pre-U4 `retain`, only `[Color(Red)]`), and a reader that took the
/// FIRST match would bounce BLUE permanents again while red stayed put.
#[test]
fn wash_out_recast_on_the_same_object_uses_its_own_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();
    let green_bear = scenario
        .add_creature(P0, "Green Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Green))
        .id();
    // Multi-authority decoy: a DIFFERENT object that already chose Green. The
    // read must stay bound to the resolving source, not to "any chosen colour
    // on the board".
    let decoy = scenario
        .add_creature(P0, "Decoy Bear", 1, 1)
        .with_mana_cost(one_color(ManaCostShard::White))
        .id();

    let wash_out = scenario
        .add_spell_to_hand_from_oracle(P0, "Wash Out", false, WASH_OUT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&decoy)
        .expect("decoy exists")
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Green));

    // CAST 1 — choose Blue.
    let first = runner.cast(wash_out).choose_option("Blue").resolve();
    // POSITIVE REACH-GUARD (a): the first resolution genuinely ran.
    first.assert_zone(&[blue_bear], Zone::Hand);
    first.assert_zone(&[red_bear, green_bear, decoy], Zone::Battlefield);
    assert_eq!(
        chosen_colors(&runner, wash_out),
        vec![ManaColor::Blue],
        "the first choice must be recorded on the source"
    );
    // CR 608.2n: the sorcery is in the graveyard, which is why the recast has to
    // move the SAME object back to hand.
    assert_eq!(runner.state().objects[&wash_out].zone, Zone::Graveyard);

    // CR 400.7: recursion returns the SAME storage object to hand. Nothing
    // clears `chosen_attributes` on this move — only `reset_for_battlefield_entry`
    // does, and a spell never reaches it.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), wash_out, Zone::Hand, &mut events);
    // POSITIVE REACH-GUARD (b): the object really is back in hand and really is
    // still carrying the first answer, so the second cast reaches the seam under
    // test rather than a freshly-cleared object.
    assert_eq!(runner.state().objects[&wash_out].zone, Zone::Hand);
    assert_eq!(
        chosen_colors(&runner, wash_out),
        vec![ManaColor::Blue],
        "the move to hand must not clear the prior answer — otherwise this test \
         would pass without the fix"
    );

    // POSITIVE REACH-GUARD (c) on the RUNTIME CHOICE PATH: the second cast
    // raises its OWN prompt. `drive_resolution` answers a `NamedChoice` window
    // only when a choice was declared and otherwise breaks, so a declared
    // `choose_option` that is never consumed would be silent.
    let halted = runner.cast(wash_out).resolve();
    assert!(
        matches!(
            halted.final_waiting_for(),
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::Color { .. },
                ..
            }
        ),
        "the SECOND resolution must raise its own colour prompt, got {:?}",
        halted.final_waiting_for()
    );
    runner
        .act(engine::types::actions::GameAction::ChooseOption {
            choice: "Red".to_string(),
        })
        .expect("answer the second colour prompt");
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(engine::types::actions::GameAction::PassPriority)
            .expect("pass priority toward resolution");
    }

    // SHAPE ROW — T9b: CR 607.2d — the answers ACCUMULATE; the second cast
    // reads the NEWEST via `current_chosen_color()`.
    assert_eq!(
        chosen_colors(&runner, wash_out),
        vec![ManaColor::Blue, ManaColor::Red],
        "T9b: CR 607.2d — the answers ACCUMULATE; the second cast reads the NEWEST via \
         current_chosen_color()"
    );
    // THE ASSERTION THAT FLIPS if the storage split is reverted (load-bearing).
    assert_eq!(
        runner.state().objects[&red_bear].zone,
        Zone::Hand,
        "the second resolution must bounce its OWN colour"
    );
    // NEGATIVE, with the positive above as its guard: the untouched colours stay.
    assert_eq!(runner.state().objects[&green_bear].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&decoy].zone,
        Zone::Battlefield,
        "the decoy's own chosen colour must never govern this resolution"
    );
    // REACH-GUARD: the second cast finished rather than hanging on a prompt.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no further prompt may be raised, got {:?}",
        runner.state().waiting_for
    );
}

/// T9c (RUNTIME) — CR 105.4. The second resolution choosing a colour NOTHING
/// has moves nothing, and in particular does not fall back to the first
/// answer.
///
/// This is the vacuity guard for T9b: if the second resolution silently
/// reused the first colour, this run would bounce `blue_bear` again.
#[test]
fn wash_out_recast_choosing_an_absent_color_moves_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();

    let wash_out = scenario
        .add_spell_to_hand_from_oracle(P0, "Wash Out", false, WASH_OUT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let first = runner.cast(wash_out).choose_option("Blue").resolve();
    // POSITIVE REACH-GUARD: the first cast really bounced its colour.
    first.assert_zone(&[blue_bear], Zone::Hand);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), wash_out, Zone::Hand, &mut events);
    // Put the blue bear back so a first-answer reuse would be observable.
    move_to_zone(
        runner.state_mut(),
        blue_bear,
        Zone::Battlefield,
        &mut events,
    );
    assert_eq!(runner.state().objects[&blue_bear].zone, Zone::Battlefield);

    let second = runner.cast(wash_out).choose_option("White").resolve();

    // THE NEGATIVE: nothing moved, because nothing is white.
    second.assert_zone(&[blue_bear, red_bear], Zone::Battlefield);
    // SHAPE ROW — the answers accumulate; `current_chosen_color()` reads the
    // newest, White, which is what governed this resolution.
    assert_eq!(
        chosen_colors(&runner, wash_out),
        vec![ManaColor::Blue, ManaColor::White],
        "the source ACCUMULATES both answers; the resolution reads only its CURRENT one"
    );
    // REACH-GUARD: the spell resolved (CR 608.2n) rather than hanging.
    assert_eq!(runner.state().objects[&wash_out].zone, Zone::Graveyard);
    assert!(
        matches!(second.final_waiting_for(), WaitingFor::Priority { .. }),
        "no further prompt may be raised, got {:?}",
        second.final_waiting_for()
    );
}

// ---------------------------------------------------------------------------
// Prismatic Strands — the only pin for U4d.
// ---------------------------------------------------------------------------

/// (RUNTIME) — CR 608.2d. The ONLY test that pins U4d: a prevention shield
/// resolves its `IsChosenColor` filter into a concrete `HasColor` at CREATION
/// time, from the source's CURRENT (newest) chosen colour — not its
/// CR 607.2d linked (oldest) one.
///
/// Built on the Wash Out recast recipe: `move_to_zone` back to hand, then a
/// second cast driven by hand (same storage object, CR 400.7) — Prismatic
/// Strands' own Flashback ability is not exercised; the recast mechanism does
/// not matter to CR 400.7's "same object, new resolution" premise.
///
/// THE ASSERTION THAT FLIPS: the SECOND shield's resolved filter carries
/// `HasColor(Blue)`. Under U4d reverted, `prevent_damage.rs` would resolve
/// BOTH shields' filters from `chosen_color()` (oldest), so shield [1] would
/// wrongly carry `HasColor(Red)`.
#[test]
fn prismatic_strands_recast_on_the_same_object_prevents_its_own_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let strands = scenario
        .add_spell_to_hand_from_oracle(P0, "Prismatic Strands", true, PRISMATIC_STRANDS)
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Flashback"], PRISMATIC_STRANDS)
        .id();

    let mut runner = scenario.build();

    // CAST 1 — start the cast with NO pre-supplied answer, so the number of
    // prompts raised is OBSERVABLE via `drain_named_choices` (a pre-supplied
    // `.choose_option()` answer cannot distinguish 1 prompt from N).
    runner.cast(strands).resolve();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "the cast must raise its own colour prompt, got {:?}",
        runner.state().waiting_for
    );
    let answered_1 = drain_named_choices(&mut runner, "Red");
    // POSITIVE REACH-GUARD: cast 1 raises EXACTLY one prompt.
    assert_eq!(answered_1, 1, "cast 1 must raise exactly ONE colour prompt");
    assert_eq!(
        chosen_colors(&runner, strands),
        vec![ManaColor::Red],
        "cast 1 records its own answer"
    );
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "cast 1 must install exactly ONE prevention shield"
    );
    assert!(
        shield_has_color(
            &runner.state().pending_damage_replacements[0],
            ManaColor::Red
        ),
        "shield 0's resolved filter must carry HasColor(Red)"
    );
    assert_eq!(runner.state().objects[&strands].zone, Zone::Graveyard);

    // CR 400.7: the move to hand must not clear the prior answer.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), strands, Zone::Hand, &mut events);
    assert_eq!(runner.state().objects[&strands].zone, Zone::Hand);
    assert_eq!(
        chosen_colors(&runner, strands),
        vec![ManaColor::Red],
        "the move to hand must not clear the prior answer"
    );

    // CAST 2 — Blue.
    runner.cast(strands).resolve();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "cast 2 must raise its own colour prompt, got {:?}",
        runner.state().waiting_for
    );
    let answered_2 = drain_named_choices(&mut runner, "Blue");
    // POSITIVE REACH-GUARD: cast 2 raises EXACTLY one prompt, and installs a
    // SECOND shield — `len() == 2` proves both shields exist, so the
    // assertion below is about the right one.
    assert_eq!(answered_2, 1, "cast 2 must raise exactly ONE colour prompt");
    assert_eq!(
        chosen_colors(&runner, strands),
        vec![ManaColor::Red, ManaColor::Blue],
        "the answers accumulate on the storage object (CR 400.7)"
    );
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        2,
        "cast 2 must install a SECOND prevention shield"
    );
    // POSITIVE REACH-GUARD: a live shield is not retroactively rewritten.
    assert!(
        shield_has_color(
            &runner.state().pending_damage_replacements[0],
            ManaColor::Red
        ),
        "shield 0 must still filter its ORIGINAL colour, red"
    );

    // THE ASSERTION THAT FLIPS.
    assert_eq!(
        shield_colors(&runner.state().pending_damage_replacements[1]),
        vec![ManaColor::Blue],
        "CR 608.2d: cast 2's shield must filter its OWN colour, blue"
    );
}

// ---------------------------------------------------------------------------
// T3 (as-enters bucket) — the 59-card as-enters bucket is a no-op.
// ---------------------------------------------------------------------------

/// (RUNTIME) — CR 400.7. The as-enters chooser class is untouched: the
/// permanent holds exactly one `Color` before and after, and its dependent
/// static still applies.
///
/// `reset_for_battlefield_entry` clears `chosen_attributes` before each
/// battlefield entry, so this bucket could never accumulate; the row exists so a
/// storage-split defect that fired on the wrong key would be caught here rather
/// than in the pool.
#[test]
fn as_enters_color_chooser_still_holds_exactly_one_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();

    let hall = scenario
        .add_artifact_to_hand_from_oracle(P0, "Hall of Triumph", HALL_OF_TRIUMPH)
        .with_mana_cost(ManaCost::zero())
        .as_legendary()
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(hall).choose_option("Blue").resolve();

    // POSITIVE REACH-GUARD (a): the artifact entered.
    outcome.assert_zone(&[hall], Zone::Battlefield);
    // POSITIVE REACH-GUARD (b): the dependent static really applies, so the
    // choice was read back and the assertion below is not about a dead value.
    assert_eq!(
        runner.state().objects[&blue_bear].power,
        Some(3),
        "the chosen-colour anthem must apply to the blue creature"
    );
    assert_eq!(
        runner.state().objects[&red_bear].power,
        Some(2),
        "the anthem must not apply to another colour"
    );

    // THE INVARIANT: exactly one chosen colour on the source.
    assert_eq!(
        chosen_colors(&runner, hall),
        vec![ManaColor::Blue],
        "an as-enters chooser holds exactly one colour"
    );
}

// ---------------------------------------------------------------------------
// T4 — U5, Floating Shield through the FULL production path.
// ---------------------------------------------------------------------------

/// T4 (RUNTIME) — U5 production-path coverage for the Floating Shield linked-
/// choice class. `Keyword::Enchant` is NOT scaffolding: CR 303.4a makes the
/// enchant ability the Aura spell's targeting authority — `from_oracle_text`
/// alone leaves `keywords = []`, `spell_targets` is empty, and CR 704.5m bins
/// the spell before it can even resolve. `from_oracle_text_with_keywords`
/// supplies it.
///
/// STATUS, stated plainly: this test asserts behaviour ALREADY GREEN at
/// `PHASE_BASE_SHA` and discriminates NO production change this phase makes —
/// it is F-C coverage, not a regression test. Its discriminating sibling for
/// this phase's storage-split change is T5, below.
#[test]
fn floating_shield_cast_records_and_reuses_the_as_enters_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let shield = scenario
        .add_enchantment_from_oracle(P0, "Floating Shield", FLOATING_SHIELD)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Enchant"], FLOATING_SHIELD)
        .id();
    // Exactly the printed cost ({2}{W}) — an unbounded pool would let an
    // unaffordable cast pass unnoticed.
    scenario.with_mana_pool(P0, white_mana(3));

    let mut runner = scenario.build();
    // Seeding lands directly on the battlefield with no ETB replacement run
    // (CR 603.6a note on `from_oracle_text_with_keywords`); move it to hand so
    // casting it below drives the FULL production path, including the
    // as-enters colour replacement.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), shield, Zone::Hand, &mut events);
    assert_eq!(runner.state().objects[&shield].zone, Zone::Hand);
    assert!(
        chosen_colors(&runner, shield).is_empty(),
        "reach-guard: the Aura must not carry a colour before it is cast"
    );

    let outcome = runner
        .cast(shield)
        .target_object(host)
        .choose_option("Blue")
        .resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the cast must resolve, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[shield], Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&shield].attached_to,
        Some(host.into()),
        "CR 608.3c: the Aura must attach to the creature it targeted"
    );
    assert_eq!(
        chosen_colors(&runner, shield),
        vec![ManaColor::Blue],
        "the as-enters choice must be recorded through PRODUCTION, never pushed by hand"
    );
    assert!(
        has_protection_from(&runner.state().objects[&host], ManaColor::Blue),
        "the linked STATIC must give the host protection from the as-enters colour: {:?}",
        runner.state().objects[&host].keywords
    );
}

// ---------------------------------------------------------------------------
// T5 — U5/U3 guard, the linked grant off a production board.
// ---------------------------------------------------------------------------

/// T5 (RUNTIME) — CR 607.2d. Floating Shield's "Sacrifice this Aura:" grant
/// reads the colour chosen by its LINKED as-enters replacement, off a board
/// built entirely through PRODUCTION (cast, not hand-seeded).
///
/// This is the card the storage split (U4) would otherwise break: before the
/// parser's `LinkedColorChoice` relation, the sacrifice ability's injector
/// handed it a chooser of its own, so the Aura would accumulate a SECOND
/// answer and the sacrifice grant would bind the activation's own (spurious)
/// colour instead of the as-enters one.
///
/// THE ASSERTION THAT FLIPS: the activation, resolved with NO colour
/// declared, ends at `WaitingFor::Priority` — the linked grant raises NO
/// prompt of its own (CR 607.2d). `drive_resolution` halts at any
/// `NamedChoice` it has no answer for, so "reaches `Priority` with nothing
/// declared" proves zero prompts exactly.
///
/// REACH-GUARD ORDERING: the sacrifice-cost zone guard (the shield reaching
/// the graveyard) is asserted BEFORE the flip, not after — costs are paid on
/// activation, before resolution, so it is the guard that establishes the
/// ACTIVATION itself happened. Without it, a broken fixture where the
/// activation never ran at all would satisfy "reaches `Priority`" just as
/// readily as correct behaviour (nothing would be on the stack either way).
/// The recipient's protection delta and the Aura's shape row are downstream
/// of the resolution the flip is about and stay AFTER it — moving them above
/// would make this test fail at a GUARD, rather than at its own
/// discriminator, on the tree where the production change is absent.
#[test]
fn floating_shield_sacrifice_grant_reads_the_as_enters_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let recipient = scenario.add_creature(P0, "Recipient Bear", 2, 2).id();
    let shield = scenario
        .add_enchantment_from_oracle(P0, "Floating Shield", FLOATING_SHIELD)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Enchant"], FLOATING_SHIELD)
        .id();
    // Exactly the printed cost ({2}{W}) — an unbounded pool would let an
    // unaffordable cast pass unnoticed. The later sacrifice activation has
    // no mana component, so this pool need not cover it too.
    scenario.with_mana_pool(P0, white_mana(3));

    let mut runner = scenario.build();
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), shield, Zone::Hand, &mut events);

    let cast = runner
        .cast(shield)
        .target_object(host)
        .choose_option("Blue")
        .resolve();
    // POSITIVE REACH-GUARD (a): the Aura is really on the battlefield,
    // attached to its cast target.
    cast.assert_zone(&[shield], Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&shield].attached_to,
        Some(host.into()),
        "CR 608.3c: the Aura must attach to its cast target"
    );
    // POSITIVE REACH-GUARD (b): the linked STATIC reads the same choice, so
    // the suppression did not strand the enchanted creature's protection.
    assert!(
        has_protection_from(&runner.state().objects[&host], ManaColor::Blue),
        "the enchanted creature must have protection from the as-enters colour: {:?}",
        runner.state().objects[&host].keywords
    );

    // Resolve the sacrifice activation with NO colour declared.
    let outcome = runner
        .activate(shield, 0)
        .target_object(recipient)
        .resolve();

    // POSITIVE REACH-GUARD (c) — moved ABOVE the flip: the sacrifice cost was
    // really paid, which is what proves the ACTIVATION under test actually
    // happened rather than never having started.
    outcome.assert_zone(&[shield], Zone::Graveyard);

    // THE ASSERTION THAT FLIPS.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the linked grant must raise NO colour prompt of its own, got {:?}",
        outcome.final_waiting_for()
    );

    // Downstream claims — NOT reach-guards for the flip; not evaluated on the
    // tree where the linked-choice relation is withheld (the run parks before
    // reaching either of these).
    assert!(
        has_protection_from(&runner.state().objects[&recipient], ManaColor::Blue),
        "the sacrifice grant must bake the as-enters colour: {:?}",
        runner.state().objects[&recipient].keywords
    );
    assert_eq!(
        chosen_colors(&runner, shield),
        vec![ManaColor::Blue],
        "the Aura must still hold exactly its as-enters answer"
    );
}

// ---------------------------------------------------------------------------
// T6 — CR 614.15 + CR 608.2d: a self-replacement override announces its OWN
// choice.
// ---------------------------------------------------------------------------

/// Faith's Shield {W} Instant — verbatim (Scryfall-verified). The
/// "Fateful hour — " ability word is part of the printed text.
const FAITHS_SHIELD: &str =
    "Target permanent you control gains protection from the color of your choice until end of \
     turn.\nFateful hour — If you have 5 or less life, instead you and each permanent you control \
     gain protection from the color of your choice until end of turn.";

/// T6 (RUNTIME) — CR 614.15 + CR 608.2d. Faith's Shield's fateful-hour
/// self-replacement override announces its OWN colour choice, rather than
/// silently reading (and destroying) the base's.
///
/// THIS PHASE'S REGRESSION, not a pre-existing bug: `game/ability_utils.rs`
/// `apply_instead_swap` (`overridden.effect = sub.effect`) discards the base's
/// effect — including its injected chooser — the moment the override applies.
/// `detect_linked_choice_linked_color`'s deleted CR 614.15 arm suppressed the
/// override's own chooser on the theory that the base's choice would carry
/// over; it does not, because the swap throws the base away before the choice
/// is announced. Before this fix: 0 prompts, `chosen == []`, and every
/// controlled permanent stayed unprotected despite two continuous effects
/// being created (`tce == 2`) — the colour to bake into them was simply never
/// chosen.
#[test]
fn faiths_shield_fateful_hour_branch_chooses_its_own_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 5);

    let targeted = scenario.add_creature(P0, "Targeted Bear", 2, 2).id();
    let untargeted = scenario.add_creature(P0, "Untargeted Bear", 2, 2).id();
    let opponents = scenario.add_creature(P1, "Opponent's Bear", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Faith's Shield", true, FAITHS_SHIELD)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .from_oracle_text_with_keywords(&["Fateful hour"], FAITHS_SHIELD)
        .id();
    // Exactly the printed cost ({W}) — an unbounded pool would let an
    // unaffordable cast pass unnoticed.
    scenario.with_mana_pool(P0, white_mana(1));

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(targeted).resolve();
    // REACH-GUARD (a): the run parks at the override's OWN colour prompt
    // before any answer is supplied.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::Color { .. },
                ..
            }
        ),
        "the fateful-hour branch must announce its own CR 608.2d colour choice, got {:?}",
        outcome.final_waiting_for()
    );

    let prompts = drain_named_choices(&mut runner, "Red");
    // COUNTED PROMPT: exactly one, not the two a naive un-suppression fix
    // would raise (one at cast time from the base's chooser, unreachable
    // because it never resolves, plus one from the override).
    assert_eq!(
        prompts, 1,
        "the fateful branch announces exactly one CR 608.2d colour choice"
    );
    assert_eq!(
        chosen_colors(&runner, spell),
        vec![ManaColor::Red],
        "the override's own answer must be recorded"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the drained run must reach priority: {:?}",
        runner.state().waiting_for
    );

    // REACH-GUARD (d), NOT a discriminator (measured 2 on both trees — it
    // proves the branch applied, nothing more): one transient continuous
    // effect per controlled permanent.
    assert_eq!(
        runner.state().transient_continuous_effects.len(),
        2,
        "the fateful branch must create one continuous effect per controlled permanent"
    );

    // THE ASSERTIONS THAT FLIP: the fateful branch's WIDE scope — "you and
    // each permanent you control" — reaches both of P0's permanents, targeted
    // or not.
    assert!(
        has_protection_from(&runner.state().objects[&targeted], ManaColor::Red),
        "the targeted permanent must gain protection from the chosen colour"
    );
    assert!(
        has_protection_from(&runner.state().objects[&untargeted], ManaColor::Red),
        "the untargeted controlled permanent must ALSO gain protection (wide fateful scope)"
    );

    // NEGATIVE, paired with the two positives above and the counted prompt so
    // it cannot pass on a card that granted nothing: the opponent's permanent
    // is outside "you and each permanent you control" and must get nothing.
    assert!(
        !has_protection_from(&runner.state().objects[&opponents], ManaColor::Red),
        "the opponent's permanent must NOT gain protection: {:?}",
        runner.state().objects[&opponents].keywords
    );

    // STATED LIMITATION (F11): this test does not and cannot assert
    // player-level protection for the controller. The fateful branch creates
    // no player-scoped continuous effect at all (`tce == 2`, one per
    // controlled permanent, none for the player), and
    // `game/static_abilities.rs::player_protection_from_object` has no
    // coloured-transient authority to read even if it did. Narrower than the
    // literal "you ... gain protection" ask; filed as backlog F11.
}

/// T6's non-fateful sibling — same construction, life kept above 5 so the
/// BASE ability (not the override) applies.
///
/// Asserts the counted single prompt and the recorded colour, but
/// DELIBERATELY DOES NOT assert protection: the non-fateful branch installs
/// ZERO transient continuous effects, measured identically at
/// `PHASE_BASE_SHA`, on the candidate, and with U2 reverted — this is
/// PRE-EXISTING and OUT OF SCOPE for this phase. Filed as backlog F12; its
/// mechanism (an announced-but-unbound target on the base ability's own
/// node) is a hypothesis, not diagnosed to the line. Asserting protection
/// here would misrepresent an undiagnosed pre-existing gap as something this
/// phase pinned.
#[test]
fn faiths_shield_non_fateful_branch_still_chooses_its_own_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    let targeted = scenario.add_creature(P0, "Targeted Bear", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Faith's Shield", true, FAITHS_SHIELD)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .from_oracle_text_with_keywords(&["Fateful hour"], FAITHS_SHIELD)
        .id();
    scenario.with_mana_pool(P0, white_mana(1));

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(targeted).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::Color { .. },
                ..
            }
        ),
        "the base ability must still announce its own colour choice, got {:?}",
        outcome.final_waiting_for()
    );

    let prompts = drain_named_choices(&mut runner, "Red");
    assert_eq!(
        prompts, 1,
        "shape (i) adds no second prompt to the branch it does not apply to"
    );
    assert_eq!(chosen_colors(&runner, spell), vec![ManaColor::Red]);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// ---------------------------------------------------------------------------
// T7 — CR 607.2d + CR 608.2d: an independently printed chooser is NOT
// suppressed by a linked anaphoric reader on the same object.
// ---------------------------------------------------------------------------

/// SYNTHETIC — no such card exists. No Magic card prints both a linked
/// anaphoric reader ("the chosen color") AND an independently-choosing grant
/// ("the color of your choice") on one object (measured: printed supplier ∧
/// independent grant on one face → 0 of 35,961, reach-guard 110). This
/// fixture pins the STRUCTURAL rule at the seam where a real such card would
/// land. The anaphoric half's coverage on a REAL card is T5
/// (`floating_shield_sacrifice_grant_reads_the_as_enters_color`, above).
const SYNTHETIC_WARD: &str =
    "Enchant creature\nAs this Aura enters, choose a color.\nEnchanted creature has protection \
     from the chosen color.\n{2}: Target creature gains protection from the color of your choice \
     until end of turn.";

/// T7 (RUNTIME) — CR 607.2d + CR 608.2d. An independently printed chooser
/// ("the color of your choice") is NOT suppressed by a linked anaphoric
/// reader ("the chosen color") on the same object; each prompts and binds
/// separately.
///
/// THE ASSERTIONS THAT FLIP are in Step 2: the independent grant announces
/// its own prompt (1, not 0), the recipient binds THAT answer, and — the
/// BL-2 catcher a prior round shipped with no test for — the linked STATIC
/// on the host keeps reading its OWN supplier's answer, unmoved by the newer,
/// unrelated choice made elsewhere on the object.
#[test]
fn linked_and_independent_color_grants_on_one_object_prompt_and_bind_separately() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let recipient = scenario.add_creature(P0, "Recipient Bear", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Synthetic Ward", SYNTHETIC_WARD)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Enchant"], SYNTHETIC_WARD)
        .id();
    // {2}{W} for the cast, plus {2} for the activation.
    scenario.with_mana_pool(P0, white_mana(5));

    let mut runner = scenario.build();
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), aura, Zone::Hand, &mut events);

    // Step 1 (reach-guard) — measured identical on base and candidate: the
    // as-enters replacement's own chooser is untouched by this phase.
    let cast = runner.cast(aura).target_object(host).resolve();
    assert!(
        matches!(
            cast.final_waiting_for(),
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::Color { .. },
                ..
            }
        ),
        "the as-enters replacement must still announce its own choice, got {:?}",
        cast.final_waiting_for()
    );
    let cast_prompts = drain_named_choices(&mut runner, "Blue");
    assert_eq!(cast_prompts, 1);
    assert_eq!(chosen_colors(&runner, aura), vec![ManaColor::Blue]);
    assert!(
        has_protection_from(&runner.state().objects[&host], ManaColor::Blue),
        "the linked static must give the host protection from the as-enters colour"
    );
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Battlefield,
        "the Aura must resolve onto the battlefield"
    );

    // Step 2 — THE ASSERTIONS THAT FLIP.
    let activation = runner.activate(aura, 0).target_object(recipient).resolve();
    assert!(
        matches!(
            activation.final_waiting_for(),
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::Color { .. },
                ..
            }
        ),
        "the independent grant must announce its OWN colour choice, got {:?}",
        activation.final_waiting_for()
    );
    let act_prompts = drain_named_choices(&mut runner, "Red");
    assert_eq!(
        act_prompts, 1,
        "the independent grant announces exactly one prompt of its own"
    );
    assert_eq!(
        runner.state().objects[&recipient].keywords,
        vec![Keyword::Protection(ProtectionTarget::Color(ManaColor::Red))],
        "the recipient must bind the independent grant's OWN answer"
    );
    assert_eq!(
        runner.state().objects[&host].keywords,
        vec![Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Blue
        ))],
        "the BL-2 catcher: the linked static must keep reading its OWN supplier's answer, \
         unmoved by the newer independent choice made elsewhere on the object"
    );
    assert_eq!(
        chosen_colors(&runner, aura),
        vec![ManaColor::Blue, ManaColor::Red],
        "the Aura accumulates both answers (phase 1's storage split)"
    );
    assert_eq!(runner.state().transient_continuous_effects.len(), 1);
}

// ---------------------------------------------------------------------------
// T7c — CR 607.2d ordering hostile case.
// ---------------------------------------------------------------------------

/// SYNTHETIC — no such card exists, for the same reason T7's fixture is
/// synthetic. Two SEPARATE activated abilities so an anaphoric activation can
/// be driven both BEFORE and AFTER the independent one.
const SYNTHETIC_WARD_TWO: &str =
    "Enchant creature\nAs this Aura enters, choose a color.\n{1}: Target creature gains \
     protection from the chosen color until end of turn.\n{2}: Target creature gains protection \
     from the color of your choice until end of turn.";

/// T7c (RUNTIME) — CR 607.2d. An anaphoric grant activated AFTER an
/// independent choice keeps reading the LINKED colour, however many
/// unrelated choices the object has made since.
///
/// THE ASSERTION THAT FLIPS: the SECOND `{1}` activation (on bear C, after
/// the `{2}` activation put a second, different answer on the object) counts
/// ZERO prompts and grants `Prot(Blue)` — the object's FIRST (linked) answer,
/// not its most recent one.
///
/// HONESTY NOTE: at `PHASE_BASE_SHA` bear C also gets `Prot(Blue)`, but
/// VACUOUSLY — the object only ever held one answer there, because the `{2}`
/// activation raised 0 prompts (wrongly suppressed) rather than 1. This
/// assertion is only meaningful together with its reach-guards: the `{2}`
/// activation must have counted exactly 1 prompt and bear B must really hold
/// `Prot(Red)`, so the object genuinely holds two answers by the time the
/// final assertion runs.
#[test]
fn an_anaphoric_grant_activated_after_an_independent_choice_keeps_the_linked_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let bear_c = scenario.add_creature(P0, "Bear C", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Synthetic Ward Two", SYNTHETIC_WARD_TWO)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Enchant"], SYNTHETIC_WARD_TWO)
        .id();
    // {2}{W} for the cast, plus {1} + {2} + {1} for the three activations —
    // headroom over the exact {7}, because the activations here are
    // generic-only and the exact-pool convention is a CAST guard.
    scenario.with_mana_pool(P0, white_mana(12));

    let mut runner = scenario.build();
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), aura, Zone::Hand, &mut events);

    let cast = runner.cast(aura).target_object(bear_a).resolve();
    let cast_prompts = drain_named_choices(&mut runner, "Blue");
    assert_eq!(
        cast_prompts, 1,
        "reach-guard: the as-enters choice must land"
    );
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::NamedChoice { .. }
    ));

    // Activate ability 0 ({1}, anaphoric) on bear A, BEFORE any independent
    // choice exists.
    let act0 = runner.activate(aura, 0).target_object(bear_a).resolve();
    assert!(
        matches!(act0.final_waiting_for(), WaitingFor::Priority { .. }),
        "the anaphoric grant must raise no prompt of its own, got {:?}",
        act0.final_waiting_for()
    );
    assert!(
        has_protection_from(&runner.state().objects[&bear_a], ManaColor::Blue),
        "bear A must get the linked (as-enters) colour"
    );

    // Activate ability 1 ({2}, independent) on bear B.
    let act1 = runner.activate(aura, 1).target_object(bear_b).resolve();
    assert!(matches!(
        act1.final_waiting_for(),
        WaitingFor::NamedChoice {
            choice_type: ChoiceType::Color { .. },
            ..
        }
    ));
    let act1_prompts = drain_named_choices(&mut runner, "Red");
    // REACH-GUARD: the `{2}` activation really did count one prompt and bear
    // B really did get the independent colour, so the object genuinely holds
    // two answers when the final assertion below runs.
    assert_eq!(
        act1_prompts, 1,
        "the independent grant announces its own prompt"
    );
    assert!(
        has_protection_from(&runner.state().objects[&bear_b], ManaColor::Red),
        "bear B must get the independent grant's own colour"
    );
    assert_eq!(
        chosen_colors(&runner, aura),
        vec![ManaColor::Blue, ManaColor::Red],
        "reach-guard: the object must genuinely hold two answers here"
    );
    // REACH-GUARD: bear A's earlier answer must still stand — phase 1's latch
    // is doing its half.
    assert!(has_protection_from(
        &runner.state().objects[&bear_a],
        ManaColor::Blue
    ));

    // THE ASSERTION THAT FLIPS: activate ability 0 ({1}, anaphoric) AGAIN, on
    // bear C, with a second (unrelated) answer now on the object.
    let act0_again = runner.activate(aura, 0).target_object(bear_c).resolve();
    assert!(
        matches!(act0_again.final_waiting_for(), WaitingFor::Priority { .. }),
        "the anaphoric grant must STILL raise no prompt of its own, got {:?}",
        act0_again.final_waiting_for()
    );
    assert!(
        has_protection_from(&runner.state().objects[&bear_c], ManaColor::Blue),
        "CR 607.2d: the anaphoric ability must read only its OWN supplier's choice, not the \
         object's most recent answer: {:?}",
        runner.state().objects[&bear_c].keywords
    );
}
