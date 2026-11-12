//! CR 616.1 + CR 101.4: the staged population's live replacement-choice
//! scheduler. Every member is consulted once; a member whose consultation parks
//! a decision holds it; the outstanding decisions are asked in APNAP order
//! (CR 101.4); a player with several chooses which comes first (CR 101.4c); and
//! the order restarts after every answer (CR 101.4d). Printed Oracle text is
//! quoted from the card data.
//!
//! Infrastructure exception: Auras are attached and control is changed by
//! setting the objects directly, instead of casting them; every destruction
//! population is produced by a public cast.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::actions::GameAction;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const WRATH_OF_GOD: &str = "Destroy all creatures. They can't be regenerated.";
const DAY_OF_JUDGMENT: &str = "Destroy all creatures.";
const HYENA_UMBRA: &str = "Enchant creature\nEnchanted creature gets +1/+1 and has first strike.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";
const SPIDER_UMBRA: &str = "Enchant creature\nEnchanted creature gets +1/+1 and has reach. (It can block creatures with flying.)\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";
const FELIDAR_UMBRA: &str = "Enchant creature\nEnchanted creature has lifelink.\n{1}{W}: Attach this Aura to target creature you control.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";
const DAUTHI_VOIDWALKER: &str = "Shadow (This creature can block or be blocked by only creatures with shadow.)\nIf a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const PROGENITUS: &str = "Protection from everything\nIf Progenitus would be put into a graveyard from anywhere, reveal Progenitus and shuffle it into its owner's library instead.";

fn zero_cost_spell(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn umbra(scenario: &mut GameScenario, owner: PlayerId, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(owner, name, oracle)
        .with_subtypes(vec!["Aura"])
        .id()
}

fn attach(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn attached_to(runner: &GameRunner, aura: ObjectId) -> Option<ObjectId> {
    match runner.state().objects[&aura].attached_to {
        Some(AttachTarget::Object(host)) => Some(host),
        _ => None,
    }
}

/// Save at the current prompt, validate it at rest, and reopen it.
fn round_trip(runner: &mut GameRunner) {
    runner
        .state()
        .resolution_stack
        .validate_at_rest(&runner.state().waiting_for)
        .expect("the prompt validates before the save");
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the prompt serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the prompt decodes")
        .into_game_state()
        .expect("the prompt restores through checked restore");
    restored
        .resolution_stack
        .validate_at_rest(&restored.waiting_for)
        .expect("the reopened prompt validates");
    *runner.state_mut() = restored;
}

/// Cast `spell` and pass priority until the population's first decision.
fn cast_until_decision(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).commit();
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
}

/// The asked replacement choice: its chooser and the object its event is about.
fn asked_choice(runner: &GameRunner) -> (PlayerId, ObjectId) {
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected a replacement choice, got {:?}",
            runner.state().waiting_for
        );
    };
    let subject = runner
        .state()
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.proposed.affected_object_id())
        .expect("the published decision is about an object");
    (player, subject)
}

/// Apply the replacement whose source is `source`.
fn choose_source(runner: &mut GameRunner, source: ObjectId) {
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!("expected a replacement choice");
    };
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == source)
        .expect("the source is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the chooser answers");
}

fn select_decision(runner: &mut GameRunner, object: ObjectId) {
    runner
        .act(GameAction::SelectCards {
            cards: vec![object],
        })
        .expect("the chooser selects which decision to make first");
}

fn finish(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "{:?}",
        state.resolution_stack
    );
    state
        .resolution_stack
        .validate_at_rest(&state.waiting_for)
        .expect("the population settles");
}

