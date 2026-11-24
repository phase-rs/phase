//! CR 700.9 + CR 603.10a: Single-permanent deaths and returns to hand use
//! counters and qualifying attachments captured on departure.

use engine::game::effects::attach::attach_to;
use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const KAMI: &str = "Flash\nEnchant creature you control\nWhenever enchanted creature or another modified creature you control dies, create X 1/1 colorless Spirit creature tokens, where X is that creature's power. (Equipment, Auras you control, and counters are modifications.)";
const FIREBREATHING: &str =
    "Enchant creature\n{R}: Enchanted creature gets +1/+0 until end of turn.";
const ONDU: &str =
    "Lifelink\nWhenever another modified creature you control dies, put two +1/+1 counters on this creature.";
const GUARDIAN: &str = "Vigilance\nWhenever a modified creature you control dies, manifest the top card of your library. (Equipment, Auras you control, and counters are modifications. To manifest a card, put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)";
const AKKI: &str = "Whenever a nontoken modified creature you control dies, create a 1/1 colorless Spirit creature token. (Equipment, Auras you control, and counters are modifications.)";
const CLOSET: &str = "This artifact enters with two +1/+1 counters on it.\n{T}: Move a +1/+1 counter from this artifact onto target creature you control. Activate only as a sorcery.\nWhenever a modified creature you control leaves the battlefield, put a +1/+1 counter on this artifact. (Equipment, Auras you control, and counters are modifications.)";

fn scenario_in_main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn add_murder(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id()
}

fn assert_resolved(outcome: &CastOutcome) {
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

fn assert_attached(state: &GameState, attachment: ObjectId, host: ObjectId) {
    assert_eq!(state.objects[&attachment].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&attachment]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(host)
    );
}

fn spirit_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.is_token
                && obj.card_types.subtypes.iter().any(|kind| kind == "Spirit")
        })
        .count()
}

fn spirits_after_murder(counter_count: u32) -> usize {
    let mut scenario = scenario_in_main_phase();
    let host = scenario.add_creature(P0, "Rumbling Baloth", 4, 4).id();
    let victim = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_plus_counters(counter_count)
        .id();
    let kami = scenario
        .add_enchantment_to_hand_from_oracle(P0, "One with the Kami", KAMI)
        .with_subtypes(vec!["Aura"])
        .id();
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    runner.cast(kami).target_object(host).resolve();
    assert_attached(runner.state(), kami, host);
    assert_eq!(
        runner.state().objects[&victim]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        counter_count
    );

    let outcome = runner.cast(murder).target_object(victim).resolve();
    outcome.assert_zone(&[victim], Zone::Graveyard);
    assert_resolved(&outcome);
    spirit_count(outcome.state())
}

#[test]
fn one_with_the_kami_counts_another_modified_creature() {
    // CR 700.9 + CR 608.2h: The counter qualifies the creature and raises its last-known power.
    assert_eq!(spirits_after_murder(1), 3);
}

#[test]
fn an_unmodified_creature_does_not_trigger() {
    // CR 700.9: The same creature without the counter does not qualify.
    assert_eq!(spirits_after_murder(0), 0);
    assert_eq!(spirits_after_murder(1), 3);
}

fn knotmaster_counters_after_murder(victim_controller: PlayerId) -> u32 {
    let mut scenario = scenario_in_main_phase();
    let ondu = scenario
        .add_creature_from_oracle(P0, "Ondu Knotmaster", 2, 2, ONDU)
        .id();
    let victim = scenario
        .add_creature(victim_controller, "Grizzly Bears", 2, 2)
        .id();
    let equipment = scenario
        .add_artifact_from_oracle(
            P1,
            "Bonesplitter",
            "Equipped creature gets +2/+0.\nEquip {1}",
        )
        .with_subtypes(vec!["Equipment"])
        .id();
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    attach_to(runner.state_mut(), equipment, victim);
    assert_attached(runner.state(), equipment, victim);
    assert_eq!(
        runner.state().objects[&victim].controller,
        victim_controller
    );
    assert_eq!(runner.state().objects[&equipment].controller, P1);

    let outcome = runner.cast(murder).target_object(victim).resolve();
    outcome.assert_zone(&[victim], Zone::Graveyard);
    assert_resolved(&outcome);
    outcome.counters(ondu, CounterType::Plus1Plus1)
}

#[test]
fn ondu_knotmaster_counts_a_creature_equipped_by_an_opponents_equipment() {
    // CR 700.9: Equipment qualifies regardless of its controller.
    assert_eq!(knotmaster_counters_after_murder(P0), 2);
}

