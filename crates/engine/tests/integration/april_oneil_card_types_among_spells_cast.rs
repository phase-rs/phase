//! April O'Neil, Hacktivist — "draw a card for each **card type among spells
//! you've cast this turn**" counts DISTINCT CARD TYPES over the per-turn cast
//! journal (CR 205.2a over CR 601.2a), not the number of spells cast.
//!
//! Pre-fix this bound `QuantityRef::SpellsCastThisTurn`, a raw count of cast
//! records, because `parse_spell_history_clause`'s terminal fallback claimed
//! any clause containing a cast verb phrase and silently discarded the
//! unconsumed "card type among" aggregation head.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{CardTypeSetSource, QuantityExpr, QuantityRef, TurnJournalKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Verbatim Scryfall Oracle text.
const APRIL_ONEIL: &str =
    "At the beginning of your end step, draw a card for each card type among spells \
you've cast this turn.";

const INSTANT_FILLER: &str = "Target player gains 1 life.";
const SORCERY_FILLER: &str = "Target player gains 2 life.";

fn mono(shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic: 0,
    }
}

fn pool(colored: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    colored
        .iter()
        .flat_map(|(kind, n)| vec![ManaUnit::new(*kind, ObjectId(0), false, vec![]); *n])
        .collect()
}

/// SHAPE half of the pair, and the reach guard for the runtime half below: the
/// trigger must parse to a distinct-card-type count over the CAST JOURNAL with
/// zero `Effect::Unimplemented`, so a "draws 2, not 3" assertion cannot pass
/// vacuously through an unimplemented early return.
#[test]
fn april_oneil_binds_card_types_over_the_cast_journal() {
    let parsed = engine::parser::parse_oracle_text(
        APRIL_ONEIL,
        "April O'Neil, Hacktivist",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "April O'Neil must parse cleanly, got {:?}",
        parsed.parse_warnings
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("April O'Neil must parse an end-step trigger");
    let execute = trigger
        .execute
        .as_deref()
        .expect("the end-step trigger must carry an executed ability");
    let engine::types::ability::Effect::Draw { count, .. } = execute.effect.as_ref() else {
        panic!("expected a Draw effect, got {:?}", execute.effect);
    };
    assert_eq!(
        *count,
        QuantityExpr::Ref {
            qty: QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::TurnJournal {
                    journal: TurnJournalKind::SpellsCast,
                    scope: engine::types::ability::CountScope::Controller,
                    filter: None,
                },
            },
        },
        "the draw count must be distinct CARD TYPES over the cast journal, \
         not a count of spells"
    );
}

/// RUNTIME half, driven through the REAL end-step trigger.
///
/// Cast THREE spells spanning TWO card types, then advance to the end step and
/// let April O'Neil's own triggered ability resolve. The 3-casts / 2-types split
/// is the discriminator: the pre-fix `SpellsCastThisTurn` reading draws 3.
///
/// The direct `resolve_quantity` probe is kept as a second, sharper assertion —
/// it pins the quantity in isolation — but it is NOT the primary check. On its
/// own it proves only that the resolver can count a hand-built AST; the drawn-card
/// assertion is what proves the parsed trigger actually reaches that resolver
/// with the source bound, so a break anywhere in trigger wiring is caught here
/// rather than passing green against a quantity nothing dispatches.
#[test]
fn april_oneil_counts_card_types_not_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Deeper than the trigger can draw, so a miscount reads as a wrong DRAW
    // COUNT rather than as an empty library.
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C", "Draw D"]);

    let april = scenario
        .add_creature_from_oracle(P0, "April O'Neil, Hacktivist", 2, 2, APRIL_ONEIL)
        .id();

    // Three casts, two card types: two instants and one sorcery.
    let instant_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Instant A", true, INSTANT_FILLER)
        .with_mana_cost(mono(ManaCostShard::Blue))
        .id();
    let instant_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Instant B", true, INSTANT_FILLER)
        .with_mana_cost(mono(ManaCostShard::Blue))
        .id();
    let sorcery = scenario
        .add_spell_to_hand_from_oracle(P0, "Sorcery A", false, SORCERY_FILLER)
        .with_mana_cost(mono(ManaCostShard::Red))
        .id();

    scenario.with_mana_pool(P0, pool(&[(ManaType::Blue, 2), (ManaType::Red, 1)]));
    let mut runner = scenario.build();

    for spell in [instant_a, instant_b, sorcery] {
        runner.cast(spell).target_player(P0).resolve();
    }

    // Reach guard: three cast records exist, so "2" below is a real
    // distinct-type answer and not an artifact of an empty journal.
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map_or(0, |records| records.len()),
        3,
        "reach guard: all three casts must be journaled"
    );

    let count = QuantityExpr::Ref {
        qty: QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::TurnJournal {
                journal: TurnJournalKind::SpellsCast,
                scope: engine::types::ability::CountScope::Controller,
                filter: None,
            },
        },
    };
    assert_eq!(
        engine::game::quantity::resolve_quantity(runner.state(), &count, P0, april),
        2,
        "three spells spanning two card types must count 2 (CR 205.2a), not 3"
    );

    // THE PRIMARY ASSERTION: April O'Neil's own trigger, through the production
    // pipeline. CR 513.1 — "at the beginning of your end step" triggers when the
    // end step begins; the trigger goes on the stack and resolves from there.
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    // CR 508.1: April O'Neil is a 2/2 and could attack, so the declare-attackers
    // turn-based action surfaces a prompt `advance_to_phase` cannot auto-pass —
    // it would stop in combat and leave the end step unreached. Cross it
    // explicitly rather than letting the phase helper stall.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach guard: the scenario must actually reach the end step, or the \
         draw assertion below passes vacuously by never triggering"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len() - hand_before,
        2,
        "April O'Neil must draw one card per CARD TYPE among the three spells \
         cast this turn — 2 (instant, sorcery), not 3 (the spell count)"
    );
}