/// CR 101.4c + CR 616.1 + CR 702.89a: Wrath of God says "Destroy all
/// creatures. They can't be regenerated." P1's two creatures each wear Hyena
/// Umbra and Felidar Umbra, so P1 has two simultaneous choices of which armor
/// applies. P1 chooses which creature's choice to make first: the second.
/// Each answer destroys exactly the chosen Aura; the other stays attached.
#[test]
fn wrath_of_god_lets_one_player_order_their_umbra_choices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let first_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let first_felidar = umbra(&mut scenario, P1, "Felidar Umbra", FELIDAR_UMBRA);
    let second = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let second_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let second_felidar = umbra(&mut scenario, P1, "Felidar Umbra", FELIDAR_UMBRA);
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    attach(&mut runner, first_hyena, first);
    attach(&mut runner, first_felidar, first);
    attach(&mut runner, second_hyena, second);
    attach(&mut runner, second_felidar, second);
    engine::game::layers::flush_layers(runner.state_mut());

    cast_until_decision(&mut runner, wrath);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P1,
            objects: vec![first, second],
        },
        "P1 holds both choices and chooses which to make first"
    );
    round_trip(&mut runner);
    select_decision(&mut runner, second);
    assert_eq!(asked_choice(&runner), (P1, second));
    choose_source(&mut runner, second_felidar);
    // One decision is left, so it is asked without a selection.
    assert_eq!(asked_choice(&runner), (P1, first));
    round_trip(&mut runner);
    choose_source(&mut runner, first_hyena);
    finish(&mut runner);

    assert_eq!(zone(&runner, first), Zone::Battlefield);
    assert_eq!(zone(&runner, second), Zone::Battlefield);
    assert_eq!(zone(&runner, second_felidar), Zone::Graveyard);
    assert_eq!(attached_to(&runner, second_hyena), Some(second));
    assert_eq!(zone(&runner, first_hyena), Zone::Graveyard);
    assert_eq!(attached_to(&runner, first_felidar), Some(first));
    assert_eq!(zone(&runner, wrath), Zone::Graveyard);
}

/// Wrath of God over P0's two Umbra-armored creatures: C1 wears P1's Hyena
/// Umbra and P0's Spider Umbra, C2 wears P0's Hyena Umbra and Spider Umbra.
/// P0 controls Dauthi Voidwalker; Rest in Peace is on the battlefield. Returns
/// the runner at its first decision and (c1, p1_hyena, c2, c2_hyena).
fn active_player_umbra_board() -> (GameRunner, [ObjectId; 4]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER);
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let c1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let p1_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let c1_spider = umbra(&mut scenario, P0, "Spider Umbra", SPIDER_UMBRA);
    let c2 = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let c2_hyena = umbra(&mut scenario, P0, "Hyena Umbra", HYENA_UMBRA);
    let c2_spider = umbra(&mut scenario, P0, "Spider Umbra", SPIDER_UMBRA);
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    attach(&mut runner, p1_hyena, c1);
    attach(&mut runner, c1_spider, c1);
    attach(&mut runner, c2_hyena, c2);
    attach(&mut runner, c2_spider, c2);
    engine::game::layers::flush_layers(runner.state_mut());
    cast_until_decision(&mut runner, wrath);
    (runner, [c1, p1_hyena, c2, c2_hyena])
}

/// CR 101.4 + CR 101.4d + CR 616.1: P0, the active player, holds two armor
/// choices. Applying P1's Hyena Umbra to C1 destroys that Aura, and its move to
/// P1's graveyard asks P1 to choose between Dauthi Voidwalker ("If a card would
/// be put into an opponent's graveyard from anywhere, instead exile it with a
/// void counter on it") and Rest in Peace. With APNAP restarted, P0's
/// remaining choice for C2 still comes before P1's.
#[test]
fn active_player_finishes_their_choices_before_one_their_answer_exposes() {
    let (mut runner, [c1, p1_hyena, c2, c2_hyena]) = active_player_umbra_board();
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P0,
            objects: vec![c1, c2],
        }
    );
    select_decision(&mut runner, c1);
    assert_eq!(asked_choice(&runner), (P0, c1));
    choose_source(&mut runner, p1_hyena);
    round_trip(&mut runner);
    assert_eq!(
        asked_choice(&runner),
        (P0, c2),
        "P0 still has a choice, so P1's newly exposed choice waits"
    );
    choose_source(&mut runner, c2_hyena);
    assert_eq!(asked_choice(&runner), (P1, p1_hyena));
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        unreachable!()
    };
    assert_eq!(candidates.len(), 2, "Dauthi Voidwalker vs Rest in Peace");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 answers");
    finish(&mut runner);
    assert_eq!(zone(&runner, c1), Zone::Battlefield);
    assert_eq!(zone(&runner, c2), Zone::Battlefield);
    assert_eq!(zone(&runner, p1_hyena), Zone::Exile);
    assert_eq!(zone(&runner, c2_hyena), Zone::Exile);
}

