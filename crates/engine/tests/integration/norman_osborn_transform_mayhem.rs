//! CR 702.187a + CR 701.27a: the printed Norman Osborn // Green Goblin faces,
//! driven the way a game drives them.
//!
//! `issue_2354_norman_osborn_mayhem.rs` covers the Mayhem grant and its
//! availability gate, but builds Green Goblin's face from an isolated Oracle
//! line, so three things it cannot reach are covered here: the card-data modal
//! face linkage, the printed `{1}{U}{B}{R}` transform ability that installs
//! Goblin Formula, and the back face's second static — "Spells you cast from
//! your graveyard cost {2} less to cast". Other fixtures carry that static
//! (`issue_3874_emet_selch_optional_decline.rs`, `do_this_only_once_each_turn.rs`),
//! and the latter does exercise it — but only on ORDINARY casts. Its
//! interaction with an alternative cost (CR 601.2f), which is what a granted
//! mayhem cast is, is untested. The last test runs the whole card —
//! Norman Osborn connives off combat damage, transforms, and casts what he
//! discarded.

use crate::support::shared_card_db;
use engine::game::casting::display_spell_cost;
use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::keywords::effective_mayhem_cost;
use engine::game::layers::evaluate_layers;
use engine::game::printed_cards::{apply_card_face_to_object, back_face_for_card_face};
use engine::game::restrictions::{record_card_discarded, record_discard};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::{casting::can_cast_object_now, casting::spell_objects_available_to_cast};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Norman Osborn's printed activated ability is his only one.
const TRANSFORM_ABILITY: usize = 0;

/// CR 712.1: install the REAL printed front face and its modal back face, so
/// the transform swaps in Green Goblin's printed statics rather than a
/// hand-built stand-in.
fn install_printed_norman(runner: &mut GameRunner, id: ObjectId) {
    let db = shared_card_db().expect("caller checked the export is present");
    let face = db
        .get_face_by_name("norman osborn")
        .expect("Norman Osborn is in the card-data export");
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    apply_card_face_to_object(obj, face);
    obj.back_face = back_face_for_card_face(db, face);
}

fn mana(runner: &mut GameRunner, units: Vec<ManaUnit>) {
    for unit in units {
        runner
            .state_mut()
            .add_mana_to_pool(P0, unit)
            .expect("staged mana enters P0's pool");
    }
}

fn unit(kind: ManaType) -> ManaUnit {
    ManaUnit::new(kind, ObjectId(0), false, vec![])
}

/// {1}{U}{B}{R} — the printed transform cost.
fn transform_mana() -> Vec<ManaUnit> {
    vec![
        unit(ManaType::Colorless),
        unit(ManaType::Blue),
        unit(ManaType::Black),
        unit(ManaType::Red),
    ]
}

/// CR 701.27a + CR 602.2b: turn the permanent over by ACTIVATING its printed
/// `{1}{U}{B}{R}: Transform` ability, so the cost is really paid and the
/// Transform effect resolves off the stack rather than through a direct call
/// to `transform_permanent`. Staging exactly the cost and asserting the pool
/// drains is what keeps the payment load-bearing; the `AsSorcery` restriction
/// is pinned separately, in the connive test's mid-combat probe.
fn transform_via_printed_ability(runner: &mut GameRunner, id: ObjectId) {
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the helper stages into an empty pool"
    );
    mana(runner, transform_mana());
    runner.activate(id, TRANSFORM_ABILITY).resolve();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "activating the printed ability must consume the staged {{1}}{{U}}{{B}}{{R}}"
    );
    assert_eq!(
        runner.state().objects[&id].name,
        "Green Goblin",
        "activating the printed transform ability must install the back face"
    );
}

fn bolt_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Red],
    }
}

/// The reduced cost: {1}{R} less Green Goblin's {2} floors the generic at zero
/// (CR 601.2f).
fn reduced_bolt_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Red],
    }
}

struct Fixture {
    runner: GameRunner,
    osborn: ObjectId,
    bolt: ObjectId,
}

