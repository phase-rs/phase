//! CR 722.3c + CR 704.5e: the copy of a prepare spell that a prepared
//! permanent's controller creates in exile remains there for as long as the
//! prepared permanent remains on the battlefield and has the prepared
//! designation. That is an explicit exception to the CR 704.5e cease-to-exist
//! state-based action for copies of cards outside the stack and battlefield.
//!
//! Every fixture prepares two permanents (A and B) through the production
//! activated-ability resolver, mutates only A's authority, and checks that
//! B's linked copy is untouched. The fixtures are db-independent: the prepare
//! face is hand-built as in `fra_bloodline_recollector.rs`.

use engine::ai_support::legal_actions;
use engine::game::game_object::{BackFaceData, PhaseOutCause};
use engine::game::phasing::{phase_in_object, phase_out_object};
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Ability index 0 prepares the creature, index 1 unprepares it.
const PREPARE_SELF: &str =
    "Pay 1 life: This creature becomes prepared.\nPay 1 life: This creature becomes unprepared.";

struct PreparedPair {
    runner: GameRunner,
    a: ObjectId,
    b: ObjectId,
    copy_a: ObjectId,
    copy_b: ObjectId,
    removal: ObjectId,
}

/// A zero-cost sorcery prepare face ("Draw a card") so castability never
/// depends on mana.
fn prepare_face(name: &str) -> BackFaceData {
    let mut card_types = CardType::default();
    card_types.core_types.push(CoreType::Sorcery);
    BackFaceData {
        layout_kind: Some(LayoutKind::Prepare),
        name: name.to_string(),
        card_types,
        mana_cost: ManaCost::zero(),
        abilities: vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )],
        ..BackFaceData::default()
    }
}

fn linked_copies(state: &GameState, source: ObjectId) -> Vec<ObjectId> {
    state
        .objects
        .values()
        .filter(|object| object.prepared_copy_source == Some(source))
        .map(|object| object.id)
        .collect()
}

fn assert_copy_present(state: &GameState, copy: ObjectId, source: ObjectId) {
    let object = state
        .objects
        .get(&copy)
        .unwrap_or_else(|| panic!("linked copy {copy:?} of {source:?} must exist"));
    assert_eq!(object.zone, Zone::Exile);
    assert!(state.exile.contains(&copy));
    assert_eq!(object.prepared_copy_source, Some(source));
}

fn assert_copy_gone(state: &GameState, copy: ObjectId) {
    assert!(
        !state.objects.contains_key(&copy),
        "copy {copy:?} must have ceased to exist"
    );
    assert!(!state.exile.contains(&copy));
}

fn assert_parsed_prepare_lines(runner: &GameRunner, source: ObjectId) {
    let object = &runner.state().objects[&source];
    assert!(
        matches!(
            object.abilities[0].effect.as_ref(),
            Effect::BecomePrepared {
                target: TargetFilter::SelfRef
            }
        ),
        "ability 0 must be a self-ref BecomePrepared, got {:?}",
        object.abilities[0].effect
    );
    assert!(
        matches!(
            object.abilities[1].effect.as_ref(),
            Effect::BecomeUnprepared {
                target: TargetFilter::SelfRef
            }
        ),
        "ability 1 must be a self-ref BecomeUnprepared, got {:?}",
        object.abilities[1].effect
    );
    assert!(
        !serde_json::to_string(object.abilities.as_ref())
            .expect("serialize abilities")
            .contains("\"Unimplemented\""),
        "fixture abilities must contain no Unimplemented node"
    );
}

/// Prepares A and B through the production activated-ability resolver (which
/// runs SBA at the priority boundary) and asserts both linked copies exist.
fn prepared_pair() -> PreparedPair {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario
        .add_creature(P0, "Prepared A", 2, 2)
        .from_oracle_text(PREPARE_SELF)
        .id();
    let b = scenario
        .add_creature(P0, "Prepared B", 2, 2)
        .from_oracle_text(PREPARE_SELF)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Linked Copy Removal", true, "Destroy target creature.")
        .id();
    scenario.with_library_top(P0, &["Library One", "Library Two", "Library Three"]);

    let mut runner = scenario.build();
    for (source, face) in [(a, "Prepare Face A"), (b, "Prepare Face B")] {
        runner
            .state_mut()
            .objects
            .get_mut(&source)
            .unwrap()
            .back_face = Some(prepare_face(face));
    }

    for source in [a, b] {
        assert_parsed_prepare_lines(&runner, source);
        assert!(runner.state().objects[&source].prepared.is_none());
    }
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|object| object.prepared_copy_source.is_none()),
        "no linked copy exists before anything is prepared"
    );

    runner.activate(a, 0).resolve();
    runner.activate(b, 0).resolve();

    // Positive reach guard: both permanents are prepared and each owns exactly
    // one linked exile copy after the priority-boundary SBA check ran.
    let mut copies = Vec::new();
    for source in [a, b] {
        let state = runner.state();
        assert!(
            state.objects[&source].prepared.is_some(),
            "{source:?} must be prepared"
        );
        let linked = linked_copies(state, source);
        assert_eq!(
            linked.len(),
            1,
            "CR 722.3c: prepared {source:?} must keep exactly one linked copy in exile, found {linked:?}"
        );
        let copy = linked[0];
        let object = &state.objects[&copy];
        assert_eq!(object.zone, Zone::Exile);
        assert!(object.is_copy);
        assert!(!object.is_token);
        assert!(state.exile.contains(&copy));
        copies.push(copy);
    }
    assert_ne!(copies[0], copies[1]);

    PreparedPair {
        runner,
        a,
        b,
        copy_a: copies[0],
        copy_b: copies[1],
        removal,
    }
}