/// Day of Judgment over P1's two Umbra-armored creatures: C1 wears P0's Hyena
/// Umbra and P1's Spider Umbra, C2 wears P1's Hyena Umbra and Spider Umbra.
/// P1 controls Dauthi Voidwalker; Rest in Peace is on the battlefield.
///
/// CR 101.4d + CR 616.1: P1 holds both armor choices. Applying P0's Hyena Umbra
/// to C1 destroys that Aura, and its move to P0's graveyard asks P0, an
/// earlier seat, to choose between Dauthi Voidwalker and Rest in Peace. APNAP
/// order restarts: P0 is asked before P1's remaining choice for C2.
#[test]
fn an_answer_that_exposes_an_earlier_seats_choice_restarts_apnap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let c1 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let p0_hyena = umbra(&mut scenario, P0, "Hyena Umbra", HYENA_UMBRA);
    let c1_spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
    let c2 = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let c2_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let c2_spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
    let judgment = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);
    let mut runner = scenario.build();
    attach(&mut runner, p0_hyena, c1);
    attach(&mut runner, c1_spider, c1);
    attach(&mut runner, c2_hyena, c2);
    attach(&mut runner, c2_spider, c2);
    engine::game::layers::flush_layers(runner.state_mut());
    cast_until_decision(&mut runner, judgment);

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P1,
            objects: vec![c1, c2],
        }
    );
    select_decision(&mut runner, c1);
    choose_source(&mut runner, p0_hyena);
    assert_eq!(
        asked_choice(&runner),
        (P0, p0_hyena),
        "P0's newly exposed choice comes before P1's remaining one"
    );
    round_trip(&mut runner);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 answers");
    assert_eq!(asked_choice(&runner), (P1, c2));
    choose_source(&mut runner, c2_hyena);
    finish(&mut runner);
    assert_eq!(zone(&runner, c1), Zone::Battlefield);
    assert_eq!(zone(&runner, c2), Zone::Battlefield);
    assert_eq!(zone(&runner, p0_hyena), Zone::Exile);
}

/// CR 903.9b + CR 101.4 + CR 616.1: Day of Judgment says "Destroy all
/// creatures." Each player's commander is a Progenitus ("If Progenitus would be
/// put into a graveyard from anywhere, reveal Progenitus and shuffle it into
/// its owner's library instead"), and each player controls the other's. The
/// redirect exposes CR 903.9b ("its owner may put it into the command zone
/// instead"), so both owners choose: P0, the active player, first, although
/// P1's commander is earlier on the battlefield. Either answer is that member's
/// approval, so the batch goes on to P1 and both commanders land where their
/// owners chose, once each. The game is saved and reopened at the first choice.
#[test]
fn day_of_judgment_asks_exchanged_progenitus_commanders_in_apnap_order() {
    for p0_takes_command in [true, false] {
        let case = format!("p0_takes_command={p0_takes_command}");
        let mut scenario =
            GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let progenitus = |scenario: &mut GameScenario, owner: PlayerId| {
            scenario
                .add_creature_from_oracle(owner, "Progenitus", 10, 10, PROGENITUS)
                .as_legendary()
                .with_subtypes(vec!["Hydra", "Avatar"])
                .commander()
                .id()
        };
        let p1_commander = progenitus(&mut scenario, P1);
        let p0_commander = progenitus(&mut scenario, P0);
        let judgment = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);
        let mut runner = scenario.build();
        // Infrastructure exception: the exchanged control (an Insurrection-style
        // effect) is set on the objects directly.
        runner
            .state_mut()
            .objects
            .get_mut(&p1_commander)
            .unwrap()
            .controller = P0;
        runner
            .state_mut()
            .objects
            .get_mut(&p0_commander)
            .unwrap()
            .controller = P1;
        engine::game::layers::flush_layers(runner.state_mut());

        cast_until_decision(&mut runner, judgment);
        round_trip(&mut runner);
        let mut asked = Vec::new();
        for _ in 0..4 {
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = runner.state().waiting_for.clone()
            else {
                break;
            };
            asked.push(player);
            let take_command = if player == P0 {
                p0_takes_command
            } else {
                !p0_takes_command
            };
            let index = candidates
                .iter()
                .position(|candidate| {
                    (candidate.description == "Move to command zone") == take_command
                })
                .expect("both CR 903.9b branches are offered");
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("the owner answers");
        }
        assert_eq!(asked, vec![P0, P1], "{case}: owners choose APNAP");
        finish(&mut runner);
        let (command, library) = if p0_takes_command {
            (p0_commander, p1_commander)
        } else {
            (p1_commander, p0_commander)
        };
        assert_eq!(zone(&runner, command), Zone::Command, "{case}");
        assert_eq!(zone(&runner, library), Zone::Library, "{case}");
    }
}

