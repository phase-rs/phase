//! Runtime regression for "target player shuffles up to N target cards from
//! their graveyard into their library" (Dwell on the Past and the rest of the
//! printed class).
//!
//! CR 115.1 + CR 701.24a + CR 608.2c: the announced player target performs the
//! shuffle, so the terminal shuffle must name that player rather than the
//! caster. The class shares one parser shape with the whole-graveyard sibling
//! (Reminisce), differing only in the moved-card filter (`ChangeZone` of a
//! typed card instead of `ChangeZoneAll`).

use engine::game::scenario::{CastCommit, CastOutcome, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DWELL_ORACLE: &str =
    "Target player shuffles up to four target cards from their graveyard into their library.";
const WAND_ORACLE: &str = "{T}: Mill a card.\n{2}, {T}, Exile this artifact: Shuffle up to five target cards from your graveyard into your library.";
const LOAMING_ORACLE: &str = "When this creature enters, target player shuffles any number of target cards from their graveyard into their library.";
const RITE_ORACLE: &str = "Return up to two target permanent cards from your graveyard to your hand. Target player shuffles up to four target cards from their graveyard into their library. Exile Rite of Renewal.";
const QUANDRIX_ORACLE: &str = "Choose two —\n• Return target creature or planeswalker to its owner's hand.\n• Counter target artifact or enchantment spell.\n• Put two +1/+1 counters on target creature.\n• Target player shuffles up to three target cards from their graveyard into their library.";
const COSI_ORACLE: &str =
    "Whenever an opponent shuffles their library, you may put a +1/+1 counter on this creature.";
const STREAM_ORACLE: &str = "Target player mills four cards. You shuffle up to four cards from your graveyard into your library.\nReplicate {2}{U}{U} (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)";

// Full printed Oracle text, checked against Scryfall's named-card API.
const MEMORY_ORACLE: &str = "Target player shuffles up to three target cards from their graveyard into their library.\nFlashback {G} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const SURGE_ORACLE: &str = "You and permanents you control gain hexproof until end of turn. Prevent all damage that black and/or red sources would deal to creatures you control this turn.";
const GAEA_ORACLE: &str = "Target player shuffles up to three target cards from their graveyard into their library.\nDraw a card.\nWhen this card is put into your graveyard from your library, shuffle your graveyard into your library.";
const THOUGHT_SCOUR_ORACLE: &str = "Target player mills two cards.\nDraw a card.";
const CREMATE_ORACLE: &str = "Exile target card from a graveyard.\nDraw a card.";

/// Ordered projection of who actually shuffled a library.
fn shuffled_players(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect()
}

/// CR 701.24a + CR 603.2c: one printed shuffle instruction randomizes once,
/// after the chosen set is delivered. Member delivery must not publish extra
/// shuffles (and therefore extra shuffle-trigger opportunities).
fn assert_only_player_shuffled(outcome: &CastOutcome, player: PlayerId, why: &str) {
    let shuffled = shuffled_players(outcome.events());
    assert_eq!(
        shuffled.as_slice(),
        &[player],
        "exactly one shuffle belongs to the instruction's player: {why}"
    );
}

/// Reach guard: the terminal `Effect::Shuffle` must have resolved at all.
fn shuffle_resolutions(outcome: &CastOutcome) -> usize {
    outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )
        })
        .count()
}

fn assert_completed(outcome: &CastOutcome) {
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must complete at a Priority window, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the stack must be empty after resolution"
    );
}

