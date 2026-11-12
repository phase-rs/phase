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
use engine::game::replacement::ReplacementWork;
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
    // The Bear's decision is consulted again and is now P0's too, so P0 holds
    // two simultaneous decisions and orders them (CR 101.4c).
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P0,
            objects: vec![c, bear],
        }
    );
    round_trip(&mut runner);
    select_decision(&mut runner, c);
    assert_eq!(asked_choice(&runner), (P0, c));
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
    for _ in 0..256 {
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

/// Wrath of God over `n` of P1's creatures with Rest in Peace on the
/// battlefield; with `umbras`, each creature also wears Hyena Umbra and Spider
/// Umbra, so every member holds an armor choice. Returns the replacement work
/// of the whole resolution, answered through public actions.
fn wrath_work(n: usize, umbras: bool) -> ReplacementWork {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let mut attachments = Vec::new();
    let mut creatures = Vec::new();
    for _ in 0..n {
        let creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        creatures.push(creature);
        if umbras {
            attachments.push((
                umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA),
                creature,
            ));
            attachments.push((
                umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA),
                creature,
            ));
        }
    }
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    for (aura, host) in attachments {
        attach(&mut runner, aura, host);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    let ((), work) = engine::game::replacement::measure_replacement_work(|| {
        cast_until_decision(&mut runner, wrath);
        let asked = answer_all(&mut runner);
        assert_eq!(asked.len(), if umbras { n } else { 0 });
        finish(&mut runner);
    });
    let expected = if umbras {
        Zone::Battlefield
    } else {
        Zone::Exile
    };
    for creature in creatures {
        assert_eq!(zone(&runner, creature), expected);
    }
    work
}

/// Review 20: "Bound the work per pending choice and measure it on a
/// population with many replacements." Each member is consulted once through
/// the replacement pipeline, and the work per member is a constant, measured at
/// three population sizes so the fixed cost of casting cancels out:
/// - no choice: exactly two pipeline entries (the outer destruction and its
///   move, CR 701.8a) and two index rebuilds per member; the removed forecast
///   ran the pipeline twice more per member before the live consult;
/// - an Umbra armor choice per member (CR 702.89a): at most four entries and
///   four rebuilds per member, covering its consult, the answer's
///   continuation and the chosen Aura's own destruction.
#[test]
fn staged_scheduler_consults_each_member_once() {
    for umbras in [false, true] {
        let [w1, w2, w3] = [8, 16, 24].map(|n| wrath_work(n, umbras));
        let per_member = |pick: fn(&ReplacementWork) -> u64| {
            let first = (pick(&w2) - pick(&w1)) / 8;
            assert_eq!(
                pick(&w3) - pick(&w2),
                first * 8,
                "umbras={umbras}: linear in the population: {w1:?} {w2:?} {w3:?}"
            );
            first
        };
        let entries = per_member(|work| work.pipeline_entries);
        let rebuilds = per_member(|work| work.index_rebuilds);
        if umbras {
            assert!(entries <= 4, "{entries} pipeline entries per member");
            assert!(rebuilds <= 4, "{rebuilds} index rebuilds per member");
        } else {
            assert_eq!(entries, 2, "no choice: each member is consulted once");
            assert_eq!(rebuilds, 2, "one index rebuild per consult");
        }
    }
}

const FEEDCE4FC_UMBRA_ORDER: &str = include_str!("../fixtures/feedce4fc_staged_umbra_order.json");
const FEEDCE4FC_LIVE_CHILD: &str =
    include_str!("../fixtures/feedce4fc_staged_umbra_live_child.json");

/// Restore a predecessor save through checked restore.
fn restore_fixture(payload: &str) -> Result<GameRunner, String> {
    let state = serde_json::from_str::<PersistedGameState>(payload)
        .map_err(|error| error.to_string())?
        .into_game_state()
        .map_err(|error| error.to_string())?;
    state
        .resolution_stack
        .validate_at_rest(&state.waiting_for)
        .map_err(|error| error.to_string())?;
    Ok(GameRunner::from_state(state))
}

/// The staged owner's asking member and sweep cursor.
fn asking_and_cursor(runner: &GameRunner) -> (Option<ObjectId>, usize) {
    runner
        .state()
        .resolution_stack
        .iter()
        .find_map(|frame| match frame {
            engine::types::resolution::ResolutionFrame::BatchDelivery(batch) => {
                batch.staged.as_ref().map(|staged| {
                    (
                        staged.asking.map(|identity| identity.object_id),
                        staged.cursor,
                    )
                })
            }
            _ => None,
        })
        .expect("a staged owner is parked")
}

