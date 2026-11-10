//! CR 730.2a + CR 730.2c + CR 730.3: a mass zone scan ranges over actual
//! battlefield permanents, not every object that stores a `Zone::Battlefield`
//! row.
//!
//! An absorbed merge component keeps a stored battlefield row so component
//! queries can locate it, but it is NOT a battlefield permanent — only the
//! published host is (CR 730.2c: an absorbed card is not a separate permanent,
//! and CR 730.2a gives the merged permanent the characteristics of its topmost
//! component). The mass `ChangeZoneAll` scan must therefore select the published
//! survivor and let the merge primitive deliver its component stack, exactly
//! once each.
//!
//! The witness is Hallowed Burial over its verbatim Oracle text (checked against
//! `data/mtgjson/AtomicCards.json`): "Put all creatures on the bottom of their
//! owners' libraries."
//!
//! * An UNCREWED Smuggler's Copter (artifact Vehicle, not a creature) with a
//!   creature absorbed underneath is not one of "all creatures" (CR 730.2a: the
//!   pile presents the Vehicle's characteristics), and the absorbed creature's
//!   stored battlefield row is not an independent permanent. Nothing from that
//!   pile may be offered to an owner or delivered.
//! * The same pile IS one creature while the Vehicle is animated (a crewed
//!   Vehicle, CR 301.7b). Hallowed Burial then names the pile's two physical
//!   cards once each and places both.

use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{EffectKind, LibraryPosition};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HALLOWED_BURIAL_ORACLE: &str = "Put all creatures on the bottom of their owners' libraries.";

/// Smuggler's Copter's real Oracle text (verified against
/// `data/mtgjson/AtomicCards.json`).
const SMUGGLERS_COPTER_ORACLE: &str = "Flying\n\
     Whenever this Vehicle attacks or blocks, you may draw a card. If you do, discard a card.\n\
     Crew 1 (Tap any number of creatures you control with total power 1 or more: This Vehicle \
     becomes an artifact creature until end of turn.)";

/// {3}{W}{W} for Hallowed Burial.
fn burial_mana() -> Vec<ManaUnit> {
    [
        ManaType::White,
        ManaType::White,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ]
    .into_iter()
    .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
    .collect()
}

fn add_hallowed_burial(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Hallowed Burial", false, HALLOWED_BURIAL_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 3,
        })
        .id()
}

fn library(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|id| id.0);
    ids
}

/// Merge `absorbed` underneath `host`, leaving it a component with a stored
/// battlefield row but no battlefield membership (the production shape of
/// `absorb_component`, CR 730.2c).
fn merge_under(state: &mut GameState, host: ObjectId, absorbed: ObjectId) {
    state.battlefield.retain(|&id| id != absorbed);
    let mut events = Vec::new();
    merge_object_onto(state, absorbed, host, MergeSide::Bottom, &mut events);
}

/// Submit the offered order and settle the placement, draining triggers.
fn submit_and_settle(runner: &mut GameRunner, chosen: &[ObjectId]) {
    runner
        .act(GameAction::SelectCards {
            cards: chosen.to_vec(),
        })
        .expect("the owner submits their arrangement");
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
                    .expect("priority pass settles the placement");
            }
            other => panic!("unexpected prompt after the arrangement: {other:?}"),
        }
    }
    panic!("the arrangement did not settle within the safety bound");
}

/// The offered order of one owner's bottom arrangement prompt.
fn bottom_arrangement_cards(waiting_for: &WaitingFor, owner: PlayerId) -> Vec<ObjectId> {
    match waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            effect_kind,
            library_position,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, owner, "CR 401.4: the cards' owner arranges them");
            assert_eq!(*effect_kind, EffectKind::PutAtLibraryPosition);
            assert_eq!(*library_position, Some(LibraryPosition::Bottom));
            assert!(
                mass_library_order.is_some(),
                "the prompt carries the frozen owner batch"
            );
            cards.clone()
        }
        other => panic!("expected {owner:?}'s CR 401.4 arrangement prompt, got {other:?}"),
    }
}

fn add_uncrewed_copter(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Smuggler's Copter", 3, 3)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text_with_keywords(&["Flying", "Crew"], SMUGGLERS_COPTER_ORACLE)
        .id()
}