/// Respond through the ordinary cast/priority pipeline, stopping after Surge
/// resolves so the original spell is still waiting beneath it.
fn resolve_surge_response(original: &mut CastCommit<'_>, surge: ObjectId) -> Vec<GameEvent> {
    assert!(matches!(
        original.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    let mut events = original
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 can respond with Surge")
        .events;
    assert!(matches!(
        original.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));

    let mut response = original.cast(surge).commit();
    events.extend_from_slice(response.events());
    for _ in 0..8 {
        if response.state().objects[&surge].zone == Zone::Graveyard {
            break;
        }
        assert!(matches!(
            response.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(
            response
                .act(GameAction::PassPriority)
                .expect("both players pass to resolve the real Surge response")
                .events,
        );
    }
    assert_eq!(response.state().objects[&surge].zone, Zone::Graveyard);
    assert!(matches!(
        response.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    events
}

/// CR 115.1 + CR 608.2c + CR 701.24a: casting Dwell on the Past at P1 moves
/// P1's two declared cards and shuffles P1's library. P0's graveyard card and
/// P0's library are untouched, so the discriminator is not vacuous.
#[test]
fn dwell_on_the_past_shuffles_the_targeted_players_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dwell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dwell on the Past", false, DWELL_ORACLE)
        .id();

    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let p0_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let p1_graveyard_a = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card A", 1, 1)
        .id();
    let p1_graveyard_b = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card B", 1, 1)
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(dwell)
        .target_player(P1)
        .target_objects(&[p1_graveyard_a, p1_graveyard_b])
        .resolve();

    assert_completed(&outcome);
    assert_eq!(
        shuffle_resolutions(&outcome),
        1,
        "the terminal shuffle must resolve"
    );

    outcome.assert_zone(&[p1_graveyard_a, p1_graveyard_b], Zone::Library);
    outcome.assert_zone(&[p0_graveyard], Zone::Graveyard);

    let p0_state = &outcome.state().players[P0.0 as usize];
    let p1_state = &outcome.state().players[P1.0 as usize];
    assert!(p1_state.library.contains(&p1_graveyard_a));
    assert!(p1_state.library.contains(&p1_graveyard_b));
    assert!(p1_state.library.contains(&p1_library));
    assert!(p1_state.graveyard.is_empty());
    assert!(p0_state.graveyard.contains(&p0_graveyard));
    assert!(!p0_state.library.contains(&p0_graveyard));
    assert!(p0_state.library.contains(&p0_library));

    assert_only_player_shuffled(
        &outcome,
        P1,
        "the targeted player shuffles their own library",
    );
}

/// CR 701.24c + CR 701.24d: even with no cards chosen (all "up to" slots
/// declined), the announced player still shuffles their library. This proves
/// the shuffle's player comes from the announced target, not the moved set.
#[test]
fn dwell_on_the_past_with_no_cards_chosen_still_shuffles_the_targeted_players_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dwell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dwell on the Past", false, DWELL_ORACLE)
        .id();

    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let p0_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let p1_graveyard = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card", 1, 1)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(dwell).target_player(P1).resolve();

    assert_completed(&outcome);
    assert_eq!(
        shuffle_resolutions(&outcome),
        1,
        "the empty chosen set still runs the terminal shuffle"
    );

    // Nothing moved: both graveyards keep their cards.
    outcome.assert_zone(&[p0_graveyard, p1_graveyard], Zone::Graveyard);
    assert_eq!(outcome.zone_of(p0_library), Zone::Library);
    assert_eq!(outcome.zone_of(p1_library), Zone::Library);

    assert_only_player_shuffled(
        &outcome,
        P1,
        "the declared player shuffles even when no cards are chosen",
    );
}

/// CR 701.24a: Wand of Vertebrae prints "Shuffle up to five target cards from
/// YOUR graveyard into your library." With no player subject, the caster
/// performs the shuffle — the over-widening guard for this change. Ability 1 is
/// the shuffle; ability 0 is "{T}: Mill a card."
#[test]
fn wand_of_vertebrae_keeps_the_casters_shuffle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let wand = scenario
        .add_artifact_from_oracle(P0, "Wand of Vertebrae", WAND_ORACLE)
        .id();

    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let p0_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let p1_graveyard = scenario
        .add_creature_to_graveyard(P1, "Opponent Graveyard Card", 1, 1)
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .activate(wand, 1)
        .target_objects(&[p0_graveyard])
        .resolve();

    assert_completed(&outcome);
    assert_eq!(
        shuffle_resolutions(&outcome),
        1,
        "the activated shuffle must resolve"
    );

    outcome.assert_zone(&[p0_graveyard], Zone::Library);
    let p0_state = &outcome.state().players[P0.0 as usize];
    let p1_state = &outcome.state().players[P1.0 as usize];
    assert!(p0_state.library.contains(&p0_graveyard));
    assert!(p0_state.library.contains(&p0_library));
    assert!(
        p1_state.graveyard.contains(&p1_graveyard),
        "the opponent's graveyard stays untouched"
    );
    assert!(p1_state.library.contains(&p1_library));

    assert_only_player_shuffled(&outcome, P0, "the caster, not the opponent, shuffles");
}

/// CR 603.3d + CR 115.1 + CR 701.24a: Loaming Shaman's enters trigger
/// ("any number of target cards") moves and shuffles the chosen player's
/// library through the trigger-target path.
#[test]
fn loaming_shaman_etb_shuffles_the_chosen_players_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shaman = scenario
        .add_creature_to_hand_from_oracle(P0, "Loaming Shaman", 3, 3, LOAMING_ORACLE)
        .id();

    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let p0_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let p1_graveyard = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card", 1, 1)
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(shaman)
        .target_player(P1)
        .target_objects(&[p1_graveyard])
        .resolve();

    assert_completed(&outcome);
    assert_eq!(
        shuffle_resolutions(&outcome),
        1,
        "the enters-trigger shuffle must resolve"
    );

    outcome.assert_zone(&[p1_graveyard], Zone::Library);
    outcome.assert_zone(&[p0_graveyard], Zone::Graveyard);
    let p0_state = &outcome.state().players[P0.0 as usize];
    let p1_state = &outcome.state().players[P1.0 as usize];
    assert!(p1_state.library.contains(&p1_graveyard));
    assert!(p1_state.library.contains(&p1_library));
    assert!(p0_state.graveyard.contains(&p0_graveyard));
    assert!(p0_state.library.contains(&p0_library));

    assert_only_player_shuffled(&outcome, P1, "the chosen player shuffles their own library");
}

/// CR 601.2c + CR 608.2c + CR 701.24a: an earlier object target is not
/// the actor of a later targeted-player shuffle. Returning P0's permanent
/// and shuffling P1's graveyard card must preserve both target groups.
#[test]
fn rite_of_renewal_keeps_the_shuffle_player_after_an_earlier_object_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of Renewal", false, RITE_ORACLE)
        .id();
    let return_card = scenario
        .add_creature_to_graveyard(P0, "First Target Permanent", 1, 1)
        .id();
    let untouched_return_card = scenario
        .add_creature_to_graveyard(P0, "Unchosen Return Card", 1, 1)
        .id();
    let shuffle_card = scenario
        .add_creature_to_graveyard(P1, "Later Target Permanent", 1, 1)
        .id();
    let untouched_shuffle_card = scenario
        .add_creature_to_graveyard(P1, "Unchosen Shuffle Card", 1, 1)
        .id();
    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");

    let mut runner = scenario.build();
    let outcome = runner
        .cast(rite)
        .target_objects(&[return_card, shuffle_card])
        .target_player(P1)
        .resolve();

    assert_completed(&outcome);
    assert_eq!(
        shuffle_resolutions(&outcome),
        1,
        "the shuffle after the earlier return target must resolve"
    );
    outcome.assert_zone(&[return_card], Zone::Hand);
    outcome.assert_zone(&[shuffle_card], Zone::Library);
    outcome.assert_zone(
        &[untouched_return_card, untouched_shuffle_card],
        Zone::Graveyard,
    );
    outcome.assert_zone(&[rite], Zone::Exile);
    let p0_state = &outcome.state().players[P0.0 as usize];
    let p1_state = &outcome.state().players[P1.0 as usize];
    assert!(p0_state.hand.contains(&return_card));
    assert!(!p0_state.library.contains(&return_card));
    assert!(p0_state.library.contains(&p0_library));
    assert!(p1_state.library.contains(&shuffle_card));
    assert!(p1_state.library.contains(&p1_library));
    assert_only_player_shuffled(
        &outcome,
        P1,
        "the earlier object target must not replace the announced shuffle player",
    );
}

/// CR 700.2 + CR 601.2c + CR 701.24a: selecting an earlier targeted mode
/// must not make its creature target the actor of the later shuffle mode.
#[test]
fn quandrix_command_keeps_the_shuffle_player_after_an_earlier_targeted_mode() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Quandrix Command", true, QUANDRIX_ORACLE)
        .id();
    let bounce_card = scenario.add_creature(P0, "Earlier Modal Target", 2, 2).id();
    let p0_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let shuffle_card = scenario
        .add_creature_to_graveyard(P1, "Later Modal Target", 1, 1)
        .id();
    let untouched_shuffle_card = scenario
        .add_creature_to_graveyard(P1, "Unchosen Shuffle Card", 1, 1)
        .id();
    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");

    let mut runner = scenario.build();
    let outcome = runner
        .cast(command)
        .modes(&[0, 3])
        .target_objects(&[bounce_card, shuffle_card])
        .target_player(P1)
        .resolve();

    assert_completed(&outcome);
    assert_eq!(
        shuffle_resolutions(&outcome),
        1,
        "the selected shuffle mode must resolve after the bounce mode"
    );
    outcome.assert_zone(&[bounce_card], Zone::Hand);
    outcome.assert_zone(&[shuffle_card], Zone::Library);
    outcome.assert_zone(&[p0_graveyard, untouched_shuffle_card], Zone::Graveyard);
    outcome.assert_zone(&[command], Zone::Graveyard);
    let p0_state = &outcome.state().players[P0.0 as usize];
    let p1_state = &outcome.state().players[P1.0 as usize];
    assert!(p0_state.hand.contains(&bounce_card));
    assert!(p0_state.library.contains(&p0_library));
    assert!(p1_state.library.contains(&shuffle_card));
    assert!(p1_state.library.contains(&p1_library));
    assert_only_player_shuffled(
        &outcome,
        P1,
        "an earlier mode's creature target must not replace the shuffle player",
    );
}

