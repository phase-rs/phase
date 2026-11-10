//! CR 730.3 + CR 730.3c + CR 401.4: physical cards of a merged permanent put
//! into ONE library position at the same time are ordered by their owner.
//!
//! Two producers reach that placement without announcing a target set:
//!   * Avenging Angel's dies trigger ("you may put it on top of its owner's
//!     library") expands a recorded departure to the absorbed component
//!     (`merge::expand_returned_merge_components`).
//!   * Tel-Jilad Stylus ("{T}: Put target permanent you own on the bottom of its
//!     owner's library.") selects the merged permanent itself, which the merge
//!     primitive splits into components as it leaves (CR 730.3). Ownership, not
//!     control, is the target restriction.
//!
//! Each test drives public actions over real Oracle text and asserts the
//! REVERSE of the offered order, so an engine-default delivery order cannot
//! satisfy the row.

use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Avenging Angel's real Oracle text (verified against data/mtgjson/AtomicCards.json).
const AVENGING_ANGEL: &str =
    "Flying\nWhen this creature dies, you may put it on top of its owner's library.";
const MURDER: &str = "Destroy target creature.";
// Tel-Jilad Stylus's real Oracle text (verified against data/mtgjson/AtomicCards.json).
const TEL_JILAD_STYLUS: &str =
    "{T}: Put target permanent you own on the bottom of its owner's library.";

fn black_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

fn library(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

/// CR 401.4: the owner's arrangement prompt, when one is open.
fn arrangement_cards(runner: &GameRunner) -> Option<Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => Some(cards.clone()),
        _ => None,
    }
}

/// Answer any optional "you may" prompt (CR 608.2d) and pass priority until the
/// owner's arrangement opens, or the stack settles first.
fn accept_optional_until_arrangement(runner: &mut GameRunner) -> Option<Vec<ObjectId>> {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional dies trigger");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => return Some(cards.clone()),
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return None;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance the trigger");
            }
            other => panic!("unexpected prompt before the arrangement: {other:?}"),
        }
    }
    panic!("resolution did not settle within the safety bound");
}

/// Finish the resolution after an arrangement answer.
fn resolve_to_priority(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    assert!(
                        runner.state().pending_mass_library_order_choice.is_none(),
                        "the arrangement carrier is consumed by its completion"
                    );
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must finish the placement");
            }
            other => panic!("unexpected prompt after the arrangement: {other:?}"),
        }
    }
    panic!("the arrangement did not settle within the safety bound");
}

/// CR 730.3c: a died merged permanent's trigger acts on every physical card it
/// became. CR 401.4: their owner chooses their top-to-bottom order.
#[test]
fn avenging_angel_merged_death_owner_orders_the_physical_cards_on_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two existing top cards make the placement observable.
    scenario.with_library_top(P0, &["Existing A", "Existing B"]);
    let angel = scenario
        .add_creature(P0, "Avenging Angel", 3, 3)
        .from_oracle_text_with_keywords(&["Flying"], AVENGING_ANGEL)
        .id();
    let component = scenario.add_creature(P0, "Migratory Greathorn", 3, 4).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    scenario.with_mana_pool(P0, black_mana(3));
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, angel, MergeSide::Top, &mut events);
    }
    let library_before = library(&runner);
    // The removal is the ONLY action that produced the death; the fixture never
    // records a departure on its own.
    runner.cast(murder).target_object(angel).commit();
    let offered = accept_optional_until_arrangement(&mut runner)
        .expect("CR 401.4: the owner must order the two cards the merged permanent became");
    assert_eq!(
        offered.len(),
        2,
        "both physical cards are placed: {offered:?}"
    );
    assert!(offered.contains(&angel) && offered.contains(&component));
    assert_eq!(runner.state().objects[&angel].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&component].zone, Zone::Graveyard);

    // Chosen order is the REVERSE of the offered (engine-delivery) order.
    let mut chosen = offered.clone();
    chosen.reverse();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("the owner's reversed order is accepted");
    resolve_to_priority(&mut runner);

    let top = library(&runner);
    assert_eq!(
        &top[..2],
        &chosen[..],
        "the chosen order is the library's top-to-bottom order (offered {offered:?})"
    );
    assert_eq!(
        &top[2..],
        &library_before[..],
        "the cards that were already on top stay below the placed group"
    );
    assert_eq!(runner.state().objects[&angel].zone, Zone::Library);
    assert_eq!(runner.state().objects[&component].zone, Zone::Library);
}

/// CR 401.4: a single card has nothing to arrange, so the dies trigger places it
/// with no prompt at all.
#[test]
fn unmerged_avenging_angel_death_opens_no_arrangement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Existing A"]);
    let angel = scenario
        .add_creature(P0, "Avenging Angel", 3, 3)
        .from_oracle_text_with_keywords(&["Flying"], AVENGING_ANGEL)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    scenario.with_mana_pool(P0, black_mana(3));
    let mut runner = scenario.build();

    runner
        .cast(murder)
        .target_object(angel)
        .accept_optional()
        .resolve();
    assert!(
        arrangement_cards(&runner).is_none(),
        "one physical card is not an arrangement: {:?}",
        runner.state().waiting_for
    );
    resolve_to_priority(&mut runner);

    assert_eq!(library(&runner)[0], angel);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 730.3 + CR 401.4: selecting a merged permanent with Tel-Jilad Stylus puts