const CONTROL_MAGIC: &str = "Enchant creature\nYou control enchanted creature.";

/// P0 (active) controls Grizzly Bears C wearing P0's Hyena Umbra and Spider
/// Umbra. Returns (c, c_hyena).
fn active_two_umbra_creature(scenario: &mut GameScenario) -> (ObjectId, ObjectId, ObjectId) {
    let c = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let hyena = umbra(scenario, P0, "Hyena Umbra", HYENA_UMBRA);
    let spider = umbra(scenario, P0, "Spider Umbra", SPIDER_UMBRA);
    (c, hyena, spider)
}

/// CR 104.3a + CR 800.4a + CR 616.1: Wrath of God in a three-player game. P2
/// controls P0's Runeclaw Bear through Control Magic ("You control enchanted
/// creature"); P1 controls Dauthi Voidwalker and Rest in Peace is on the
/// battlefield, so the Bear's move to P0's graveyard is P2's choice, held while
/// P0 is asked about their own creature's Umbra armor. P2 concedes: Control
/// Magic leaves the game with P2, the Bear's control returns to P0, and its
/// decision is asked of P0. Nothing is lost and the batch finishes.
#[test]
fn a_held_decisions_chooser_concedes_and_its_owner_decides() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let (c, c_hyena, c_spider) = active_two_umbra_creature(&mut scenario);
    let bear = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let magic = umbra(&mut scenario, p2, "Control Magic", CONTROL_MAGIC);
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    attach(&mut runner, c_hyena, c);
    attach(&mut runner, c_spider, c);
    attach(&mut runner, magic, bear);
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&bear].controller, p2);

    cast_until_decision(&mut runner, wrath);
    assert_eq!(
        asked_choice(&runner),
        (P0, c),
        "P0 is asked first (CR 101.4)"
    );
    runner
        .act(GameAction::Concede { player_id: p2 })
        .expect("P2 concedes while P0's choice is open");
    assert!(runner.state().players[2].is_eliminated);
    assert_eq!(runner.state().objects[&bear].controller, P0);
    assert_eq!(asked_choice(&runner), (P0, c));
    round_trip(&mut runner);
    choose_source(&mut runner, c_hyena);
    // The destroyed Hyena Umbra is P0's card and P1 controls Dauthi
    // Voidwalker, so its move is P0's choice too: P0 holds two decisions.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P0,
            objects: vec![c_hyena, bear],
        },
        "the Bear's decision is now P0's, beside the Aura's"
    );
    select_decision(&mut runner, bear);
    assert_eq!(asked_choice(&runner), (P0, bear));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 answers for the Bear");
    assert_eq!(asked_choice(&runner), (P0, c_hyena));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 answers for the Aura");
    finish(&mut runner);
    assert_eq!(zone(&runner, bear), Zone::Exile);
    assert_eq!(zone(&runner, c), Zone::Battlefield);
    assert_eq!(zone(&runner, c_hyena), Zone::Exile);
    assert_eq!(attached_to(&runner, c_spider), Some(c));
}