/// CR 701.24a + CR 603.2c: one selected opposing card gives Cosi's Trickster
/// one trigger opportunity, not one for delivery plus another for the terminal
/// instruction. The trigger is accepted and resolves through the normal stack.
#[test]
fn dwell_on_the_past_one_card_shuffles_once_and_triggers_cosi_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cosi = scenario
        .add_creature_from_oracle(P0, "Cosi's Trickster", 1, 1, COSI_ORACLE)
        .id();
    let dwell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dwell on the Past", false, DWELL_ORACLE)
        .id();
    let selected = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card", 1, 1)
        .id();
    let unchosen = scenario
        .add_creature_to_graveyard(P1, "Unchosen Graveyard Card", 1, 1)
        .id();
    scenario.add_card_to_library_top(P0, "Caster Library Card");
    let target_library = scenario.add_card_to_library_top(P1, "Target Library Card");

    let mut runner = scenario.build();
    let outcome = runner
        .cast(dwell)
        .target_player(P1)
        .target_object(selected)
        .accept_optional()
        .resolve();

    assert_completed(&outcome);
    outcome.assert_zone(&[selected], Zone::Library);
    outcome.assert_zone(&[unchosen], Zone::Graveyard);
    assert!(outcome.state().players[P1.0 as usize]
        .library
        .contains(&target_library));
    outcome.assert_counters(cosi, CounterType::Plus1Plus1, 1);
    assert_eq!(shuffle_resolutions(&outcome), 1);
    assert_only_player_shuffled(
        &outcome,
        P1,
        "the single printed shuffle must give Cosi's Trickster one opportunity",
    );
}