fn run_sba(runner: &mut GameRunner) {
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
}

fn has_cast_prepared_copy(state: &GameState, source: ObjectId) -> bool {
    legal_actions(state)
        .iter()
        .any(|action| matches!(action, GameAction::CastPreparedCopy { source: s } if *s == source))
}

fn has_cast_spell_for(state: &GameState, object: ObjectId) -> bool {
    legal_actions(state).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == object),
    )
}

#[test]
fn linked_copies_survive_sba_while_prepared_on_battlefield() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();

    // An unlinked copy of a card in exile: CR 704.5e still applies to it.
    let sentinel = create_object(
        runner.state_mut(),
        CardId(9_999),
        P0,
        "Sentinel".to_string(),
        Zone::Exile,
    );
    {
        let object = runner.state_mut().objects.get_mut(&sentinel).unwrap();
        object.is_copy = true;
        object.is_token = false;
    }
    assert!(runner.state().exile.contains(&sentinel));

    for _ in 0..2 {
        run_sba(&mut runner);
        assert_copy_gone(runner.state(), sentinel);
        assert_copy_present(runner.state(), copy_a, a);
        assert_copy_present(runner.state(), copy_b, b);
    }
}

#[test]
fn linked_copy_ceases_when_its_permanent_is_destroyed() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        removal,
    } = prepared_pair();

    runner.cast(removal).target_object(a).resolve();

    let state = runner.state();
    assert_eq!(state.objects[&a].zone, Zone::Graveyard);
    assert_copy_gone(state, copy_a);
    assert_copy_present(state, copy_b, b);
    assert!(state.objects[&b].prepared.is_some());
}

#[test]
fn linked_copy_ceases_when_its_permanent_is_unprepared() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();

    runner.activate(b, 1).resolve();

    let state = runner.state();
    assert!(state.objects[&b].prepared.is_none());
    assert_copy_gone(state, copy_b);
    assert_copy_present(state, copy_a, a);
    assert!(state.objects[&a].prepared.is_some());
}

#[test]
fn hostile_dangling_designation_releases_the_copy() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();

    // Bypass `unprepare_object` so only the SBA predicate can act.
    runner.state_mut().objects.get_mut(&a).unwrap().prepared = None;
    assert_copy_present(runner.state(), copy_a, a);

    run_sba(&mut runner);
    assert_copy_gone(runner.state(), copy_a);
    assert_copy_present(runner.state(), copy_b, b);
}

#[test]
fn hostile_source_off_battlefield_releases_the_copy() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();

    // Overwrite the zone in place so the eager battlefield-exit cleanup in
    // `move_to_zone` is bypassed.
    runner.state_mut().objects.get_mut(&a).unwrap().zone = Zone::Graveyard;
    assert!(runner.state().objects[&a].prepared.is_some());
    assert_copy_present(runner.state(), copy_a, a);

    run_sba(&mut runner);
    assert_copy_gone(runner.state(), copy_a);
    assert_copy_present(runner.state(), copy_b, b);
}

#[test]
fn hostile_phantom_source_releases_the_copy() {
    let PreparedPair {
        mut runner,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();

    let phantom = ObjectId(runner.state().next_object_id + 1_000);
    assert!(!runner.state().objects.contains_key(&phantom));
    runner
        .state_mut()
        .objects
        .get_mut(&copy_a)
        .unwrap()
        .prepared_copy_source = Some(phantom);

    run_sba(&mut runner);
    assert_copy_gone(runner.state(), copy_a);
    assert_copy_present(runner.state(), copy_b, b);
}

#[test]
fn hostile_copy_displaced_out_of_exile_ceases() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();

    move_to_zone(runner.state_mut(), copy_a, Zone::Graveyard, &mut Vec::new());
    {
        let state = runner.state();
        let copy = &state.objects[&copy_a];
        assert_eq!(copy.zone, Zone::Graveyard);
        assert_eq!(
            copy.prepared_copy_source,
            Some(a),
            "the link survives the move, so only the exile-zone conjunct releases it"
        );
        assert_eq!(state.objects[&a].zone, Zone::Battlefield);
        assert!(state.objects[&a].prepared.is_some());
    }

    run_sba(&mut runner);
    assert_copy_gone(runner.state(), copy_a);
    assert!(!runner
        .state()
        .players
        .iter()
        .any(|player| player.graveyard.contains(&copy_a)));
    assert_copy_present(runner.state(), copy_b, b);
}

