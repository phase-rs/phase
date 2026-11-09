//! Runtime regression for "target player shuffles up to N target cards from
//! their graveyard into their library" (Dwell on the Past and the rest of the
//! printed class).
//!
//! CR 115.1 + CR 701.24a + CR 608.2c: the announced player target performs the
//! shuffle, so the terminal shuffle must name that player rather than the
//! caster. The class shares one parser shape with the whole-graveyard sibling
//! (Reminisce), differing only in the moved-card filter (`ChangeZone` of a
//! typed card instead of `ChangeZoneAll`).

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
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
const COSI_ORACLE: &str = "Whenever an opponent shuffles their library, you may put a +1/+1 counter on this creature.";

/// Ordered projection of who actually shuffled a library.
fn shuffled_players(outcome: &CastOutcome) -> Vec<PlayerId> {
    outcome
        .events()
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
    let shuffled = shuffled_players(outcome);
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