/// CR 903.9b + CR 614.1 + CR 701.24a: a chosen commander pauses delivery
/// for its owner's library/command-zone decision. Declining that replacement
/// must retain the whole set and still publish only the terminal shuffle.
#[test]
fn dwell_on_the_past_shuffles_once_after_a_commander_replacement_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dwell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dwell on the Past", false, DWELL_ORACLE)
        .id();
    let commander = scenario
        .add_creature_to_graveyard(P1, "Chosen Commander", 2, 2)
        .as_legendary()
        .commander()
        .id();
    let second = scenario
        .add_creature_to_graveyard(P1, "Second Chosen Card", 1, 1)
        .id();
    let unchosen = scenario
        .add_creature_to_graveyard(P1, "Unchosen Graveyard Card", 1, 1)
        .id();
    let target_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    scenario.add_card_to_library_top(P0, "Caster Library Card");
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    // CR 903.9a: establish the owner's ordinary decision to leave the
    // commander in its initial graveyard before announcing the spell.
    runner
        .act(GameAction::PassPriority)
        .expect("initial commander state-based choice is reachable");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CommanderZoneChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("the owner leaves the commander in the graveyard");

    let paused = runner
        .cast(dwell)
        .target_player(P1)
        .target_objects(&[commander, second])
        .resolve();
    assert!(matches!(
        paused.final_waiting_for(),
        WaitingFor::ReplacementChoice {
            player: P1,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    let mut events = paused.events().to_vec();
    assert!(
        shuffled_players(&events).is_empty(),
        "delivery must not shuffle any partial set before the owner decides"
    );

    // OptionalBranch's public choice contract is index 0 = accept,
    // index 1 = decline. Keep the originally requested library destination.
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("declining the commander replacement resumes the move")
            .events,
    );
    for _ in 0..16 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("normal priority passing finishes the resumed spell")
                .events,
        );
    }

    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for card in [commander, second, target_library] {
        assert!(runner.state().players[P1.0 as usize]
            .library
            .contains(&card));
    }
    assert_eq!(runner.state().objects[&unchosen].zone, Zone::Graveyard);
    assert_eq!(shuffled_players(&events).as_slice(), &[P1]);
}