/// CR 101.4c + CR 104.3a + CR 800.4a: Wrath of God in a three-player game.
/// P1 holds two Umbra armor choices and is choosing which to make first when
/// P1 concedes; both creatures leave the game with P1. P2's creature with no
/// choice is destroyed, and the batch finishes.
#[test]
fn the_selecting_player_concedes_and_the_batch_finishes() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let first_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let first_spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
    let second = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let second_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let second_spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
    let bystander = scenario.add_creature(p2, "Elvish Mystic", 1, 1).id();
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    attach(&mut runner, first_hyena, first);
    attach(&mut runner, first_spider, first);
    attach(&mut runner, second_hyena, second);
    attach(&mut runner, second_spider, second);
    engine::game::layers::flush_layers(runner.state_mut());

    cast_until_decision(&mut runner, wrath);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P1,
            objects: vec![first, second],
        }
    );
    round_trip(&mut runner);
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes at the selection");
    assert!(runner.state().players[1].is_eliminated);
    finish(&mut runner);
    assert_eq!(zone(&runner, bystander), Zone::Graveyard);
    assert_eq!(zone(&runner, wrath), Zone::Graveyard);
}

/// CR 104.3a + CR 800.4a + CR 702.89a: Wrath of God in a three-player game.
/// P1's Runeclaw Bear wears P0's Hyena Umbra, so the armor destroys that Aura
/// instead, and the Aura's move to P0's graveyard asks P0 to choose between
/// P2's Dauthi Voidwalker and Rest in Peace: a live child held on the Bear. P0
/// also holds an armor choice for their own creature and selects it first. P1
/// concedes while that is asked; the Bear leaves the game, but the Aura is
/// P0's and its decision is still asked, after which the batch finishes.
#[test]
fn a_live_child_survives_its_hosts_owner_conceding() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(p2, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER);
    scenario.add_enchantment_from_oracle(p2, "Rest in Peace", REST_IN_PEACE);
    let (c, c_hyena, c_spider) = active_two_umbra_creature(&mut scenario);
    let host = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let p0_hyena = umbra(&mut scenario, P0, "Hyena Umbra", HYENA_UMBRA);
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    attach(&mut runner, c_hyena, c);
    attach(&mut runner, c_spider, c);
    attach(&mut runner, p0_hyena, host);
    engine::game::layers::flush_layers(runner.state_mut());

    cast_until_decision(&mut runner, wrath);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P0,
            objects: vec![c, p0_hyena],
        },
        "P0 holds C's armor choice and the Aura's Dauthi/Rest in Peace choice"
    );
    select_decision(&mut runner, c);
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes while P0's choice is open");
    assert!(runner.state().players[1].is_eliminated);
    round_trip(&mut runner);
    choose_source(&mut runner, c_spider);
    // The destroyed Spider Umbra is P0's card and P2 controls Dauthi
    // Voidwalker, so its move is P0's choice as well.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P0,
            objects: vec![c_spider, p0_hyena],
        },
        "the departed host's Aura decision survives beside the new one"
    );
    select_decision(&mut runner, p0_hyena);
    assert_eq!(asked_choice(&runner), (P0, p0_hyena));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 answers for the departed host's Aura");
    assert_eq!(asked_choice(&runner), (P0, c_spider));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 answers for the Spider Umbra");
    finish(&mut runner);
    assert_eq!(zone(&runner, p0_hyena), Zone::Exile);
    assert_eq!(zone(&runner, c_spider), Zone::Exile);
    assert_eq!(zone(&runner, c), Zone::Battlefield);
    assert_eq!(attached_to(&runner, c_hyena), Some(c));
}

const AKROMAS_VENGEANCE: &str = "Destroy all artifacts, creatures, and enchantments.\nCycling {3} ({3}, Discard this card: Draw a card.)";