/// CR 616.1: a save written before the scheduler existed parks the member
/// being asked at the approval cursor (Day of Judgment, Grizzly Bears wearing
/// Hyena Umbra and Spider Umbra; the frozen fixture's provenance names the
/// writer). Checked restore makes that member the asking one and moves the
/// sweep past it, and the reopened choice settles the population.
#[test]
fn a_pre_scheduler_save_restores_its_member_as_the_asking_one() {
    let (bears, hyena, spider, runeclaw) = (ObjectId(1), ObjectId(2), ObjectId(3), ObjectId(4));
    let mut runner = restore_fixture(FEEDCE4FC_UMBRA_ORDER).expect("the old save restores");
    assert_eq!(runner.state().objects[&bears].name, "Grizzly Bears");
    assert_eq!(asking_and_cursor(&runner), (Some(bears), 1));
    assert_eq!(asked_choice(&runner), (P1, bears));
    choose_source(&mut runner, hyena);
    finish(&mut runner);
    assert_eq!(zone(&runner, bears), Zone::Battlefield);
    assert_eq!(zone(&runner, hyena), Zone::Graveyard);
    assert_eq!(attached_to(&runner, spider), Some(bears));
    assert_eq!(zone(&runner, runeclaw), Zone::Graveyard);
}

/// CR 616.1 + CR 702.89a: the pre-scheduler save of a live child (Hyena
/// Umbra's Aura waiting on P1's Dauthi Voidwalker vs Rest in Peace choice, with
/// no pause boundary on its host) restores with the host as the asking member,
/// and the answer settles the population.
#[test]
fn a_pre_scheduler_live_child_save_restores_its_host_as_the_asking_one() {
    let (bears, hyena, dauthi) = (ObjectId(1), ObjectId(2), ObjectId(3));
    let mut runner = restore_fixture(FEEDCE4FC_LIVE_CHILD).expect("the old save restores");
    assert_eq!(runner.state().objects[&hyena].name, "Hyena Umbra");
    assert_eq!(asking_and_cursor(&runner), (Some(bears), 1));
    assert_eq!(asked_choice(&runner), (P1, hyena));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 answers");
    finish(&mut runner);
    assert_eq!(zone(&runner, bears), Zone::Battlefield);
    assert_eq!(zone(&runner, hyena), Zone::Exile);
    assert_eq!(zone(&runner, dauthi), Zone::Exile);
}

/// Checked restore refuses a pre-scheduler save whose member is
/// mid-consultation at the cursor with no pause of its own (the first fixture
/// with its pause boundary removed): no published decision can be bound to it,
/// so the staged frame fails validation.
#[test]
fn a_pre_scheduler_save_without_its_pause_is_refused() {
    fn clear_paused(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if map.contains_key("staged") && map.contains_key("paused_current") {
                    map.insert("paused_current".to_string(), serde_json::Value::Null);
                }
                for child in map.values_mut() {
                    clear_paused(child);
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(clear_paused),
            _ => {}
        }
    }
    let mut value: serde_json::Value =
        serde_json::from_str(FEEDCE4FC_UMBRA_ORDER).expect("the fixture is JSON");
    clear_paused(&mut value);
    let error = match restore_fixture(&value.to_string()) {
        Ok(_) => panic!("a mid-consultation member with no pause must not restore"),
        Err(error) => error,
    };
    assert!(
        error.contains("a consulted staged member follows the approval cursor"),
        "{error}"
    );
}

/// CR 101.4c: at the replacement decision order prompt, the AI's legal actions
/// are exactly one selection per held decision, and each is accepted.
#[test]
fn ai_offers_one_selection_per_held_decision() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let first_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let first_spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
    let second = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let second_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let second_spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    attach(&mut runner, first_hyena, first);
    attach(&mut runner, first_spider, first);
    attach(&mut runner, second_hyena, second);
    attach(&mut runner, second_spider, second);
    engine::game::layers::flush_layers(runner.state_mut());
    cast_until_decision(&mut runner, wrath);

    let mut legal = engine::ai_support::legal_actions(runner.state());
    legal.sort_by_key(|action| format!("{action:?}"));
    let mut expected = vec![
        GameAction::SelectCards { cards: vec![first] },
        GameAction::SelectCards {
            cards: vec![second],
        },
    ];
    expected.sort_by_key(|action| format!("{action:?}"));
    assert_eq!(legal, expected);
    for action in legal {
        let mut probe = GameRunner::from_state(runner.state().clone());
        probe
            .act(action)
            .expect("each offered selection is accepted");
        assert!(matches!(
            probe.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ));
    }
}

/// CR 104.3a + CR 800.4a + CR 903.9b: Day of Judgment in a three-player
/// Commander game. P1's commander is Progenitus, whose redirect to its owner's
/// library carries a shuffle that waits above the staged batch while P1 makes
/// the CR 903.9b command-zone choice. P1 concedes at that choice; Progenitus
/// leaves the game with P1, and the batch, still below the shuffle, delivers
/// P2's Grizzly Bears and settles.
#[test]
fn a_commander_owner_concedes_at_the_command_zone_choice_and_the_batch_settles() {
    let p2 = PlayerId(2);
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let progenitus = scenario
        .add_creature_from_oracle(P1, "Progenitus", 10, 10, PROGENITUS)
        .as_legendary()
        .with_subtypes(vec!["Hydra", "Avatar"])
        .commander()
        .id();
    let bears = scenario.add_creature(p2, "Grizzly Bears", 2, 2).id();
    let judgment = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);
    let mut runner = scenario.build();

    cast_until_decision(&mut runner, judgment);
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the CR 903.9b choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.description == "Move to command zone"));
    assert_eq!(zone(&runner, bears), Zone::Battlefield);
    round_trip(&mut runner);
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes at the command-zone choice");
    assert!(runner.state().players[1].is_eliminated);
    finish(&mut runner);
    assert_eq!(zone(&runner, bears), Zone::Graveyard);
    assert_eq!(zone(&runner, judgment), Zone::Graveyard);
    let _ = progenitus;
}