/// CR 701.24a + CR 603.2c: a non-targeted graveyard choice carries the same
/// terminal placement through EffectZoneChoice. The earlier mill target does
/// not become the actor for the caster's separate shuffle instruction.
#[test]
fn stream_of_thought_resolution_choice_shuffles_once_for_the_caster() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stream = scenario
        .add_spell_to_hand_from_oracle(P0, "Stream of Thought", false, STREAM_ORACLE)
        .id();
    let selected = scenario
        .add_creature_to_graveyard(P0, "Chosen Graveyard Card", 1, 1)
        .id();
    let unchosen = scenario
        .add_creature_to_graveyard(P0, "Unchosen Graveyard Card", 1, 1)
        .id();
    let caster_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let milled = [
        scenario.add_card_to_library_top(P1, "Milled Card A"),
        scenario.add_card_to_library_top(P1, "Milled Card B"),
        scenario.add_card_to_library_top(P1, "Milled Card C"),
        scenario.add_card_to_library_top(P1, "Milled Card D"),
    ];

    let mut runner = scenario.build();
    let outcome = runner
        .cast(stream)
        .target_player(P1)
        .effect_zone(&[selected])
        .resolve();

    assert_completed(&outcome);
    outcome.assert_zone(&[selected], Zone::Library);
    outcome.assert_zone(&[unchosen, stream], Zone::Graveyard);
    outcome.assert_zone(&milled, Zone::Graveyard);
    assert!(outcome.state().players[P0.0 as usize]
        .library
        .contains(&caster_library));
    assert!(outcome.state().players[P0.0 as usize]
        .library
        .contains(&selected));
    assert!(!outcome.state().players[P1.0 as usize]
        .library
        .contains(&selected));
    assert_eq!(shuffle_resolutions(&outcome), 1);
    assert_only_player_shuffled(
        &outcome,
        P0,
        "resolution-time delivery must not auto-shuffle before the caster's terminal shuffle",
    );
}

