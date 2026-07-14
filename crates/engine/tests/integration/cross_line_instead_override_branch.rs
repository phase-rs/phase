//! CR 614.6 + CR 614.15 — a CROSS-LINE "instead" self-replacement override is a
//! BRANCH of the ability it replaces, never an independent sibling ability.
//!
//! CR 614.6:  "If an event is replaced, it never happens. A modified event
//!             occurs instead."
//! CR 614.15: self-replacement effects "replace part or all of that spell or
//!             ability's own effect(s) … the text can be a separate ability,
//!             particularly when preceded by an ability word."
//!
//! CR 614.15 is the authority for this class: the override is printed as its own
//! ability-word LINE ("Corrupted — …", "Spell mastery — …"), but it replaces the
//! PREVIOUS printed line's effect. The parser has a cross-line binder for exactly
//! this (oracle.rs), but its gate recognized only WHOLE-clause overrides (a bare
//! trailing "instead"). It did not recognize the CR 614.15 PARTIAL forms —
//! "… instead of <N>" / "… instead of <phrase>" — nor an override whose condition
//! failed to lower. Those lines fell through and were emitted as INDEPENDENT
//! top-level abilities, so the engine performed the base effect AND the override.
//!
//! Oracle text below is verbatim from the full-pool export (never a paraphrase):
//! a paraphrase can take a different parser branch and leave the real card broken.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::AbilityCondition;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

/// Anoint with Affliction {1}{B} — Instant. Verbatim Oracle text.
const ANOINT: &str = "Exile target creature if it has mana value 3 or less.\nCorrupted — Exile that creature instead if its controller has three or more poison counters.";

/// Gather the Pack {1}{G} — Sorcery. Verbatim Oracle text.
const GATHER_THE_PACK: &str = "Reveal the top five cards of your library. You may put a creature card from among them into your hand. Put the rest into your graveyard.\nSpell mastery — If there are two or more instant and/or sorcery cards in your graveyard, put up to two creature cards from among the revealed cards into your hand instead of one.";

/// CR 614.6 DOUBLE-EXECUTION WITNESS (board state).
///
/// Anoint with Affliction exiles the target ONLY if its mana value is 3 or less.
/// The "Corrupted —" line replaces that conditional exile with an unconditional
/// one, but ONLY while the target's controller has three or more poison counters.
///
/// Here the target has mana value 5 and its controller has ZERO poison counters,
/// so BOTH the printed condition (CR 608.2c) and the Corrupted override are false:
/// the creature must survive.
///
/// RED before the fix: the "Corrupted —" line was emitted as an INDEPENDENT second
/// top-level ability — `ChangeZone { destination: Exile, target: ParentTarget }`
/// with `condition: None`, because its condition ("its controller has three or
/// more poison counters") never lowered. The engine therefore exiled the creature
/// UNCONDITIONALLY, ignoring both the mana-value gate and the poison gate.
/// This assertion flips the moment that sibling is reintroduced.
#[test]
fn anoint_with_affliction_cross_line_override_does_not_exile_unconditionally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Anoint with Affliction", true, ANOINT)
        .id();
    // Mana value 5 — OUTSIDE the printed "mana value 3 or less" gate.
    let victim = {
        let mut b = scenario.add_creature(P1, "Serra Angel", 4, 4);
        b.with_mana_cost(ManaCost::generic(5));
        b.id()
    };
    let mut runner = scenario.build();

    // Control: the target's controller has no poison counters, so the Corrupted
    // override cannot apply either.
    assert_eq!(
        poison_counters(&runner, P1),
        0,
        "precondition: the Corrupted override must be OFF for this witness to \
         discriminate — if P1 had 3+ poison, exiling would be correct and the \
         test could not fail"
    );

    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();

    outcome.assert_zone(&[victim], Zone::Battlefield);
}

fn poison_counters(runner: &GameRunner, player: PlayerId) -> u32 {
    runner.state().players[player.0 as usize].poison_counters
}

/// CR 614.6 REGRESSION GUARD — the override's OWN tail must not leak into the
/// not-replaced branch.
///
/// Precognitive Perception {3}{U}{U} — Instant:
///   "Draw three cards.
///    Addendum — If you cast this spell during your main phase, instead scry 3,
///    then draw three cards."
///
/// The override body is a TWO-clause chain ("scry 3, then draw three cards") and
/// BOTH clauses belong to the replacement. When the Addendum condition is false,
/// the printed "Draw three cards" runs and NOTHING of the override may run.
///
/// This face is the one the full-pool ledger surfaced that this unit did not
/// predict: widening the condition vocabulary made the line take the
/// strip-the-body path, which moved the override's second clause out from under
/// the condition the chain had been stamping on it. The engine runs an unswapped
/// `ConditionInstead` sub's `sub_ability` tail when it has no `else_ability`
/// (effects/mod.rs — a clause printed AFTER an "instead" sentence is an
/// independent instruction), so an unguarded tail here would draw three MORE
/// cards outside your main phase. Six cards off a card that draws three.
///
/// Cast in the UPKEEP (not a main phase) — exactly three cards, never six.
#[test]
fn precognitive_perception_override_tail_does_not_run_when_addendum_is_false() {
    const PRECOG: &str = "Draw three cards.\nAddendum — If you cast this spell during your main phase, instead scry 3, then draw three cards.";

    let mut scenario = GameScenario::new();
    // NOT a main phase — the Addendum condition is false.
    scenario.at_phase(Phase::Upkeep);
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Precognitive Perception", true, PRECOG)
        .id();
    for i in 0..12 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    outcome.assert_hand_drawn(P0, 3);
}