#[test]
fn hostile_phased_out_source_releases_the_copy_and_cannot_cast() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();

    let phased = phase_out_object(
        runner.state_mut(),
        a,
        PhaseOutCause::Directly,
        &mut Vec::new(),
    );
    assert!(phased.contains(&a), "phase-out reach guard");
    {
        let state = runner.state();
        let source = &state.objects[&a];
        assert!(source.is_phased_out());
        assert_eq!(source.zone, Zone::Battlefield);
        assert!(source.prepared.is_some());
        assert_copy_present(state, copy_a, a);
        assert_copy_present(state, copy_b, b);
    }

    // CR 702.26b/d: a phased-out permanent is treated as though it isn't on
    // the battlefield, so its copy no longer has a prepared permanent to stay for.
    run_sba(&mut runner);
    assert_copy_gone(runner.state(), copy_a);
    assert_copy_present(runner.state(), copy_b, b);

    assert!(
        has_cast_prepared_copy(runner.state(), b),
        "the phased-in prepared permanent keeps its cast action"
    );
    assert!(
        !has_cast_prepared_copy(runner.state(), a),
        "a phased-out prepared permanent cannot cast its copy"
    );
    assert!(runner
        .act(GameAction::CastPreparedCopy { source: a })
        .is_err());
    assert!(
        linked_copies(runner.state(), a).is_empty(),
        "a rejected cast must not resurrect the copy"
    );

    let phased_in = phase_in_object(runner.state_mut(), a, &mut Vec::new());
    assert!(phased_in.contains(&a));
    assert!(!runner.state().objects[&a].is_phased_out());
    assert!(runner.state().objects[&a].prepared.is_some());
    assert!(has_cast_prepared_copy(runner.state(), a));
}

#[test]
fn linked_copy_is_castable_only_through_cast_prepared_copy() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));

    // Paired positive reach guard.
    assert!(has_cast_prepared_copy(runner.state(), a));
    assert!(has_cast_prepared_copy(runner.state(), b));

    assert!(
        !has_cast_spell_for(runner.state(), copy_a),
        "the linked copy must not be offered as a generic exile cast"
    );
    assert!(!has_cast_spell_for(runner.state(), copy_b));

    let card_id = runner.state().objects[&copy_a].card_id;
    let forged = runner.act(GameAction::CastSpell {
        object_id: copy_a,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        forged.is_err(),
        "CR 722.3c: a forged generic CastSpell of the linked copy must be rejected"
    );
    assert!(runner.state().objects[&a].prepared.is_some());
    assert_copy_present(runner.state(), copy_a, a);
    assert!(runner.state().stack.is_empty());

    // Instrument-reach control: the same object with its link removed is
    // offered by the generic exile alt-cost enumeration.
    runner
        .state_mut()
        .objects
        .get_mut(&copy_b)
        .unwrap()
        .prepared_copy_source = None;
    assert!(
        has_cast_spell_for(runner.state(), copy_b),
        "control: the unlinked copy is reached by the generic exile enumeration"
    );
}

#[test]
fn persisted_linked_copy_is_the_one_cast_and_no_second_copy_appears() {
    let PreparedPair {
        mut runner,
        a,
        b,
        copy_a,
        copy_b,
        ..
    } = prepared_pair();
    let hand_before = runner.state().players[0].hand.len();

    runner
        .act(GameAction::CastPreparedCopy { source: a })
        .expect("CastPreparedCopy must start the cast");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting state during prepared cast: {other:?}"),
        }
    }

    {
        let state = runner.state();
        assert!(
            state.stack.iter().any(|entry| entry.id == copy_a),
            "the persisted linked copy is the object cast"
        );
        assert_eq!(state.objects[&copy_a].zone, Zone::Stack);
        assert!(
            state.objects[&a].prepared.is_none(),
            "CR 722.3c + CR 601.2i: casting the copy unprepares its permanent"
        );
        assert!(
            !state
                .objects
                .values()
                .any(|object| object.zone == Zone::Exile && object.prepared_copy_source == Some(a)),
            "no second linked copy appears in exile"
        );
        assert!(state.objects[&b].prepared.is_some());
        assert_copy_present(state, copy_b, b);
    }

    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.players[0].hand.len(), hand_before + 1);
    assert!(
        !state.objects.contains_key(&copy_a),
        "CR 704.5e: the resolved copy of a spell ceases to exist"
    );
    assert_copy_present(state, copy_b, b);
}