/// Memory's Journey's 2011-09-22 Wizards ruling explicitly says an illegal
/// player prevents its effect even when the targeted graveyard cards stay legal:
/// https://api.scryfall.com/cards/isd/66/rulings
#[test]
fn memorys_journey_moves_no_cards_and_neither_player_shuffles_after_surge() {
    // Keep two legal alternatives after P1 gains hexproof; a single remaining
    // player would be auto-selected instead of exposing a target-choice prompt.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let memory = scenario
        .add_spell_to_hand_from_oracle(P0, "Memory's Journey", true, MEMORY_ORACLE)
        .id();
    let surge = scenario
        .add_spell_to_hand_from_oracle(P1, "Surge of Salvation", true, SURGE_ORACLE)
        .id();
    let player_probe = scenario
        .add_spell_to_hand_from_oracle(P0, "Thought Scour", true, THOUGHT_SCOUR_ORACLE)
        .id();
    let card_probe = scenario
        .add_spell_to_hand_from_oracle(P0, "Cremate", true, CREMATE_ORACLE)
        .id();
    let selected_a = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card A", 1, 1)
        .id();
    let selected_b = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card B", 1, 1)
        .id();
    let unchosen = scenario
        .add_creature_to_graveyard(P1, "Unchosen Graveyard Card", 1, 1)
        .id();
    let caster_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let caster_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let target_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let mut runner = scenario.build();
    let mut original = runner
        .cast(memory)
        .target_player(P1)
        .target_objects(&[selected_a, selected_b])
        .commit();
    let caster_library_before = original.state().players[P0.0 as usize].library.clone();
    let target_library_before = original.state().players[P1.0 as usize].library.clone();
    let mut events = resolve_surge_response(&mut original, surge);
    assert_eq!(original.state().objects[&memory].zone, Zone::Stack);

    // Observe real target offers, not stack slots or protection implementation.
    // These announcements are withdrawn before either probe pays or resolves.
    let card_id = original.state().objects[&player_probe].card_id;
    events.extend(
        original
            .act(GameAction::CastSpell {
                object_id: player_probe,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("P0 can announce the player-target probe above Memory's Journey")
            .events,
    );
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = &original.state().waiting_for
    else {
        panic!(
            "Thought Scour must offer its target-player choice, got {:?}",
            original.state().waiting_for
        );
    };
    let legal = &target_slots[selection.current_slot].legal_targets;
    assert!(legal.contains(&TargetRef::Player(P0)));
    assert!(
        !legal.contains(&TargetRef::Player(P1)),
        "Surge must actually make P1 illegal for P0's spells"
    );
    events.extend(
        original
            .act(GameAction::CancelCast)
            .expect("withdraw Thought Scour without changing any zones")
            .events,
    );

    let card_id = original.state().objects[&card_probe].card_id;
    events.extend(
        original
            .act(GameAction::CastSpell {
                object_id: card_probe,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("P0 can announce the graveyard-card probe")
            .events,
    );
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = &original.state().waiting_for
    else {
        panic!(
            "Cremate must offer its target-card choice, got {:?}",
            original.state().waiting_for
        );
    };
    let legal = &target_slots[selection.current_slot].legal_targets;
    for card in [selected_a, selected_b] {
        assert!(
            legal.contains(&TargetRef::Object(card)),
            "Surge must leave P1's graveyard cards legal targets for P0"
        );
    }
    events.extend(
        original
            .act(GameAction::CancelCast)
            .expect("withdraw Cremate without moving its potential targets")
            .events,
    );

    let outcome = original.resolve();
    events.extend_from_slice(outcome.events());
    assert_completed(&outcome);
    outcome.assert_zone(
        &[selected_a, selected_b, unchosen, caster_graveyard],
        Zone::Graveyard,
    );
    outcome.assert_zone(&[memory, surge], Zone::Graveyard);
    outcome.assert_zone(&[player_probe, card_probe], Zone::Hand);
    outcome.assert_zone(&[caster_library, target_library], Zone::Library);
    assert_eq!(
        outcome.state().players[P0.0 as usize].library,
        caster_library_before
    );
    assert_eq!(
        outcome.state().players[P1.0 as usize].library,
        target_library_before
    );
    assert!(
        shuffled_players(&events).is_empty(),
        "neither the illegal target nor the caster may shuffle"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, .. }
                if [selected_a, selected_b, unchosen, caster_graveyard].contains(object_id)
        )),
        "no graveyard card may move, even transiently"
    );
}

/// The draw is a separate printed instruction. Legal card targets keep the
/// spell resolving after Surge invalidates only the shuffle's player.
#[test]
fn gaeas_blessing_still_draws_when_surge_invalidates_only_the_shuffle_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blessing = scenario
        .add_spell_to_hand_from_oracle(P0, "Gaea's Blessing", false, GAEA_ORACLE)
        .id();
    let surge = scenario
        .add_spell_to_hand_from_oracle(P1, "Surge of Salvation", true, SURGE_ORACLE)
        .id();
    let selected_a = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card A", 1, 1)
        .id();
    let selected_b = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card B", 1, 1)
        .id();
    let caster_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let drawn = scenario.add_card_to_library_top(P0, "Independent Draw");
    let target_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let mut runner = scenario.build();
    let mut original = runner
        .cast(blessing)
        .target_player(P1)
        .target_objects(&[selected_a, selected_b])
        .commit();
    let mut events = resolve_surge_response(&mut original, surge);
    assert_eq!(original.state().objects[&blessing].zone, Zone::Stack);
    let outcome = original.resolve();
    events.extend_from_slice(outcome.events());

    assert_completed(&outcome);
    outcome.assert_zone(&[selected_a, selected_b, blessing, surge], Zone::Graveyard);
    outcome.assert_zone(&[drawn], Zone::Hand);
    outcome.assert_zone(&[caster_library, target_library], Zone::Library);
    outcome.assert_hand_drawn(P0, 1);
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::CardDrawn { player_id: P0, object_id, .. } if *object_id == drawn
    )));
    assert!(
        shuffled_players(&events).is_empty(),
        "skipping the dependent shuffle must not skip Gaea's independent draw"
    );
}