/// Answer every replacement choice with its first candidate, recording each
/// asked choice's chooser and subject; selections pick announcement order.
fn answer_all(runner: &mut GameRunner) -> Vec<(PlayerId, ObjectId)> {
    let mut asked = Vec::new();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementDecisionOrder { objects, .. } => {
                select_decision(runner, objects[0]);
            }
            WaitingFor::ReplacementChoice { .. } => {
                asked.push(asked_choice(runner));
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the chooser answers");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    asked
}

/// CR 616.1 + CR 702.89a: Akroma's Vengeance destroys P1's creature and the
/// Hyena Umbra on it at once. The armor destroys the Aura instead of the
/// creature, so the Aura's one move to P1's graveyard (Dauthi Voidwalker vs
/// Rest in Peace, P1's choice) is asked exactly once, whether the Aura is
/// consulted before or after its host: the host's decision decides the Aura,
/// which waits on it.
#[test]
fn akromas_vengeance_asks_once_about_an_umbra_in_either_order() {
    for aura_first in [true, false] {
        let case = format!("aura_first={aura_first}");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dauthi = scenario
            .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
            .id();
        let peace = scenario
            .add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE)
            .id();
        let (hyena, host) = if aura_first {
            let hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
            (hyena, scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
        } else {
            let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
            (umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA), host)
        };
        let vengeance = zero_cost_spell(&mut scenario, "Akroma's Vengeance", AKROMAS_VENGEANCE);
        let mut runner = scenario.build();
        attach(&mut runner, hyena, host);
        engine::game::layers::flush_layers(runner.state_mut());

        cast_until_decision(&mut runner, vengeance);
        round_trip(&mut runner);
        let asked = answer_all(&mut runner);
        assert_eq!(
            asked,
            vec![(P1, hyena)],
            "{case}: one question about the Aura"
        );
        finish(&mut runner);
        assert_eq!(zone(&runner, host), Zone::Battlefield, "{case}");
        assert_eq!(zone(&runner, hyena), Zone::Exile, "{case}");
        assert_eq!(zone(&runner, dauthi), Zone::Exile, "{case}");
        assert_eq!(zone(&runner, peace), Zone::Exile, "{case}");
    }
}

/// CR 616.1 + CR 702.89a: the host wears Hyena Umbra and Spider Umbra, both
/// population members of Akroma's Vengeance. Whichever order the battlefield
/// lists them, P1 first chooses which armor applies; neither Aura is asked
/// about its own move before that, because the armor decides one of them.
/// Then the chosen Aura's move and the other Aura's own move are asked, once
/// each.
#[test]
fn akromas_vengeance_asks_the_armor_before_either_aura_in_either_order() {
    for auras_first in [true, false] {
        let case = format!("auras_first={auras_first}");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER);
        scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
        let host_first = !auras_first;
        let early_host = host_first.then(|| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id());
        let hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
        let spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
        let host =
            early_host.unwrap_or_else(|| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id());
        let vengeance = zero_cost_spell(&mut scenario, "Akroma's Vengeance", AKROMAS_VENGEANCE);
        let mut runner = scenario.build();
        attach(&mut runner, hyena, host);
        attach(&mut runner, spider, host);
        engine::game::layers::flush_layers(runner.state_mut());

        cast_until_decision(&mut runner, vengeance);
        assert_eq!(asked_choice(&runner), (P1, host), "{case}: the armor first");
        round_trip(&mut runner);
        choose_source(&mut runner, hyena);
        // P1 now holds the chosen Hyena Umbra's move (a live child of the
        // host) and Spider Umbra's own move: two simultaneous choices, so P1
        // orders them (CR 101.4c). Each is asked exactly once.
        let mut asked = answer_all(&mut runner);
        asked.sort();
        let mut expected = vec![(P1, hyena), (P1, spider)];
        expected.sort();
        assert_eq!(asked, expected, "{case}: then each Aura's own move, once");
        finish(&mut runner);
        assert_eq!(zone(&runner, host), Zone::Battlefield, "{case}");
        assert_eq!(zone(&runner, hyena), Zone::Exile, "{case}");
        assert_eq!(zone(&runner, spider), Zone::Exile, "{case}");
    }
}

const OPEN_THE_VAULTS: &str = "Return all artifact and enchantment cards from all graveyards to the battlefield under their owners' control. (Auras with nothing to enchant remain in graveyards.)";
const ROOT_MAZE: &str = "Artifacts and lands enter tapped.";
const ARCHELOS: &str = "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.";