/// Norman Osborn on the battlefield and a {1}{R} nonland card to put in the
/// graveyard. No mana is staged: each test adds exactly what it means to test.
fn fixture() -> Option<Fixture> {
    shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let osborn = scenario.add_creature(P0, "Norman Osborn", 1, 1).id();
    let bolt = scenario
        .add_spell_to_hand(P0, "Mayhem Bolt", true)
        .with_mana_cost(bolt_cost())
        .id();

    let mut runner = scenario.build();
    install_printed_norman(&mut runner, osborn);
    evaluate_layers(runner.state_mut());
    Some(Fixture {
        runner,
        osborn,
        bolt,
    })
}

fn to_graveyard(runner: &mut GameRunner, card: ObjectId) {
    engine::game::zones::move_to_zone(runner.state_mut(), card, Zone::Graveyard, &mut Vec::new());
    evaluate_layers(runner.state_mut());
}

/// Cast `card` from the graveyard and assert the engine elected Mayhem — the
/// production path, not a castability predicate.
fn cast_and_assert_mayhem(runner: &mut GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the granted mayhem cast is legal and affordable");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "auto-payment should land on priority, got {:?}",
        result.waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1, "the spell is on the stack");
    match &runner.state().stack[0].kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => assert_eq!(
            *casting_variant,
            CastingVariant::Mayhem,
            "the engine must elect the granted Mayhem variant"
        ),
        other => panic!("expected a spell on the stack, got {other:?}"),
    }
}

/// CR 702.187a + CR 613.1f: the grant lives on the BACK face, so the front face
/// must not hand out mayhem, and activating the printed transform must start
/// it — including for a card that was already in the graveyard.
#[test]
fn printed_transform_ability_installs_the_graveyard_mayhem_grant() {
    let Some(Fixture {
        mut runner,
        osborn,
        bolt,
    }) = fixture()
    else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };

    to_graveyard(&mut runner, bolt);
    assert!(
        effective_mayhem_cost(runner.state(), bolt).is_none(),
        "the Norman Osborn face has no graveyard grant"
    );

    transform_via_printed_ability(&mut runner, osborn);

    assert!(
        effective_mayhem_cost(runner.state(), bolt).is_some(),
        "Goblin Formula must reach a card already in the graveyard"
    );
}

/// CR 702.187b + CR 601.2f: Green Goblin reads "The mayhem cost is equal to its
/// mana cost", so `ManaCost::SelfManaCost` must resolve against the recipient —
/// not against Green Goblin's own {1}{U}{B}{R}, and not {0}.
#[test]
fn granted_mayhem_cost_is_the_recipients_own_mana_cost() {
    let Some(Fixture {
        mut runner,
        osborn,
        bolt,
    }) = fixture()
    else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };

    transform_via_printed_ability(&mut runner, osborn);
    to_graveyard(&mut runner, bolt);

    assert_eq!(
        effective_mayhem_cost(runner.state(), bolt),
        Some(bolt_cost()),
        "the granted mayhem cost is the card's own mana cost"
    );
}

/// CR 702.187b: the grant is unconditional, the AVAILABILITY is not. Asserted
/// as a flip inside one test so the negative half cannot pass vacuously: the
/// same object, same pool and same board become castable on the discard mark
/// alone. The pool covers the UNREDUCED {1}{R} so a regression in Green
/// Goblin's cost reduction cannot masquerade as the gate.
#[test]
fn mayhem_availability_flips_on_the_discard_this_turn_mark() {
    let Some(Fixture {
        mut runner,
        osborn,
        bolt,
    }) = fixture()
    else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };

    transform_via_printed_ability(&mut runner, osborn);
    to_graveyard(&mut runner, bolt);
    mana(
        &mut runner,
        vec![unit(ManaType::Colorless), unit(ManaType::Red)],
    );

    assert!(
        effective_mayhem_cost(runner.state(), bolt).is_some(),
        "reach guard: the grant is live, so only the discard gate is in play"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, bolt),
        "no discard this turn means no mayhem cast"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&bolt),
        "legal actions must not offer the cast before the gate opens"
    );
    let card_id = runner.state().objects[&bolt].card_id;
    assert!(
        runner
            .act(GameAction::CastSpell {
                object_id: bolt,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "the action itself must be refused, not merely hidden from legal actions"
    );

    record_discard(runner.state_mut(), P0);
    record_card_discarded(runner.state_mut(), bolt);
    evaluate_layers(runner.state_mut());

    assert!(
        can_cast_object_now(runner.state(), P0, bolt),
        "the discard mark alone must open the mayhem cast"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&bolt),
        "legal actions must offer the cast once the gate opens"
    );
}