#[test]
fn an_opponents_modified_creature_does_not_trigger() {
    // CR 603.10a: "You control" uses the departing creature's controller.
    assert_eq!(knotmaster_counters_after_murder(P1), 0);
    assert_eq!(knotmaster_counters_after_murder(P0), 2);
}

fn guardian_manifests_after_murder(victim_controller: PlayerId) -> bool {
    let mut scenario = scenario_in_main_phase();
    scenario.add_creature_from_oracle(
        victim_controller,
        "Guardian of the Forgotten",
        4,
        4,
        GUARDIAN,
    );
    let top = scenario.add_card_to_library_top(victim_controller, "Forest");
    let victim = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .controlled_by(victim_controller)
        .id();
    let aura = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Firebreathing", FIREBREATHING)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], FIREBREATHING)
        .id();
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    runner.cast(aura).target_object(victim).resolve();
    assert_attached(runner.state(), aura, victim);
    assert_eq!(runner.state().objects[&aura].controller, P0);
    assert_eq!(runner.state().objects[&victim].owner, P0);
    assert_eq!(
        runner.state().objects[&victim].controller,
        victim_controller
    );

    let outcome = runner.cast(murder).target_object(victim).resolve();
    outcome.assert_zone(&[victim], Zone::Graveyard);
    assert_resolved(&outcome);
    let card = &outcome.state().objects[&top];
    if card.zone == Zone::Battlefield {
        // CR 701.40a: Manifest puts the top card onto the battlefield as a face-down 2/2.
        assert!(card.face_down);
        assert_eq!((card.power, card.toughness), (Some(2), Some(2)));
        true
    } else {
        assert_eq!(card.zone, Zone::Library);
        false
    }
}

#[test]
fn guardian_of_the_forgotten_counts_a_creature_enchanted_by_its_controllers_aura() {
    // CR 700.9: An Aura controlled by the creature's controller qualifies.
    assert!(guardian_manifests_after_murder(P0));
}

#[test]
fn an_opponents_aura_does_not_make_a_creature_modified() {
    // CR 700.9: The Aura shares the creature's owner, but not its controller.
    assert!(!guardian_manifests_after_murder(P1));
    assert!(guardian_manifests_after_murder(P0));
}

fn akki_spirits_after_murder(kill_token: bool) -> usize {
    let mut scenario = scenario_in_main_phase();
    scenario.add_creature_from_oracle(P0, "Akki Ember-Keeper", 2, 1, AKKI);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let raise = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Battlegrowth",
            true,
            "Put a +1/+1 counter on target creature.",
        )
        .id();
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(raise).resolve();
    assert_resolved(&outcome);
    let token = outcome
        .state()
        .objects
        .values()
        .find(|obj| obj.zone == Zone::Battlefield && obj.is_token)
        .expect("Raise the Alarm creates a Soldier token")
        .id;
    let victim = if kill_token { token } else { bear };
    assert_eq!(runner.state().objects[&victim].is_token, kill_token);
    let outcome = runner.cast(growth).target_object(victim).resolve();
    outcome.assert_counters(victim, CounterType::Plus1Plus1, 1);

    let outcome = runner.cast(murder).target_object(victim).resolve();
    assert_resolved(&outcome);
    // CR 700.4 + CR 704.5d: Verify the death in the ledger even if the token ceases to exist.
    assert!(outcome.state().zone_changes_this_turn.iter().any(|record| {
        record.object_id == victim
            && record.from_zone == Some(Zone::Battlefield)
            && record.to_zone == Zone::Graveyard
    }));
    spirit_count(outcome.state())
}

#[test]
fn akki_ember_keeper_ignores_a_modified_token() {
    // CR 700.9 + CR 111.1: Both creatures were modified, and Akki requires a nontoken.
    assert_eq!(akki_spirits_after_murder(true), 0);
    assert_eq!(akki_spirits_after_murder(false), 1);
}

#[test]
fn costume_closet_counts_a_modified_creature_that_is_bounced() {
    let mut scenario = scenario_in_main_phase();
    let closet = scenario
        .add_artifact_to_hand_from_oracle(P0, "Costume Closet", CLOSET)
        .id();
    let victim = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_plus_counters(1)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(closet).resolve();
    outcome.assert_zone(&[closet], Zone::Battlefield);
    // CR 122.6a: Costume Closet enters with two counters.
    outcome.assert_counters(closet, CounterType::Plus1Plus1, 2);
    outcome.assert_counters(victim, CounterType::Plus1Plus1, 1);

    let outcome = runner.cast(unsummon).target_object(victim).resolve();
    // CR 603.10a + CR 700.9: The look-back also applies to a move to hand.
    outcome.assert_zone(&[victim], Zone::Hand);
    assert_resolved(&outcome);
    outcome.assert_counters(closet, CounterType::Plus1Plus1, 3);
}