/// CR 101.4 + CR 616.1: Open the Vaults returns an Ornithopter to each player.
/// Root Maze ("Artifacts and lands enter tapped") and Archelos ("As long as
/// Archelos is untapped, other permanents enter untapped") both apply to each
/// entry, so each entrant's owner chooses. P0, the active player, is asked
/// first even when P1's card is earlier in the graveyards; the game is saved
/// and reopened at that first choice.
#[test]
fn open_the_vaults_asks_entry_choices_in_apnap_order() {
    for p1_card_first in [true, false] {
        let case = format!("p1_card_first={p1_card_first}");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(P0, "Root Maze", ROOT_MAZE);
        scenario.add_creature_from_oracle(P1, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS);
        let thopter = |scenario: &mut GameScenario, owner: PlayerId| {
            scenario
                .add_creature_to_graveyard(owner, "Ornithopter", 0, 2)
                .as_artifact_creature()
                .from_oracle_text("Flying")
                .id()
        };
        let (p0_thopter, p1_thopter) = if p1_card_first {
            let p1 = thopter(&mut scenario, P1);
            (thopter(&mut scenario, P0), p1)
        } else {
            let p0 = thopter(&mut scenario, P0);
            (p0, thopter(&mut scenario, P1))
        };
        let vaults = zero_cost_spell(&mut scenario, "Open the Vaults", OPEN_THE_VAULTS);
        let mut runner = scenario.build();

        cast_until_decision(&mut runner, vaults);
        assert_eq!(asked_choice(&runner), (P0, p0_thopter), "{case}");
        round_trip(&mut runner);
        let asked = answer_all(&mut runner);
        assert_eq!(asked, vec![(P0, p0_thopter), (P1, p1_thopter)], "{case}");
        finish(&mut runner);
        assert_eq!(zone(&runner, p0_thopter), Zone::Battlefield, "{case}");
        assert_eq!(zone(&runner, p1_thopter), Zone::Battlefield, "{case}");
    }
}

const PITHING_NEEDLE: &str = "As this artifact enters, choose a card name.\nActivated abilities of sources with the chosen name can't be activated unless they're mana abilities.";
const PHYREXIAN_PROCESSOR: &str = "As this artifact enters, pay any amount of life.\n{4}, {T}: Create an X/X black Phyrexian Minion creature token, where X is the life paid as this artifact entered.";

/// CR 614.12a + CR 101.4 + CR 616.1: Open the Vaults returns P1's Pithing
/// Needle ("As this artifact enters, choose a card name") and P0's Phyrexian
/// Processor ("As this artifact enters, pay any amount of life"). Both
/// as-enters choices are made before the artifacts enter, P0's first although
/// the Needle is earlier in the graveyards. Each held prompt is restored as the
/// replacement raised it, through a save and reopen at each.
#[test]
fn open_the_vaults_asks_as_enters_choices_in_apnap_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let needle = scenario
        .add_creature_to_graveyard(P1, "Pithing Needle", 0, 0)
        .as_artifact()
        .from_oracle_text(PITHING_NEEDLE)
        .id();
    let processor = scenario
        .add_creature_to_graveyard(P0, "Phyrexian Processor", 0, 0)
        .as_artifact()
        .from_oracle_text(PHYREXIAN_PROCESSOR)
        .id();
    let vaults = zero_cost_spell(&mut scenario, "Open the Vaults", OPEN_THE_VAULTS);
    let mut runner = scenario.build();
    // Infrastructure exception: the card-name universe a name is chosen from.
    runner.state_mut().all_card_names = std::sync::Arc::from(["Llanowar Elves".to_string()]);
    let life_before = runner.life(P0);

    cast_until_decision(&mut runner, vaults);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { player: P0, .. }
        ),
        "P0's life payment comes first: {:?}",
        runner.state().waiting_for
    );
    round_trip(&mut runner);
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("P0 pays 3 life");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice { player: P1, .. }
        ),
        "then P1 names a card: {:?}",
        runner.state().waiting_for
    );
    round_trip(&mut runner);
    // The card-name universe is runtime data that a reopen rehydrates.
    runner.state_mut().all_card_names = std::sync::Arc::from(["Llanowar Elves".to_string()]);
    let named = "Llanowar Elves".to_string();
    runner
        .act(GameAction::ChooseOption {
            choice: named.clone(),
        })
        .expect("P1 names a card");
    finish(&mut runner);
    assert_eq!(zone(&runner, processor), Zone::Battlefield);
    assert_eq!(zone(&runner, needle), Zone::Battlefield);
    assert_eq!(runner.life(P0), life_before - 3);
    assert!(runner.state().objects[&needle]
        .chosen_attributes
        .iter()
        .any(|attribute| matches!(attribute, ChosenAttribute::CardName(name) if *name == named)));
}