/// SHAPE (CR 614.15 PARTIAL replacement): "… instead of one" replaces only the
/// COUNT of the printed Dig — not the whole instruction.
///
/// RED before the fix: the "Spell mastery —" line matched none of the cross-line
/// gate's whole-clause forms (it ends "instead of one", not a bare "instead"), so it
/// was published as a SECOND top-level ability — and its body, parsed in isolation,
/// had lowered to `ChangeZone { destination: Hand, target: Creature, up_to,
/// multi_target: 0..2 }`: a targeted BOUNCE of up to two creatures off the
/// battlefield. With spell mastery on, casting Gather the Pack both dug AND offered
/// to bounce two creatures — an effect the card does not have.
///
/// The override must instead bind as an alternative `Dig` that keeps the printed
/// Dig's source (top five), its `reveal`, and its rest-to-graveyard rider, changing
/// only the keep count (1 -> 2). Asserting the effect KIND is what discriminates a
/// faithful rebuild from a naive whole-effect swap that would silently drop the
/// reveal and the rest-to-graveyard.
#[test]
fn gather_the_pack_binds_the_partial_override_as_an_alternative_dig() {
    use engine::types::ability::Effect;

    let parsed = parse_oracle_text(
        GATHER_THE_PACK,
        "Gather the Pack",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    assert_eq!(
        parsed.abilities.len(),
        1,
        "CR 614.6: the spell-mastery override replaces the printed Dig's count; it \
         must be bound INTO that Dig, never published as a second ability (which \
         resolved as a stray creature bounce). Got: {:#?}",
        parsed.abilities
    );

    let base = &parsed.abilities[0];
    let Effect::Dig {
        keep_count: base_keep,
        count: base_count,
        reveal: base_reveal,
        rest_destination: base_rest,
        ..
    } = base.effect.as_ref()
    else {
        panic!("base must remain the printed Dig, got {:?}", base.effect);
    };
    assert_eq!(*base_keep, Some(1), "printed Dig keeps one creature card");
    assert!(*base_reveal, "printed Dig reveals");

    let sub = base
        .sub_ability
        .as_ref()
        .expect("the override must be bound as the Dig's sub_ability");
    assert!(
        matches!(
            sub.condition,
            Some(AbilityCondition::ConditionInstead { .. })
        ),
        "CR 614.1a: the override must be a ConditionInstead branch so the runtime \
         SWAPS the Dig rather than running both. Got: {:?}",
        sub.condition
    );

    // CR 614.15: the override replaces only the PART it names ("instead of one").
    // Everything else about the Dig — source size, reveal, rest destination — must
    // survive the rebuild, which is exactly what a bare ChangeZone would have lost.
    let Effect::Dig {
        keep_count: alt_keep,
        count: alt_count,
        reveal: alt_reveal,
        rest_destination: alt_rest,
        ..
    } = sub.effect.as_ref()
    else {
        panic!(
            "the alternative must be a full Dig, not a bare ChangeZone (that drops \
             the reveal, the library source and the rest-to-graveyard rider). Got: {:?}",
            sub.effect
        );
    };
    assert_eq!(
        *alt_keep,
        Some(2),
        "spell mastery keeps TWO creature cards, not one"
    );
    assert_eq!(
        alt_count, base_count,
        "the alternative reuses the printed Dig's source (top five)"
    );
    assert_eq!(alt_reveal, base_reveal, "the alternative still reveals");
    assert_eq!(
        alt_rest, base_rest,
        "the alternative still puts the rest into the graveyard"
    );
}

/// SHAPE (CR 614.15 + CR 614.6): the cross-line override must be BOUND to the
/// ability it replaces — one top-level ability whose `sub_ability` is the override,
/// gated by `ConditionInstead` — never a second, independent top-level ability.
///
/// This discriminates the two ways the runtime witness above could go green:
///   * BOUND (what we want): one ability, override as a ConditionInstead branch.
///   * merely NEUTERED: two abilities, the second an inert `Unimplemented`.
///
/// Without this, a regression that degraded the branch back to an honest-red
/// sibling would still pass the runtime assertion.
#[test]
fn anoint_with_affliction_binds_the_cross_line_override_as_a_branch() {
    let parsed = parse_oracle_text(
        ANOINT,
        "Anoint with Affliction",
        &[],
        &["Instant".to_string()],
        &[],
    );

    assert_eq!(
        parsed.abilities.len(),
        1,
        "CR 614.6: the \"Corrupted —\" override replaces the printed exile; it must \
         be bound INTO that ability, not published as a second independent one. \
         Two top-level abilities = the engine performs both. Got: {:#?}",
        parsed.abilities
    );

    let base = &parsed.abilities[0];
    let sub = base
        .sub_ability
        .as_ref()
        .expect("the override must be bound as the base ability's sub_ability");
    assert!(
        matches!(
            sub.condition,
            Some(AbilityCondition::ConditionInstead { .. })
        ),
        "CR 614.1a: the bound override must carry ConditionInstead so the runtime \
         SWAPS the base effect rather than running both. Got: {:?}",
        sub.condition
    );
}