/// Sibling: the journal's optional narrowing filter (Hurkyl's "noncreature
/// spells you've cast this turn"). `filter: None` counts every member; the
/// empty journal counts 0.
#[test]
fn a_filtered_cast_journal_narrows_the_type_tally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario.add_creature(P0, "Journal Reader", 1, 1).id();
    let instant = scenario
        .add_spell_to_hand_from_oracle(P0, "Instant A", true, INSTANT_FILLER)
        .with_mana_cost(mono(ManaCostShard::Blue))
        .id();
    // The EXCLUDED member for the filtered arm below — a second card type in the
    // journal that a narrowing filter must reject.
    let sorcery = scenario
        .add_spell_to_hand_from_oracle(P0, "Sorcery A", false, SORCERY_FILLER)
        .with_mana_cost(mono(ManaCostShard::Red))
        .id();

    scenario.with_mana_pool(P0, pool(&[(ManaType::Blue, 1), (ManaType::Red, 1)]));
    let mut runner = scenario.build();

    let unfiltered = |scope| QuantityExpr::Ref {
        qty: QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::TurnJournal {
                journal: TurnJournalKind::SpellsCast,
                scope,
                filter: None,
            },
        },
    };
    // CR 601.2a: the journal's optional narrowing filter, matched against each
    // record's cast-time snapshot (a resolved spell is no longer an object,
    // CR 400.7).
    let narrowed_to = |type_filter| QuantityExpr::Ref {
        qty: QuantityRef::DistinctCardTypes {
            source: CardTypeSetSource::TurnJournal {
                journal: TurnJournalKind::SpellsCast,
                scope: engine::types::ability::CountScope::Controller,
                filter: Some(engine::types::ability::TargetFilter::Typed(
                    engine::types::ability::TypedFilter::new(type_filter),
                )),
            },
        },
    };

    // Empty journal → 0.
    assert_eq!(
        engine::game::quantity::resolve_quantity(
            runner.state(),
            &unfiltered(engine::types::ability::CountScope::Controller),
            P0,
            source,
        ),
        0,
        "an empty journal contributes no card types"
    );

    runner.cast(instant).target_player(P0).resolve();

    assert_eq!(
        engine::game::quantity::resolve_quantity(
            runner.state(),
            &unfiltered(engine::types::ability::CountScope::Controller),
            P0,
            source,
        ),
        1,
        "one instant cast → one card type"
    );

    // CR 109.5: "you"/"your" on an object refers to that object's controller,
    // so the same journal read at `Opponents` scope sees the OTHER player's
    // journal, which is empty. (NOT CR 109.4, which says only stack/battlefield
    // objects HAVE a controller; that rule does not define the possessive.)
    assert_eq!(
        engine::game::quantity::resolve_quantity(
            runner.state(),
            &unfiltered(engine::types::ability::CountScope::Opponents),
            P0,
            source,
        ),
        0,
        "the opponents' journal is empty, so the scope axis is live"
    );

    // FILTERED ARM — what this test is named for. Put a SECOND card type in the
    // journal, then narrow to each one in turn. Without the second cast the
    // filter would be indistinguishable from `None`, which is why the exclusion
    // half is asserted alongside the inclusion half.
    runner.cast(sorcery).target_player(P0).resolve();
    assert_eq!(
        engine::game::quantity::resolve_quantity(
            runner.state(),
            &unfiltered(engine::types::ability::CountScope::Controller),
            P0,
            source,
        ),
        2,
        "reach guard: both casts are journaled, so the filter below has \
         something to exclude"
    );
    for (type_filter, label) in [
        (engine::types::ability::TypeFilter::Instant, "instant"),
        (engine::types::ability::TypeFilter::Sorcery, "sorcery"),
    ] {
        assert_eq!(
            engine::game::quantity::resolve_quantity(
                runner.state(),
                &narrowed_to(type_filter),
                P0,
                source,
            ),
            1,
            "narrowing to {label} must admit exactly that one record, not both"
        );
    }
}