/// CR 730.2a + CR 730.2c + CR 730.3: an uncrewed Smuggler's Copter is an
/// artifact Vehicle, not a creature, so it is not one of "all creatures". The
/// creature absorbed underneath it stores a `Zone::Battlefield` row but is not a
/// battlefield permanent either, so neither may be offered or delivered.
///
/// REVERT-FAILING: the broad zone scan matched the absorbed creature's stored
/// battlefield row as an independent permanent, so P0's arrangement prompt named
/// a phantom third card and the delivery moved the absorbed card out from under
/// its host without ever moving the host.
#[test]
fn uncrewed_vehicle_absorbed_creature_is_not_a_mass_scan_candidate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["P0 Library"]);
    let copter = add_uncrewed_copter(&mut scenario);
    let absorbed = scenario.add_creature(P0, "Absorbed Creature", 2, 2).id();
    let bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let vanguard = scenario.add_creature(P0, "P0 Vanguard", 1, 1).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_under(runner.state_mut(), copter, absorbed);

    // Fixture guards: the host is the published permanent; the absorbed card is
    // not, but it still stores a battlefield row.
    {
        let state = runner.state();
        assert_eq!(
            state.objects[&copter].merged_components,
            vec![copter, absorbed],
            "fixture: the Vehicle is the topmost (published) component"
        );
        assert!(state.battlefield.contains(&copter));
        assert!(!state.battlefield.contains(&absorbed));
        assert_eq!(
            state.objects[&absorbed].zone,
            Zone::Battlefield,
            "fixture: the absorbed member stores a battlefield row"
        );
        assert!(
            !state.objects[&copter]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "fixture: an uncrewed Vehicle is not a creature"
        );
    }
    let filler = library(&runner, P0)[0];

    let outcome = runner.cast(burial).resolve();
    let offered = bottom_arrangement_cards(outcome.final_waiting_for(), P0);
    assert_eq!(
        sorted(offered.clone()),
        sorted(vec![bear, vanguard]),
        "only the two real battlefield creatures are offered; neither the \
         uncrewed Vehicle nor its absorbed card is a candidate"
    );

    let mut chosen = offered;
    chosen.reverse();
    submit_and_settle(&mut runner, &chosen);

    assert_eq!(
        library(&runner, P0),
        vec![filler, chosen[0], chosen[1]],
        "only the two real creatures reach the bottom of P0's library"
    );
    for id in [bear, vanguard] {
        assert_eq!(zone(&runner, id), Zone::Library, "the real creatures leave");
    }
    // The merged pile is untouched: the host is still the published permanent
    // and the absorbed card never left it.
    assert_eq!(
        zone(&runner, copter),
        Zone::Battlefield,
        "an uncrewed Vehicle is not a creature and is not placed"
    );
    assert!(runner.state().battlefield.contains(&copter));
    assert_eq!(
        runner.state().objects[&copter].merged_components,
        vec![copter, absorbed],
        "the pile keeps both physical cards"
    );
    assert!(
        !runner.state().battlefield.contains(&absorbed),
        "the absorbed card remains a component, never a permanent"
    );
    assert_eq!(
        zone(&runner, absorbed),
        Zone::Battlefield,
        "the absorbed card's stored battlefield row never becomes an arrival"
    );
}

/// CR 301.7b + CR 730.2a + CR 730.3: while the Vehicle is animated (a crewed
/// Vehicle is an artifact creature), the pile IS one creature. Hallowed Burial
/// selects the published host once and its physical-arrival expansion names the
/// pile's two cards exactly once each, then places both.
///
/// REVERT-FAILING (in spirit): the broad scan also selected the absorbed card's
/// stored battlefield row independently; the earlier dedup only hid the
/// duplicate in the prompt, leaving the delivery path with a phantom candidate.
#[test]
fn creature_vehicle_host_is_a_single_candidate_with_both_physical_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, burial_mana());
    scenario.with_library_top(P0, &["P0 Library"]);
    // CR 301.7b: the crewed state is what makes the Vehicle a creature. The
    // witness seeds that state directly rather than replaying the crew cost,
    // because this row is about the mass scan, not the crew action.
    let copter = scenario
        .add_creature(P0, "Smuggler's Copter", 3, 3)
        .as_artifact_creature()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text_with_keywords(&["Flying", "Crew"], SMUGGLERS_COPTER_ORACLE)
        .id();
    let absorbed = scenario.add_creature(P0, "Absorbed Creature", 2, 2).id();
    let burial = add_hallowed_burial(&mut scenario);
    let mut runner = scenario.build();
    merge_under(runner.state_mut(), copter, absorbed);
    let filler = library(&runner, P0)[0];

    let outcome = runner.cast(burial).resolve();
    let offered = bottom_arrangement_cards(outcome.final_waiting_for(), P0);
    assert_eq!(
        sorted(offered.clone()),
        sorted(vec![copter, absorbed]),
        "the prompt names the published host's two physical cards exactly once"
    );

    let mut chosen = offered;
    chosen.reverse();
    submit_and_settle(&mut runner, &chosen);

    assert_eq!(
        library(&runner, P0),
        vec![filler, chosen[0], chosen[1]],
        "both physical cards are placed at the bottom in the owner's order"
    );
    for id in [copter, absorbed] {
        assert_eq!(zone(&runner, id), Zone::Library, "every arrival is placed");
    }
    assert!(
        !runner.state().battlefield.contains(&copter)
            && !runner.state().battlefield.contains(&absorbed),
        "the pile left the battlefield as one permanent"
    );
}