const THALIA_HERETIC_CATHAR: &str =
    "First strike\nCreatures and nonbasic lands your opponents control enter tapped.";

/// CR 101.4c + CR 616.1f: Open the Vaults returns two of P0's Ornithopters.
/// Root Maze ("Artifacts and lands enter tapped"), Archelos ("As long as
/// Archelos is untapped, other permanents enter untapped") and P1's Thalia,
/// Heretic Cathar ("Creatures and nonbasic lands your opponents control enter
/// tapped") all apply to each entry, so P0 holds two decisions. After P0
/// applies one replacement to the first Ornithopter, its entry still needs an
/// order between the remaining two: that new choice is held, and P0 again
/// chooses which decision to make next.
#[test]
fn a_further_choice_on_the_same_entry_is_ordered_with_the_others() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let maze = scenario
        .add_enchantment_from_oracle(P0, "Root Maze", ROOT_MAZE)
        .id();
    scenario.add_creature_from_oracle(P1, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS);
    scenario
        .add_creature_from_oracle(P1, "Thalia, Heretic Cathar", 3, 2, THALIA_HERETIC_CATHAR)
        .as_legendary();
    let thopter = |scenario: &mut GameScenario| {
        scenario
            .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
            .as_artifact_creature()
            .from_oracle_text("Flying")
            .id()
    };
    let first = thopter(&mut scenario);
    let second = thopter(&mut scenario);
    let vaults = zero_cost_spell(&mut scenario, "Open the Vaults", OPEN_THE_VAULTS);
    let mut runner = scenario.build();

    cast_until_decision(&mut runner, vaults);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P0,
            objects: vec![first, second],
        }
    );
    select_decision(&mut runner, first);
    assert_eq!(asked_choice(&runner), (P0, first));
    choose_source(&mut runner, maze);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::ReplacementDecisionOrder {
            player: P0,
            objects: vec![first, second],
        },
        "the first entry's further choice is ordered with the second's"
    );
    round_trip(&mut runner);
    answer_all(&mut runner);
    finish(&mut runner);
    assert_eq!(zone(&runner, first), Zone::Battlefield);
    assert_eq!(zone(&runner, second), Zone::Battlefield);
}

/// CR 101.4 + CR 800.4a: Wrath of God in a three-player game. P1's creature
/// wears Hyena Umbra and Spider Umbra, so P1 chooses its armor. P0's Runeclaw
/// Bear is controlled by P2 through Control Magic, and P1's Dauthi Voidwalker
/// and Rest in Peace both apply to its move, so P2 holds that choice; P1 is
/// asked first. P2 concedes: Control Magic leaves the game, the Bear returns to
/// P0, and its choice becomes P0's. P0 is earlier in APNAP order than P1, so
/// P0's choice is asked before P1's.
#[test]
fn a_departure_that_hands_a_choice_to_an_earlier_seat_restarts_apnap() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let c = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let c_hyena = umbra(&mut scenario, P1, "Hyena Umbra", HYENA_UMBRA);
    let c_spider = umbra(&mut scenario, P1, "Spider Umbra", SPIDER_UMBRA);
    let bear = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let magic = umbra(&mut scenario, p2, "Control Magic", CONTROL_MAGIC);
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH_OF_GOD);
    let mut runner = scenario.build();
    attach(&mut runner, c_hyena, c);
    attach(&mut runner, c_spider, c);
    attach(&mut runner, magic, bear);
    engine::game::layers::flush_layers(runner.state_mut());

    cast_until_decision(&mut runner, wrath);
    assert_eq!(asked_choice(&runner), (P1, c), "P0 has no choice yet");
    runner
        .act(GameAction::Concede { player_id: p2 })
        .expect("P2 concedes while P1's choice is open");
    assert_eq!(runner.state().objects[&bear].controller, P0);
    assert_eq!(
        asked_choice(&runner),
        (P0, bear),
        "P0's choice now comes first"
    );
    round_trip(&mut runner);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 answers");
    assert_eq!(asked_choice(&runner), (P1, c));
    choose_source(&mut runner, c_hyena);
    answer_all(&mut runner);
    finish(&mut runner);
    assert_eq!(zone(&runner, bear), Zone::Exile);
    assert_eq!(zone(&runner, c), Zone::Battlefield);
}