/// CR 601.2f: Green Goblin's second printed static — "Spells you cast from your
/// graveyard cost {2} less to cast" — applies to the mayhem cast. {1}{R} less
/// {2} floors the generic at zero, leaving {R}, so a pool of exactly {R} is
/// affordable ONLY while the reduction applies. The graveyard is a castable
/// zone on this board only through mayhem, so the completed cast pins both the
/// variant and the reduced payment.
#[test]
fn graveyard_cost_reduction_applies_to_the_granted_mayhem_cast() {
    let Some(Fixture {
        mut runner,
        osborn,
        bolt,
    }) = fixture()
    else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };

    transform_via_printed_ability(&mut runner, osborn);
    to_graveyard(&mut runner, bolt);
    record_discard(runner.state_mut(), P0);
    record_card_discarded(runner.state_mut(), bolt);
    evaluate_layers(runner.state_mut());

    assert_eq!(
        display_spell_cost(runner.state(), P0, bolt),
        Some(reduced_bolt_cost()),
        "the {{2}} graveyard reduction must come off the {{1}}{{R}} mayhem cost"
    );

    mana(&mut runner, vec![unit(ManaType::Red)]);
    cast_and_assert_mayhem(&mut runner, bolt);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "{{R}} alone paid the reduced cost"
    );
}

/// CR 701.50a + CR 702.187b: the whole card, in printed order. Norman Osborn is
/// unblockable, connives off combat damage (CR 701.50a: draw, discard, and a
/// +1/+1 counter for the nonland discard), and the discarded card is cast from
/// the graveyard once he transforms in the post-combat main phase. This is the
/// one test that drives the printed connive trigger and the real discard
/// pipeline rather than stamping the mark, and it pins the `AsSorcery`
/// restriction by failing to transform mid-combat.
#[test]
fn front_face_connive_feeds_the_graveyard_grant_after_transforming() {
    let Some(Fixture {
        mut runner,
        osborn,
        bolt,
    }) = fixture()
    else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };

    // Connive draws, so the card must be on top of the library, not in hand:
    // an empty hand then forces the auto-discard arm (CR 701.50a).
    engine::game::zones::move_to_library_position(runner.state_mut(), bolt, true, &mut Vec::new());
    evaluate_layers(runner.state_mut());

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(osborn, AttackTarget::Player(P1))])
        .expect("Norman Osborn can't be blocked and attacks P1");

    // CR 602.5d: "Activate only as a sorcery" follows sorcery timing, so the
    // printed ability must refuse here even with its cost fully staged — the
    // full stage is what proves the refusal is the timing gate and not
    // affordability. CR 500.5: the pool empties as the step ends, leaving the
    // post-combat activation clean.
    mana(&mut runner, transform_mana());
    let refusal = runner
        .act(GameAction::ActivateAbility {
            source_id: osborn,
            ability_index: TRANSFORM_ABILITY,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect_err("CR 602.5d: the transform ability must refuse activation during combat");
    assert!(
        matches!(&refusal, EngineError::ActionNotAllowed(msg) if msg.contains("AsSorcery")),
        "CR 602.5d: expected the sorcery-timing refusal, got {refusal:?}"
    );
    assert_eq!(
        runner.state().objects[&osborn].name,
        "Norman Osborn",
        "the refused activation must leave the front face up"
    );

    runner.combat_damage();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "connive drew the card and discarded it"
    );
    assert_eq!(
        runner.state().objects[&osborn]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "CR 701.50a: a nonland card was discarded, so connive resolved and left a counter"
    );

    runner.advance_to_phase(Phase::PostCombatMain);
    transform_via_printed_ability(&mut runner, osborn);

    assert_eq!(
        effective_mayhem_cost(runner.state(), bolt),
        Some(bolt_cost()),
        "the connived-away card carries Goblin Formula's granted mayhem cost"
    );
    mana(
        &mut runner,
        vec![unit(ManaType::Colorless), unit(ManaType::Red)],
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&bolt),
        "legal actions must offer the connived card's mayhem cast"
    );
    cast_and_assert_mayhem(&mut runner, bolt);
}