/// Both the earlier return and later self-exile are independent of the
/// targeted player's shuffle; neither may be swallowed by the illegal group.
#[test]
fn rite_of_renewal_still_returns_and_exiles_when_surge_invalidates_the_shuffle_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of Renewal", false, RITE_ORACLE)
        .id();
    let surge = scenario
        .add_spell_to_hand_from_oracle(P1, "Surge of Salvation", true, SURGE_ORACLE)
        .id();
    let returned = scenario
        .add_creature_to_graveyard(P0, "Earlier Return Target", 1, 1)
        .id();
    let selected_a = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card A", 1, 1)
        .id();
    let selected_b = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card B", 1, 1)
        .id();
    let caster_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let target_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let mut runner = scenario.build();
    let mut original = runner
        .cast(rite)
        .target_objects(&[returned, selected_a, selected_b])
        .target_player(P1)
        .commit();
    let mut events = resolve_surge_response(&mut original, surge);
    assert_eq!(original.state().objects[&rite].zone, Zone::Stack);
    let outcome = original.resolve();
    events.extend_from_slice(outcome.events());

    assert_completed(&outcome);
    outcome.assert_zone(&[returned], Zone::Hand);
    outcome.assert_zone(&[selected_a, selected_b, surge], Zone::Graveyard);
    outcome.assert_zone(&[rite], Zone::Exile);
    outcome.assert_zone(&[caster_library, target_library], Zone::Library);
    assert!(
        shuffled_players(&events).is_empty(),
        "the independent return and exile do not authorize either player to shuffle"
    );
}