/// its physical cards on the bottom of their owners' libraries, and the owner
/// chooses their order.
#[test]
fn tel_jilad_merged_permanent_owner_orders_the_physical_cards_on_bottom() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Existing A", "Existing B"]);
    let stylus = scenario
        .add_artifact_from_oracle(P0, "Tel-Jilad Stylus", TEL_JILAD_STYLUS)
        .id();
    let host = scenario.add_creature(P0, "Host Creature", 2, 2).id();
    let component = scenario.add_creature(P0, "Migratory Greathorn", 3, 4).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, host, MergeSide::Top, &mut events);
    }

    let outcome = runner.activate(stylus, 0).target_object(host).resolve();
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "reach guard: the merged permanent is still on the battlefield; waiting {:?}",
        outcome.final_waiting_for()
    );
    let offered = arrangement_cards(&runner).unwrap_or_else(|| {
        panic!(
            "CR 401.4: the stylus' owner must order the merged permanent's physical cards; \
             waiting {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        offered.len(),
        2,
        "both physical cards are placed: {offered:?}"
    );
    assert!(offered.contains(&host) && offered.contains(&component));

    // Chosen order is the REVERSE of the offered (merge-stack delivery) order.
    let mut chosen = offered.clone();
    chosen.reverse();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("the owner's reversed order is accepted");
    resolve_to_priority(&mut runner);

    let bottom = library(&runner);
    assert_eq!(
        &bottom[2..],
        &chosen[..],
        "the chosen order is the placed group's top-to-bottom order at the bottom \
         (offered {offered:?})"
    );
    assert_eq!(runner.state().objects[&host].zone, Zone::Library);
    assert_eq!(runner.state().objects[&component].zone, Zone::Library);
    assert_eq!(runner.state().objects[&stylus].zone, Zone::Battlefield);
}

/// CR 401.4: a non-merged target is one card, so the stylus opens no prompt.
#[test]
fn tel_jilad_single_permanent_opens_no_arrangement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Existing A"]);
    let stylus = scenario
        .add_artifact_from_oracle(P0, "Tel-Jilad Stylus", TEL_JILAD_STYLUS)
        .id();
    let creature = scenario.add_creature(P0, "Host Creature", 2, 2).id();
    let mut runner = scenario.build();

    runner.activate(stylus, 0).target_object(creature).resolve();
    assert!(
        arrangement_cards(&runner).is_none(),
        "one physical card is not an arrangement: {:?}",
        runner.state().waiting_for
    );
    resolve_to_priority(&mut runner);

    assert_eq!(library(&runner).last().copied(), Some(creature));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 108.3 + CR 401.4: Tel-Jilad Stylus names a permanent its activator OWNS,
/// not one they control. A merged permanent owned but controlled by an opponent
/// is a legal target, and its owner orders the physical cards in their library.
#[test]
fn tel_jilad_targets_an_owned_permanent_controlled_by_an_opponent() {
    use engine::types::ability::TargetRef;
    use engine::types::game_state::CastPaymentMode;
    use engine::types::player::PlayerId;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Existing A", "Existing B"]);
    let stylus = scenario
        .add_artifact_from_oracle(P0, "Tel-Jilad Stylus", TEL_JILAD_STYLUS)
        .id();
    // Owned by P0 (the activator), controlled by P1.
    let host = scenario
        .add_creature(P0, "Host Creature", 2, 2)
        .controlled_by(PlayerId(1))
        .id();
    let component = scenario.add_creature(P0, "Migratory Greathorn", 3, 4).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, host, MergeSide::Top, &mut events);
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: stylus,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the stylus is activatable");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(host)),
            "an owned, opponent-controlled permanent is a legal target: {:?}",
            target_slots[0].legal_targets
        ),
        other => panic!("the stylus must announce its target: {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host)),
        })
        .expect("the owned target is accepted");
    let offered = accept_optional_until_arrangement(&mut runner).unwrap_or_else(|| {
        panic!(
            "CR 401.4: the owner orders the physical cards regardless of control; waiting {:?}",
            runner.state().waiting_for
        )
    });
    let mut chosen = offered.clone();
    chosen.reverse();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("the owner's reversed order is accepted");
    resolve_to_priority(&mut runner);

    let bottom = library(&runner);
    assert_eq!(&bottom[2..], &chosen[..]);
    assert_eq!(runner.state().objects[&host].zone, Zone::Library);
    assert_eq!(runner.state().objects[&component].zone, Zone::Library);
}

/// CR 108.3: an opponent-owned permanent the activator controls is NOT what
/// "a permanent you own" names, so it is not a legal target.
#[test]
fn tel_jilad_rejects_an_opponent_owned_permanent_you_control() {
    use engine::types::ability::TargetRef;
    use engine::types::game_state::CastPaymentMode;
    use engine::types::player::PlayerId;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stylus = scenario
        .add_artifact_from_oracle(P0, "Tel-Jilad Stylus", TEL_JILAD_STYLUS)
        .id();
    let borrowed = scenario
        .add_creature(PlayerId(1), "Borrowed Creature", 2, 2)
        .controlled_by(P0)
        .id();
    // A decoy keeps the target prompt open so the exclusion is observable.
    let owned = scenario.add_creature(P0, "Owned Creature", 2, 2).id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: stylus,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the stylus is activatable");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(owned)),
                "reach guard: the owned decoy is offered: {:?}",
                target_slots[0].legal_targets
            );
            assert!(
                !target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(borrowed)),
                "control does not satisfy \"you own\": {:?}",
                target_slots[0].legal_targets
            );
        }
        other => panic!("the stylus must announce its target: {other:?}"),
    }
}