/// A legal target in another selected mode still receives that mode's effect.
#[test]
fn quandrix_command_still_bounces_when_surge_invalidates_only_the_shuffle_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Quandrix Command", true, QUANDRIX_ORACLE)
        .id();
    let surge = scenario
        .add_spell_to_hand_from_oracle(P1, "Surge of Salvation", true, SURGE_ORACLE)
        .id();
    let bounced = scenario.add_creature(P0, "Earlier Modal Target", 2, 2).id();
    let selected_a = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card A", 1, 1)
        .id();
    let selected_b = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card B", 1, 1)
        .id();
    let caster_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let target_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let mut runner = scenario.build();
    let mut original = runner
        .cast(command)
        .modes(&[0, 3])
        .target_objects(&[bounced, selected_a, selected_b])
        .target_player(P1)
        .commit();
    let mut events = resolve_surge_response(&mut original, surge);
    assert_eq!(original.state().objects[&command].zone, Zone::Stack);
    let outcome = original.resolve();
    events.extend_from_slice(outcome.events());

    assert_completed(&outcome);
    outcome.assert_zone(&[bounced], Zone::Hand);
    outcome.assert_zone(&[selected_a, selected_b, command, surge], Zone::Graveyard);
    outcome.assert_zone(&[caster_library, target_library], Zone::Library);
    assert!(
        shuffled_players(&events).is_empty(),
        "the legal bounce mode must resolve without either player shuffling"
    );
}

/// CR 608.2b + CR 701.24c + CR 701.24e: losing every card target does not
/// remove the still-legal player's instruction, even if their library is empty.
#[test]
fn memorys_journey_still_shuffles_when_cremate_removes_its_only_card_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let memory = scenario
        .add_spell_to_hand_from_oracle(P0, "Memory's Journey", true, MEMORY_ORACLE)
        .id();
    let cremate = scenario
        .add_spell_to_hand_from_oracle(P1, "Cremate", true, CREMATE_ORACLE)
        .id();
    let selected = scenario
        .add_creature_to_graveyard(P1, "Chosen Graveyard Card", 1, 1)
        .id();
    let unchosen = scenario
        .add_creature_to_graveyard(P1, "Unchosen Graveyard Card", 1, 1)
        .id();
    let caster_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let drawn = scenario.add_card_to_library_top(P1, "Cremate Draw");
    let mut runner = scenario.build();
    let mut original = runner
        .cast(memory)
        .target_player(P1)
        .target_object(selected)
        .commit();
    let mut events = original
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 can respond")
        .events;
    assert!(matches!(
        original.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    {
        let mut response = original.cast(cremate).target_object(selected).commit();
        events.extend_from_slice(response.events());
        for _ in 0..8 {
            if response.state().objects[&cremate].zone == Zone::Graveyard {
                break;
            }
            assert!(matches!(
                response.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            events.extend(
                response
                    .act(GameAction::PassPriority)
                    .expect("normal priority passing resolves Cremate")
                    .events,
            );
        }
        assert_eq!(response.state().objects[&cremate].zone, Zone::Graveyard);
        assert_eq!(response.state().objects[&selected].zone, Zone::Exile);
        assert_eq!(response.state().objects[&drawn].zone, Zone::Hand);
        assert!(response.state().players[P1.0 as usize].library.is_empty());
    }
    assert_eq!(original.state().objects[&memory].zone, Zone::Stack);
    let outcome = original.resolve();
    events.extend_from_slice(outcome.events());

    assert_completed(&outcome);
    outcome.assert_zone(&[selected], Zone::Exile);
    outcome.assert_zone(&[unchosen, memory, cremate], Zone::Graveyard);
    outcome.assert_zone(&[drawn], Zone::Hand);
    outcome.assert_zone(&[caster_library], Zone::Library);
    assert!(outcome.state().players[P1.0 as usize].library.is_empty());
    assert_eq!(shuffled_players(&events).as_slice(), &[P1]);
}
