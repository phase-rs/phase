//! CR 602.2b + CR 601.2g-h: an activated ability is paid the way a spell is.
//! The player activates mana abilities while paying, so an activation in
//! `CastPaymentMode::Manual` stops in `ManaPayment` once its X and targets
//! are announced, exactly as a manual cast does, and nothing is tapped for the
//! player. Cancelling there reverses the whole activation (CR 733.1).
//!
//! https://github.com/phase-rs/phase/issues/6016

use crate::support::shared_card_db;
use engine::ai_support::legal_actions_full;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, StaticCondition, StaticDefinition, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::casting_costs::{ActivationCostLock, ActivationCostLockPoint};
use engine::types::counter::CounterType;
use engine::types::events::{BoundActivationCost, GameEvent};
use engine::types::game_state::{
    CastPaymentMode, CounterRemoveChoice, PayCostKind, PersistedGameState,
    PersistedRestoreFinalization, ShardChoice, ShardOptions, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

struct EquipBoard {
    runner: GameRunner,
    bonesplitter: ObjectId,
    bears: ObjectId,
    sol_ring: ObjectId,
    mountain: ObjectId,
}

/// Bonesplitter (Equip {1}) and a creature to equip, with Sol Ring and a
/// Mountain untapped: either source could pay the {1}.
fn equip_board() -> EquipBoard {
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bonesplitter = scenario.add_real_card(P0, "Bonesplitter", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db);
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    EquipBoard {
        runner,
        bonesplitter,
        bears,
        sol_ring,
        mountain,
    }
}

/// The index of the only non-mana activated ability `source` offers now.
fn activated_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped
        .get(&source)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::ActivateAbility { ability_index, .. } => Some(*ability_index),
            _ => None,
        })
        .expect("the source's activated ability is offered")
}

fn activate(
    runner: &mut GameRunner,
    source_id: ObjectId,
    payment_mode: CastPaymentMode,
) -> WaitingFor {
    let ability_index = activated_ability_index(runner, source_id);
    runner
        .act(GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode,
        })
        .expect("the activation begins")
        .waiting_for
}

/// Answer a target prompt with `targets`, if the engine asks one.
fn choose_targets(runner: &mut GameRunner, targets: &[ObjectId]) -> WaitingFor {
    if !matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        return runner.state().waiting_for.clone();
    }
    runner
        .act(GameAction::SelectTargets {
            targets: targets.iter().copied().map(TargetRef::Object).collect(),
        })
        .expect("the targets are legal")
        .waiting_for
}

fn tap_land_for_mana(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the engine offers the land's mana ability while paying");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap the land");
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

/// CR 602.2b + CR 601.2g: Manual Equip {1} asks how to pay after the target is
/// chosen; the player taps the Mountain and Sol Ring is never touched.
#[test]
fn a_manual_equip_waits_for_the_player_to_tap_the_land_and_leaves_sol_ring_untapped() {
    let EquipBoard {
        mut runner,
        bonesplitter,
        bears,
        sol_ring,
        mountain,
    } = equip_board();

    activate(&mut runner, bonesplitter, CastPaymentMode::Manual);
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(
        matches!(waiting_for, WaitingFor::ManaPayment { player, .. } if player == P0),
        "CR 601.2g: a manual activation stops for its mana payment, got {waiting_for:?}"
    );
    assert!(
        !tapped(&runner, sol_ring),
        "nothing is tapped for the player"
    );
    assert!(
        !tapped(&runner, mountain),
        "nothing is tapped for the player"
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing is on the stack yet"
    );

    tap_land_for_mana(&mut runner, mountain);
    let waiting_for = runner
        .act(GameAction::PassPriority)
        .expect("the paid activation completes")
        .waiting_for;
    assert!(
        matches!(waiting_for, WaitingFor::Priority { .. }),
        "{waiting_for:?}"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the equip ability is on the stack"
    );
    assert!(tapped(&runner, mountain), "the player's chosen land paid");
    assert!(!tapped(&runner, sol_ring), "Sol Ring was not used to pay");

    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&bears]
            .attachments
            .contains(&bonesplitter),
        "the equip ability resolves onto the chosen creature"
    );
}

/// The default stays automatic: an Auto Equip {1} is paid without a
/// `ManaPayment` window and reaches the stack straight from its target.
#[test]
fn an_auto_equip_still_pays_automatically() {
    let EquipBoard {
        mut runner,
        bonesplitter,
        bears,
        sol_ring,
        mountain,
    } = equip_board();

    activate(&mut runner, bonesplitter, CastPaymentMode::Auto);
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(
        matches!(waiting_for, WaitingFor::Priority { .. }),
        "an automatic activation needs no payment prompt, got {waiting_for:?}"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the equip ability is on the stack"
    );
    assert!(
        tapped(&runner, sol_ring) || tapped(&runner, mountain),
        "the engine tapped a source to pay {{1}}"
    );
}

/// CR 601.2b + CR 601.2c + CR 601.2g: Candelabra of Tawnos announces X, then
/// its X targets, then is paid. Paid manually, the two target lands tap for
/// the {2} and the ability untaps them; Sol Ring, which alone could pay {2},
/// stays untapped.
#[test]
fn a_manual_x_activation_is_paid_with_the_mana_abilities_the_player_chooses() {
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let candelabra = scenario.add_real_card(P0, "Candelabra of Tawnos", Zone::Battlefield, db);
    let island_a = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let island_b = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    let waiting_for = activate(&mut runner, candelabra, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::ChooseXValue { .. }),
        "CR 601.2b: X is announced first, got {waiting_for:?}"
    );
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("X = 2 is affordable");
    let waiting_for = choose_targets(&mut runner, &[island_a, island_b]);
    assert!(
        matches!(waiting_for, WaitingFor::ManaPayment { .. }),
        "CR 601.2g: the targets are chosen, then the cost is paid, got {waiting_for:?}"
    );
    assert!(
        !tapped(&runner, sol_ring),
        "nothing is tapped for the player"
    );

    tap_land_for_mana(&mut runner, island_a);
    tap_land_for_mana(&mut runner, island_b);
    runner
        .act(GameAction::PassPriority)
        .expect("the paid activation completes");
    assert_eq!(runner.state().stack.len(), 1, "the ability is on the stack");
    assert!(!tapped(&runner, sol_ring), "Sol Ring was not used to pay");

    runner.advance_until_stack_empty();
    assert!(
        !tapped(&runner, island_a) && !tapped(&runner, island_b),
        "the ability untaps the lands that paid for it"
    );
    assert!(tapped(&runner, candelabra), "{{T}} was paid");
}

/// CR 733.1 + CR 733.2: cancelling an activation during its manual payment
/// reverses the whole activation, exactly as cancelling a manual cast does:
/// nothing is on the stack, nothing is attached and the activation is not
/// counted. The mana ability activated while paying is the player's to keep
/// (CR 733.1 lets them reverse it, it does not make them), so its mana floats
/// and pays for the activation when the player redoes it.
#[test]
fn cancelling_a_manual_activation_during_payment_reverses_it() {
    let EquipBoard {
        mut runner,
        bonesplitter,
        bears,
        sol_ring,
        mountain,
    } = equip_board();

    activate(&mut runner, bonesplitter, CastPaymentMode::Manual);
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(
        matches!(waiting_for, WaitingFor::ManaPayment { .. }),
        "{waiting_for:?}"
    );
    tap_land_for_mana(&mut runner, mountain);

    let waiting_for = runner
        .act(GameAction::CancelCast)
        .expect("the activation can be cancelled while paying")
        .waiting_for;
    assert!(
        matches!(waiting_for, WaitingFor::Priority { player } if player == P0),
        "CR 733.2: the player who had priority retains it, got {waiting_for:?}"
    );
    assert!(runner.state().stack.is_empty(), "the activation is gone");
    assert!(runner.state().pending_cast.is_none());
    assert!(
        runner.state().objects[&bears].attachments.is_empty(),
        "nothing was equipped"
    );
    assert!(
        !runner
            .state()
            .activated_abilities_this_turn
            .keys()
            .any(|key| key.0 == bonesplitter),
        "a reversed activation is not counted"
    );
    assert!(!tapped(&runner, sol_ring), "Sol Ring was never touched");
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "the Mountain's mana floats, as after a cancelled manual cast"
    );

    // CR 733.2: the player may redo the reversed action legally; the floating
    // mana pays for it.
    activate(&mut runner, bonesplitter, CastPaymentMode::Manual);
    choose_targets(&mut runner, &[bears]);
    runner
        .act(GameAction::PassPriority)
        .expect("the floating mana pays the redone activation");
    assert_eq!(runner.state().stack.len(), 1);
    assert!(!tapped(&runner, sol_ring), "Sol Ring was never touched");
}

/// Bonesplitter's equip aimed at Heartfire Hero (Valiant: the first time each
/// turn it becomes the target of a spell or ability its controller controls,
/// it gets a +1/+1 counter), paid manually.
fn valiant_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bonesplitter = scenario.add_real_card(P0, "Bonesplitter", Zone::Battlefield, db);
    let hero = scenario.add_real_card(P0, "Heartfire Hero", Zone::Battlefield, db);
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    activate(&mut runner, bonesplitter, CastPaymentMode::Manual);
    let waiting_for = choose_targets(&mut runner, &[hero]);
    assert!(
        matches!(waiting_for, WaitingFor::ManaPayment { .. }),
        "{waiting_for:?}"
    );
    assert!(
        runner.state().stack.is_empty(),
        "CR 603.3: the targeting trigger waits for the activation to finish"
    );
    tap_land_for_mana(&mut runner, mountain);
    (runner, bonesplitter, hero, mountain)
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&engine::types::counter::CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// CR 602.2b + CR 603.2 + CR 603.3: the trigger the activation's target caused
/// is collected across the manual payment window and put on the stack above the
/// ability once it is activated.
#[test]
fn a_targeting_trigger_survives_the_manual_payment_window() {
    let (mut runner, bonesplitter, hero, _) = valiant_board();
    runner
        .act(GameAction::PassPriority)
        .expect("the paid activation completes");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the equip ability and the valiant trigger above it"
    );
    runner.advance_until_stack_empty();
    assert_eq!(plus_one_counters(&runner, hero), 1, "valiant resolved once");
    assert!(runner.state().objects[&hero]
        .attachments
        .contains(&bonesplitter));
}

/// CR 733.1: no abilities trigger as a result of an undone action, so a
/// cancelled manual activation leaves its targeting trigger behind with it.
#[test]
fn a_cancelled_manual_activation_triggers_nothing() {
    let (mut runner, _, hero, _) = valiant_board();
    runner
        .act(GameAction::CancelCast)
        .expect("the activation can be cancelled while paying");
    assert!(
        runner.state().stack.is_empty(),
        "no trigger was put on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(plus_one_counters(&runner, hero), 0);
}

// ---------------------------------------------------------------------------
// Rows that need more than the equip board. Every card is a printed card.
// ---------------------------------------------------------------------------

fn scenario_at_main() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn build(scenario: GameScenario) -> GameRunner {
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
}

fn add(scenario: &mut GameScenario, owner: PlayerId, name: &str, zone: Zone) -> ObjectId {
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    scenario.add_real_card(owner, name, zone, db)
}

/// Activates ability `ability_index` of `source_id` in `payment_mode`.
fn activate_index(
    runner: &mut GameRunner,
    source_id: ObjectId,
    ability_index: usize,
    payment_mode: CastPaymentMode,
) -> WaitingFor {
    runner
        .act(GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode,
        })
        .expect("the activation begins")
        .waiting_for
}

fn act(runner: &mut GameRunner, action: GameAction) -> WaitingFor {
    match runner.act(action.clone()) {
        Ok(result) => result.waiting_for,
        Err(error) => panic!(
            "{action:?} rejected at {:?}: {error:?}",
            runner.state().waiting_for
        ),
    }
}

fn pass(runner: &mut GameRunner) -> WaitingFor {
    act(runner, GameAction::PassPriority)
}

fn cancel(runner: &mut GameRunner) -> WaitingFor {
    act(runner, GameAction::CancelCast)
}

fn select_cards(runner: &mut GameRunner, cards: &[ObjectId]) -> WaitingFor {
    act(
        runner,
        GameAction::SelectCards {
            cards: cards.to_vec(),
        },
    )
}

fn in_zone(runner: &GameRunner, id: ObjectId, zone: Zone) -> bool {
    runner.state().objects[&id].zone == zone
}

fn is_mana_payment(waiting_for: &WaitingFor) -> bool {
    matches!(waiting_for, WaitingFor::ManaPayment { player, .. } if *player == P0)
}

fn is_priority(waiting_for: &WaitingFor) -> bool {
    matches!(waiting_for, WaitingFor::Priority { player } if *player == P0)
}

fn set_loyalty(runner: &mut GameRunner, id: ObjectId, loyalty: u32) {
    let object = runner.state_mut().objects.get_mut(&id).expect("the walker");
    object.loyalty = Some(loyalty);
    object
        .counters
        .insert(engine::types::counter::CounterType::Loyalty, loyalty);
}

fn loyalty(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id].loyalty
}

/// CR 702.29a + CR 602.2b + CR 601.2g: Remote Isle's Cycling {2} discards the
/// card as a cost, and a cost that is only a self-discard used to be paid at
/// once, before any window. Manual stops in `ManaPayment` first; the player
/// taps the two Islands and Sol Ring, which alone could pay, stays untapped.
#[test]
fn a_manual_cycling_asks_how_to_pay_before_discarding() {
    let mut scenario = scenario_at_main();
    let isle = add(&mut scenario, P0, "Remote Isle", Zone::Hand);
    let island_a = add(&mut scenario, P0, "Island", Zone::Battlefield);
    let island_b = add(&mut scenario, P0, "Island", Zone::Battlefield);
    let sol_ring = add(&mut scenario, P0, "Sol Ring", Zone::Battlefield);
    let mut runner = build(scenario);

    let waiting_for = activate(&mut runner, isle, CastPaymentMode::Manual);
    assert!(
        is_mana_payment(&waiting_for),
        "the first prompt is the payment window, not a discard choice: {waiting_for:?}"
    );
    assert!(in_zone(&runner, isle, Zone::Hand), "nothing is paid yet");
    assert!(runner.state().stack.is_empty());
    assert!(!tapped(&runner, sol_ring) && !tapped(&runner, island_a));

    tap_land_for_mana(&mut runner, island_a);
    tap_land_for_mana(&mut runner, island_b);
    let waiting_for = pass(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(in_zone(&runner, isle, Zone::Graveyard), "cycled");
    assert_eq!(runner.state().stack.len(), 1, "the cycling ability");
    assert!(!tapped(&runner, sol_ring), "Sol Ring was not used to pay");
}

/// The Auto sibling of the cycling row: the same board pays itself and needs
/// no window, so the Manual row above is reached by the mode alone.
#[test]
fn an_auto_cycling_pays_itself() {
    let mut scenario = scenario_at_main();
    let isle = add(&mut scenario, P0, "Remote Isle", Zone::Hand);
    add(&mut scenario, P0, "Island", Zone::Battlefield);
    add(&mut scenario, P0, "Island", Zone::Battlefield);
    add(&mut scenario, P0, "Sol Ring", Zone::Battlefield);
    let mut runner = build(scenario);

    let waiting_for = activate(&mut runner, isle, CastPaymentMode::Auto);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(in_zone(&runner, isle, Zone::Graveyard));
    assert_eq!(runner.state().stack.len(), 1);
}

/// Bow of Nylea ("{1}{G}, {T}: Choose one"), a modal activation with two
/// creatures to aim at. The mode choice comes before the targets and the
/// payment; the payment mode rides across it on the prompt itself.
fn bow_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = scenario_at_main();
    let bow = add(&mut scenario, P0, "Bow of Nylea", Zone::Battlefield);
    let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let forest_a = add(&mut scenario, P0, "Forest", Zone::Battlefield);
    let forest_b = add(&mut scenario, P0, "Forest", Zone::Battlefield);
    (build(scenario), bow, bears, forest_a, forest_b)
}

/// CR 602.2b + CR 601.2b + CR 601.2g: modes, then targets, then payment. The
/// payment mode survives the mode choice and the target selection.
#[test]
fn a_manual_modal_activation_keeps_its_payment_mode_across_the_mode_choice() {
    let (mut runner, bow, bears, forest_a, forest_b) = bow_board();

    let waiting_for = activate(&mut runner, bow, CastPaymentMode::Manual);
    assert!(
        matches!(
            waiting_for,
            WaitingFor::AbilityModeChoice {
                activation_payment_mode: CastPaymentMode::Manual,
                ..
            }
        ),
        "the mode prompt carries the player's payment mode: {waiting_for:?}"
    );
    let waiting_for = act(&mut runner, GameAction::SelectModes { indices: vec![0] });
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "mode 0 ('Put a +1/+1 counter on target creature') asks for its target: {waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert!(!tapped(&runner, forest_a) && !tapped(&runner, forest_b));
    assert!(runner.state().stack.is_empty());

    tap_land_for_mana(&mut runner, forest_a);
    tap_land_for_mana(&mut runner, forest_b);
    pass(&mut runner);
    assert_eq!(runner.state().stack.len(), 1, "the modal ability");
    assert!(tapped(&runner, bow), "{{T}} was paid");
}

/// The Auto sibling: the same modal activation carries Auto across the mode
/// choice and pays itself once the target is chosen.
#[test]
fn an_auto_modal_activation_still_pays_itself() {
    let (mut runner, bow, bears, forest_a, forest_b) = bow_board();

    let waiting_for = activate(&mut runner, bow, CastPaymentMode::Auto);
    assert!(
        matches!(
            waiting_for,
            WaitingFor::AbilityModeChoice {
                activation_payment_mode: CastPaymentMode::Auto,
                ..
            }
        ),
        "{waiting_for:?}"
    );
    act(&mut runner, GameAction::SelectModes { indices: vec![0] });
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(runner.state().stack.len(), 1);
    assert!(tapped(&runner, forest_a) && tapped(&runner, forest_b));
}

/// The wire shape: Auto is the default and is not written, a payload without
/// the key reads as Auto, and Manual survives a round trip.
#[test]
fn the_payment_mode_of_an_activation_is_optional_on_the_wire() {
    let action = |payment_mode| GameAction::ActivateAbility {
        source_id: ObjectId(7),
        ability_index: 2,
        payment_mode,
    };

    let auto = serde_json::to_string(&action(CastPaymentMode::Auto)).unwrap();
    assert!(
        !auto.contains("payment_mode"),
        "Auto is the default and is omitted: {auto}"
    );
    assert_eq!(
        serde_json::from_str::<GameAction>(&auto).unwrap(),
        action(CastPaymentMode::Auto),
        "a payload without the key reads as Auto"
    );

    let manual = serde_json::to_string(&action(CastPaymentMode::Manual)).unwrap();
    assert!(
        manual.contains("\"payment_mode\":{\"type\":\"Manual\"}"),
        "{manual}"
    );
    assert_eq!(
        serde_json::from_str::<GameAction>(&manual).unwrap(),
        action(CastPaymentMode::Manual)
    );
}

/// An activation with no mana in its cost has no payment window to open:
/// Viscera Seer's "Sacrifice a creature" asks for the sacrifice and goes to the
/// stack, in Manual exactly as in Auto.
#[test]
fn a_manual_activation_without_mana_opens_no_window() {
    for mode in [CastPaymentMode::Manual, CastPaymentMode::Auto] {
        let mut scenario = scenario_at_main();
        let seer = add(&mut scenario, P0, "Viscera Seer", Zone::Battlefield);
        let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
        let mut runner = build(scenario);

        let waiting_for = activate(&mut runner, seer, mode);
        assert!(
            matches!(
                waiting_for,
                WaitingFor::PayCost {
                    kind: PayCostKind::Sacrifice,
                    ..
                }
            ),
            "{mode:?}: the sacrifice is the only prompt: {waiting_for:?}"
        );
        let waiting_for = select_cards(&mut runner, &[bears]);
        assert!(is_priority(&waiting_for), "{mode:?}: {waiting_for:?}");
        assert!(in_zone(&runner, bears, Zone::Graveyard), "{mode:?}");
        assert_eq!(runner.state().stack.len(), 1, "{mode:?}: on the stack");
    }
}

/// Jace Beleren's +2 costs only loyalty (CR 606.4): Manual opens no window.
#[test]
fn a_manual_loyalty_activation_without_a_tax_opens_no_window() {
    let mut scenario = scenario_at_main();
    let jace = add(&mut scenario, P0, "Jace Beleren", Zone::Battlefield);
    let mut runner = build(scenario);
    set_loyalty(&mut runner, jace, 5);

    let waiting_for = activate_index(&mut runner, jace, 0, CastPaymentMode::Manual);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(loyalty(&runner, jace), Some(7), "the loyalty cost was paid");
    assert_eq!(runner.state().stack.len(), 1, "the ability is on the stack");
}

/// Jace Beleren under Eidolon of Obstruction ("Loyalty abilities of
/// planeswalkers your opponents control cost {1} more to activate"), with one
/// Island: the {1} tax is a mana payment (CR 601.2f), so Manual stops for it,
/// before the loyalty counters change (CR 601.2h).
fn taxed_jace_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = scenario_at_main();
    let jace = add(&mut scenario, P0, "Jace Beleren", Zone::Battlefield);
    let island = add(&mut scenario, P0, "Island", Zone::Battlefield);
    add(
        &mut scenario,
        P1,
        "Eidolon of Obstruction",
        Zone::Battlefield,
    );
    let mut runner = build(scenario);
    set_loyalty(&mut runner, jace, 5);
    (runner, jace, island)
}

/// CR 601.2f + CR 601.2g + CR 733.1: the untargeted +2 asks for the tax first;
/// cancelling there leaves the loyalty where it was.
#[test]
fn a_manual_taxed_untargeted_loyalty_activation_waits_for_the_tax() {
    let (mut runner, jace, island) = taxed_jace_board();

    let waiting_for = activate_index(&mut runner, jace, 0, CastPaymentMode::Manual);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert_eq!(
        loyalty(&runner, jace),
        Some(5),
        "loyalty is paid after the tax"
    );
    assert!(runner.state().stack.is_empty());
    assert!(!tapped(&runner, island));

    let waiting_for = cancel(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(loyalty(&runner, jace), Some(5));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());

    // The redo is legal and, paid by hand, completes.
    let waiting_for = activate_index(&mut runner, jace, 0, CastPaymentMode::Manual);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    tap_land_for_mana(&mut runner, island);
    pass(&mut runner);
    assert_eq!(loyalty(&runner, jace), Some(7));
    assert_eq!(runner.state().stack.len(), 1);
}

#[test]
fn an_auto_taxed_untargeted_loyalty_activation_pays_the_tax_itself() {
    let (mut runner, jace, island) = taxed_jace_board();

    let waiting_for = activate_index(&mut runner, jace, 0, CastPaymentMode::Auto);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(loyalty(&runner, jace), Some(7));
    assert_eq!(runner.state().stack.len(), 1);
    assert!(tapped(&runner, island), "the Island paid the tax");
}

/// The targeted -1 ("Target player draws a card") is not hoisted ahead of its
/// target: target, then the tax window, with the loyalty unchanged until paid.
#[test]
fn a_manual_taxed_targeted_loyalty_activation_waits_for_the_tax_after_its_target() {
    let (mut runner, jace, island) = taxed_jace_board();

    let waiting_for = activate_index(&mut runner, jace, 1, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = act(
        &mut runner,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        },
    );
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert_eq!(
        loyalty(&runner, jace),
        Some(5),
        "loyalty is paid after the tax"
    );
    assert!(runner.state().stack.is_empty());

    let waiting_for = cancel(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(loyalty(&runner, jace), Some(5));
    assert!(runner.state().stack.is_empty());

    // Guard: redone and paid by hand, the loyalty drops by one.
    activate_index(&mut runner, jace, 1, CastPaymentMode::Manual);
    act(
        &mut runner,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        },
    );
    tap_land_for_mana(&mut runner, island);
    pass(&mut runner);
    assert_eq!(loyalty(&runner, jace), Some(4));
    assert_eq!(runner.state().stack.len(), 1);
}

#[test]
fn an_auto_taxed_targeted_loyalty_activation_pays_the_tax_itself() {
    let (mut runner, jace, island) = taxed_jace_board();

    activate_index(&mut runner, jace, 1, CastPaymentMode::Auto);
    let waiting_for = act(
        &mut runner,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        },
    );
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(loyalty(&runner, jace), Some(4));
    assert_eq!(runner.state().stack.len(), 1);
    assert!(tapped(&runner, island));
}

/// Embercleave's Equip {3} with two cost reducers that disagree on a floor:
/// Zirda, the Dawnwaker ("Abilities you activate that aren't mana abilities
/// cost {2} less to activate. This effect can't reduce the mana in that cost to
/// less than one mana.") and Bladehold War-Whip ("Equip abilities you activate
/// of other Equipment cost {1} less to activate"). Zirda then War-Whip locks
/// {0}; War-Whip then Zirda locks {1} (CR 601.2f: the order is the player's).
fn election_board() -> (GameRunner, ObjectId, ObjectId, [ObjectId; 2]) {
    let mut scenario = scenario_at_main();
    add(&mut scenario, P0, "Zirda, the Dawnwaker", Zone::Battlefield);
    add(&mut scenario, P0, "Bladehold War-Whip", Zone::Battlefield);
    let embercleave = add(&mut scenario, P0, "Embercleave", Zone::Battlefield);
    let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let mountain_a = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
    let mountain_b = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
    let runner = build(scenario);
    // `add_real_card` queues no enters-the-battlefield trigger: the board is
    // exactly what was placed.
    assert!(runner.state().stack.is_empty());
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|object| !object.is_token),
        "no Rebel token exists"
    );
    (runner, embercleave, bears, [mountain_a, mountain_b])
}

/// Zirda is listed first on the board, so index 0 of the election is Zirda and
/// index 1 War-Whip: `[1, 0]` applies War-Whip first and locks {1}, `[0, 1]`
/// locks {0}.
const LOCK_ONE: [usize; 2] = [1, 0];
const LOCK_ZERO: [usize; 2] = [0, 1];

fn elect(runner: &mut GameRunner, order: [usize; 2]) -> WaitingFor {
    act(
        runner,
        GameAction::OrderCostReductions {
            order: order.to_vec(),
            hybrid_announcement: Vec::new(),
        },
    )
}

/// CR 601.2f + CR 602.2b: the election keeps the activation's payment mode:
/// after electing the {1} order and aiming, Manual stops for the payment.
#[test]
fn a_manual_activation_keeps_its_payment_mode_through_the_cost_election() {
    let (mut runner, embercleave, bears, [mountain_a, mountain_b]) = election_board();

    let waiting_for = activate(&mut runner, embercleave, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::OrderCostReductions { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = elect(&mut runner, LOCK_ONE);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "a targeted activation elects before it aims: {waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert!(!tapped(&runner, mountain_a) && !tapped(&runner, mountain_b));
    assert!(runner.state().stack.is_empty());

    tap_land_for_mana(&mut runner, mountain_a);
    let waiting_for = pass(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(runner.state().stack.len(), 1, "the equip ability");
    assert!(!tapped(&runner, mountain_b), "the locked cost was {{1}}");
}

#[test]
fn an_auto_activation_pays_the_elected_cost_itself() {
    let (mut runner, embercleave, bears, [mountain_a, mountain_b]) = election_board();

    activate(&mut runner, embercleave, CastPaymentMode::Auto);
    elect(&mut runner, LOCK_ONE);
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        usize::from(tapped(&runner, mountain_a)) + usize::from(tapped(&runner, mountain_b)),
        1,
        "exactly one Mountain paid the {{1}}"
    );
}

/// Guard: the other order locks {0}, which has no mana leg, so neither mode
/// has a window to open.
#[test]
fn an_election_that_locks_no_mana_opens_no_window_in_either_mode() {
    for mode in [CastPaymentMode::Manual, CastPaymentMode::Auto] {
        let (mut runner, embercleave, bears, [mountain_a, mountain_b]) = election_board();
        activate(&mut runner, embercleave, mode);
        elect(&mut runner, LOCK_ZERO);
        let waiting_for = choose_targets(&mut runner, &[bears]);
        assert!(is_priority(&waiting_for), "{mode:?}: {waiting_for:?}");
        assert_eq!(runner.state().stack.len(), 1, "{mode:?}");
        assert!(!tapped(&runner, mountain_a) && !tapped(&runner, mountain_b));
    }
}

/// The presentation surfaces of every choice P0's interaction view offers for
/// exactly `action` (matched by its opaque action id): empty when it is not
/// offered.
fn offered_surfaces(
    runner: &GameRunner,
    action: &GameAction,
) -> Vec<Vec<engine::types::interaction::InteractionPresentationSurface>> {
    use engine::game::interaction::{
        bind_interaction_authority, derive_viewer_interaction, interaction_action_id,
    };
    use engine::types::interaction::{
        InteractionOpportunityResponse, InteractionPresentationSurface, InteractionSessionId,
    };

    let mut bound = runner.state().clone();
    bind_interaction_authority(&mut bound, InteractionSessionId("viewer".to_string()))
        .expect("the interaction authority binds");
    let filtered = engine::game::visibility::filter_state_for_viewer(&bound, P0);
    let view = derive_viewer_interaction(&bound, &filtered, P0);
    let wanted = interaction_action_id(action);
    view.opportunities
        .iter()
        .flat_map(|opportunity| match &opportunity.response {
            InteractionOpportunityResponse::ExactChoices { choices } => choices.iter(),
            InteractionOpportunityResponse::Schema { candidates, .. } => candidates.iter(),
        })
        .filter(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Action { action_id: Some(id), .. }
                        if *id == wanted
                )
            })
        })
        .map(|choice| choice.surfaces.clone())
        .collect()
}

fn activation(
    source_id: ObjectId,
    ability_index: usize,
    payment_mode: CastPaymentMode,
) -> GameAction {
    GameAction::ActivateAbility {
        source_id,
        ability_index,
        payment_mode,
    }
}

/// An interaction candidate for a manual activation is offered for an ability
/// that has a payment to make, and not for a mana ability, which is activated
/// during a payment rather than paid for (CR 605.3a). The pair is told apart
/// by the payment mode in its presentation, as a manual cast is.
#[test]
fn a_manual_sibling_is_offered_for_an_equip_and_not_for_a_mana_ability() {
    let EquipBoard {
        runner,
        bonesplitter,
        sol_ring,
        ..
    } = equip_board();
    let equip_index = activated_ability_index(&runner, bonesplitter);
    let ring_index = runner.state().objects[&sol_ring]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("Sol Ring's mana ability");

    let auto = offered_surfaces(
        &runner,
        &activation(bonesplitter, equip_index, CastPaymentMode::Auto),
    );
    let manual = offered_surfaces(
        &runner,
        &activation(bonesplitter, equip_index, CastPaymentMode::Manual),
    );
    assert_eq!(auto.len(), 1, "the equip has an automatic candidate");
    assert_eq!(manual.len(), 1, "the equip has a manual sibling");
    assert_ne!(
        auto[0], manual[0],
        "the manual sibling is distinguishable from the automatic candidate"
    );
    assert!(!offered_surfaces(
        &runner,
        &activation(sol_ring, ring_index, CastPaymentMode::Auto)
    )
    .is_empty());
    assert!(
        offered_surfaces(
            &runner,
            &activation(sol_ring, ring_index, CastPaymentMode::Manual)
        )
        .is_empty(),
        "a mana ability has no manual sibling"
    );
}

/// CR 605.3a + CR 113.6b: Elvish Spirit Guide ("Exile this card from your hand:
/// Add {G}.") is a mana ability that works from the hand, so it sits in the
/// Priority candidate list itself rather than being appended after it. It is
/// offered once, automatically, and gets no manual twin; Bonesplitter's equip
/// beside it still does, so the gate is on the mana ability and not on the board.
#[test]
fn a_mana_ability_activated_from_the_hand_has_no_manual_sibling_at_priority() {
    let mut scenario = scenario_at_main();
    let guide = add(&mut scenario, P0, "Elvish Spirit Guide", Zone::Hand);
    let bonesplitter = add(&mut scenario, P0, "Bonesplitter", Zone::Battlefield);
    add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    add(&mut scenario, P0, "Mountain", Zone::Battlefield);
    let runner = build(scenario);
    let guide_index = runner.state().objects[&guide]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("Elvish Spirit Guide's mana ability");
    let equip_index = activated_ability_index(&runner, bonesplitter);

    assert_eq!(
        offered_surfaces(
            &runner,
            &activation(guide, guide_index, CastPaymentMode::Auto)
        )
        .len(),
        1,
        "reach guard: the hand-zone mana ability is offered once, automatically"
    );
    assert!(
        offered_surfaces(
            &runner,
            &activation(guide, guide_index, CastPaymentMode::Manual)
        )
        .is_empty(),
        "a hand-zone mana ability has no manual sibling"
    );
    assert_eq!(
        offered_surfaces(
            &runner,
            &activation(bonesplitter, equip_index, CastPaymentMode::Manual)
        )
        .len(),
        1,
        "the equip beside it keeps its manual sibling"
    );
}

/// The same holds inside a spell's manual `ManaPayment` window, where the
/// mana abilities that can pay it are the candidates: Krark-Clan Ironworks
/// ("Sacrifice an artifact: Add {C}{C}.") is offered once, automatically, and
/// has no manual twin (CR 605.3a).
#[test]
fn a_mana_ability_in_a_manual_payment_window_has_no_manual_sibling() {
    let mut scenario = scenario_at_main();
    let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Hand);
    let ironworks = add(&mut scenario, P0, "Krark-Clan Ironworks", Zone::Battlefield);
    let altar = add(&mut scenario, P0, "Ashnod's Altar", Zone::Battlefield);
    add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let mut runner = build(scenario);
    let card_id = runner.state().objects[&bears].card_id;
    let waiting_for = act(
        &mut runner,
        GameAction::CastSpell {
            object_id: bears,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
    );
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");

    let ring_index = runner.state().objects[&ironworks]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("Krark-Clan Ironworks' mana ability");
    assert_eq!(
        offered_surfaces(
            &runner,
            &activation(ironworks, ring_index, CastPaymentMode::Auto)
        )
        .len(),
        1,
        "reach guard: the window offers the ability once, automatically"
    );
    assert!(
        offered_surfaces(
            &runner,
            &activation(ironworks, ring_index, CastPaymentMode::Manual)
        )
        .is_empty(),
        "a mana ability has no manual sibling in the payment window either"
    );
    // The window is a DirectChoices window (interaction.rs `human_response_model`),
    // whose list is the Auto mana actions and which clones no Manual sibling, so the
    // viewer half above holds by the window model. This half only checks the AI
    // list (`legal_actions_full`), which never builds Manual siblings at all; the
    // sibling rule itself is pinned by the Priority rows.
    let (legal, _, _) = legal_actions_full(runner.state());
    for source in [ironworks, altar] {
        let index = runner.state().objects[&source]
            .abilities
            .iter()
            .position(is_mana_ability)
            .expect("a mana ability");
        let offered = |mode| legal.contains(&activation(source, index, mode));
        assert!(
            offered(CastPaymentMode::Auto),
            "reach guard: {source:?} is offered"
        );
        assert!(
            !offered(CastPaymentMode::Manual),
            "{source:?} has no manual twin"
        );
    }
}

/// CR 702.29a + CR 603.2 + CR 603.3: Lonely Sandbar's Cycling {U}, with Drake
/// Haven ("Whenever you cycle or discard a card") and Magmakin Artillerist
/// ("Whenever you discard one or more cards, this creature deals that much
/// damage to each opponent") out. Paid by hand, the cycling puts its two
/// triggers above it, one of each, exactly as an automatic payment does.
#[test]
fn a_manual_cycling_puts_the_triggers_of_its_discard_on_the_stack_once() {
    let board = |mode| {
        let mut scenario = scenario_at_main();
        let artillerist = add(&mut scenario, P0, "Magmakin Artillerist", Zone::Battlefield);
        let haven = add(&mut scenario, P0, "Drake Haven", Zone::Battlefield);
        let sandbar = add(&mut scenario, P0, "Lonely Sandbar", Zone::Hand);
        let island = add(&mut scenario, P0, "Island", Zone::Battlefield);
        let mut runner = build(scenario);
        let waiting_for = activate(&mut runner, sandbar, mode);
        (runner, artillerist, haven, sandbar, island, waiting_for)
    };
    let triggers_from = |runner: &GameRunner, source: ObjectId| {
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| {
                entry.source_id == source
                    && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
            })
            .count()
    };
    let settle_order = |runner: &mut GameRunner| {
        if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
            act(
                runner,
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
            );
        }
    };

    // Auto reference.
    let (mut runner, artillerist, haven, sandbar, _, waiting_for) = board(CastPaymentMode::Auto);
    assert!(
        !is_mana_payment(&waiting_for),
        "reference: no window: {waiting_for:?}"
    );
    settle_order(&mut runner);
    assert!(in_zone(&runner, sandbar, Zone::Graveyard));
    assert_eq!(runner.state().stack.len(), 3, "cycling + two triggers");
    assert_eq!(triggers_from(&runner, artillerist), 1);
    assert_eq!(triggers_from(&runner, haven), 1);

    // Manual.
    let (mut runner, artillerist, haven, sandbar, island, waiting_for) =
        board(CastPaymentMode::Manual);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert!(runner.state().stack.is_empty());
    tap_land_for_mana(&mut runner, island);
    pass(&mut runner);
    settle_order(&mut runner);
    assert!(in_zone(&runner, sandbar, Zone::Graveyard));
    assert_eq!(runner.state().stack.len(), 3, "cycling + two triggers");
    assert_eq!(triggers_from(&runner, artillerist), 1, "once");
    assert_eq!(triggers_from(&runner, haven), 1, "once");

    let opponent_life = runner.state().players[1].life;
    // Drake Haven's "you may pay {1}" is declined; the Artillerist's trigger
    // is the one under test.
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            act(
                &mut runner,
                GameAction::DecideOptionalEffect { accept: false },
            );
        } else if runner.state().stack.is_empty() {
            break;
        } else {
            pass(&mut runner);
        }
    }
    assert!(runner.state().stack.is_empty(), "everything resolved");
    assert_eq!(
        runner.state().players[1].life,
        opponent_life - 1,
        "Artillerist's trigger resolved once"
    );
}

// ---------------------------------------------------------------------------
// The window precedes every non-mana cost (CR 601.2g + CR 601.2h)
// ---------------------------------------------------------------------------
//
// CR 601.2g: "If the total cost includes a mana payment, the player then has a
// chance to activate mana abilities ... Mana abilities must be activated before
// costs are paid." So a Manual activation stops in `ManaPayment` before any of
// its non-mana costs is paid — a counter removal, a random discard, an exile, a
// sacrifice of its source — and pays them after the window (CR 601.2h). Each row
// has its Auto reference on the same board.

fn ability_index_of(runner: &GameRunner, source: ObjectId, text_fragment: &str) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|description| description.contains(text_fragment))
        })
        .unwrap_or_else(|| panic!("{source:?} has an ability reading {text_fragment:?}"))
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn pool_total(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].mana_pool.total()
}

fn is_cost_prompt(waiting_for: &WaitingFor, matches_kind: impl Fn(&PayCostKind) -> bool) -> bool {
    matches!(waiting_for, WaitingFor::PayCost { kind, .. } if matches_kind(kind))
}

/// Ghave, Guru of Spores ("Ghave enters with five +1/+1 counters on it. {1},
/// Remove a +1/+1 counter from a creature you control: Create a 1/1 green
/// Saproling creature token."), with a Swamp and an unused Island.
fn ghave_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, usize) {
    let mut scenario = scenario_at_main();
    let ghave = add(
        &mut scenario,
        P0,
        "Ghave, Guru of Spores",
        Zone::Battlefield,
    );
    let swamp = add(&mut scenario, P0, "Swamp", Zone::Battlefield);
    let island = add(&mut scenario, P0, "Island", Zone::Battlefield);
    let runner = build(scenario);
    assert_eq!(
        plus_counters(&runner, ghave),
        5,
        "setup: Ghave enters with five"
    );
    let index = ability_index_of(&runner, ghave, "Remove a +1/+1 counter");
    (runner, ghave, swamp, island, index)
}

/// Auto reference: the counter-removal choice is the first prompt.
#[test]
fn an_auto_counter_removal_activation_chooses_its_counter_first() {
    let (mut runner, ghave, _, _, index) = ghave_board();
    let waiting_for = activate_index(&mut runner, ghave, index, CastPaymentMode::Auto);
    assert!(
        is_cost_prompt(&waiting_for, |kind| matches!(
            kind,
            PayCostKind::RemoveCounter { .. }
        )),
        "{waiting_for:?}"
    );
}

/// CR 601.2g + CR 601.2h: Manual stops in the window with all five counters on
/// Ghave; the counter is chosen and removed after the Swamp paid the {1}.
#[test]
fn a_manual_counter_removal_activation_opens_the_window_before_removing_the_counter() {
    let (mut runner, ghave, swamp, island, index) = ghave_board();
    let waiting_for = activate_index(&mut runner, ghave, index, CastPaymentMode::Manual);
    assert!(
        is_mana_payment(&waiting_for),
        "the window is the first prompt: {waiting_for:?}"
    );
    assert_eq!(
        plus_counters(&runner, ghave),
        5,
        "no counter is removed yet"
    );
    assert!(runner.state().stack.is_empty());

    tap_land_for_mana(&mut runner, swamp);
    let waiting_for = pass(&mut runner);
    assert!(
        is_cost_prompt(&waiting_for, |kind| matches!(
            kind,
            PayCostKind::RemoveCounter { .. }
        )),
        "the counter removal follows the window: {waiting_for:?}"
    );
    let waiting_for = select_cards(&mut runner, &[ghave]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(
        plus_counters(&runner, ghave),
        4,
        "one counter was removed once"
    );
    assert_eq!(runner.state().stack.len(), 1, "the token ability");
    assert!(tapped(&runner, swamp) && !tapped(&runner, island));
}

/// Pyromancy ("{3}, Discard a card at random: This enchantment deals damage to
/// any target equal to the mana value of the discarded card."), three lands and
/// two cards in hand. CR 601.2c + CR 601.2g + CR 601.2h: target, then the window
/// with the hand intact, then the random discard — after every nonrandom cost
/// and with no second window.
#[test]
fn a_manual_random_discard_activation_discards_after_its_window() {
    let mut scenario = scenario_at_main();
    let pyromancy = add(&mut scenario, P0, "Pyromancy", Zone::Battlefield);
    let mountains = [
        add(&mut scenario, P0, "Mountain", Zone::Battlefield),
        add(&mut scenario, P0, "Mountain", Zone::Battlefield),
        add(&mut scenario, P0, "Mountain", Zone::Battlefield),
    ];
    add(&mut scenario, P0, "Grizzly Bears", Zone::Hand);
    add(&mut scenario, P0, "Hill Giant", Zone::Hand);
    let mut runner = build(scenario);
    let hand_before = hand_size(&runner, P0);
    let opponent_life = life(&runner, P1);

    let waiting_for = activate(&mut runner, pyromancy, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = act(
        &mut runner,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        },
    );
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert_eq!(
        hand_size(&runner, P0),
        hand_before,
        "nothing is discarded yet"
    );

    for mountain in mountains {
        tap_land_for_mana(&mut runner, mountain);
    }
    let waiting_for = pass(&mut runner);
    assert!(
        is_priority(&waiting_for),
        "the random discard is paid at once, with no second window: {waiting_for:?}"
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_before - 1,
        "one card discarded"
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner.advance_until_stack_empty();
    let dealt = opponent_life - life(&runner, P1);
    assert!(
        dealt == 2 || dealt == 4,
        "the damage is the discarded card's mana value: {dealt}"
    );
}

/// Coin of Fate's "{3}{W}, {T}, Exile two creature cards from your graveyard,
/// Sacrifice this artifact: ...". CR 601.2g + CR 601.2h: the window comes with
/// the graveyard and the Coin intact; the exile choice and the Coin's own
/// sacrifice follow it.
#[test]
fn a_manual_multi_component_activation_exiles_and_sacrifices_after_its_window() {
    let mut scenario = scenario_at_main();
    let coin = add(&mut scenario, P0, "Coin of Fate", Zone::Battlefield);
    let first = add(&mut scenario, P0, "Grizzly Bears", Zone::Graveyard);
    let second = add(&mut scenario, P0, "Hill Giant", Zone::Graveyard);
    let lands = [
        add(&mut scenario, P0, "Plains", Zone::Battlefield),
        add(&mut scenario, P0, "Plains", Zone::Battlefield),
        add(&mut scenario, P0, "Plains", Zone::Battlefield),
        add(&mut scenario, P0, "Plains", Zone::Battlefield),
    ];
    let mut runner = build(scenario);

    let waiting_for = activate(&mut runner, coin, CastPaymentMode::Manual);
    assert!(
        is_mana_payment(&waiting_for),
        "the window is the first prompt: {waiting_for:?}"
    );
    assert!(in_zone(&runner, first, Zone::Graveyard) && in_zone(&runner, second, Zone::Graveyard));
    assert!(in_zone(&runner, coin, Zone::Battlefield) && !tapped(&runner, coin));

    for land in lands {
        tap_land_for_mana(&mut runner, land);
    }
    let waiting_for = pass(&mut runner);
    assert!(
        is_cost_prompt(&waiting_for, |kind| matches!(
            kind,
            PayCostKind::ExileFromZone { .. }
        )),
        "the exile follows the window: {waiting_for:?}"
    );
    let waiting_for = select_cards(&mut runner, &[first, second]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(in_zone(&runner, first, Zone::Exile) && in_zone(&runner, second, Zone::Exile));
    assert!(
        in_zone(&runner, coin, Zone::Graveyard),
        "the Coin was sacrificed once"
    );
    assert_eq!(runner.state().stack.len(), 1);
}

/// Crystal Shard ("{3}, {T} or {U}, {T}: Return target creature to its owner's
/// hand unless its controller pays {1}."). CR 601.2b + CR 601.2g: the target,
/// then the cost alternative is announced — nothing is paid — and the window
/// prices the alternative selected. A second creature makes the target a real
/// choice: the engine selects a lone legal target without asking.
fn shard_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, [ObjectId; 3]) {
    let mut scenario = scenario_at_main();
    let shard = add(&mut scenario, P0, "Crystal Shard", Zone::Battlefield);
    let victim = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let island = add(&mut scenario, P0, "Island", Zone::Battlefield);
    let mountains = [
        add(&mut scenario, P0, "Mountain", Zone::Battlefield),
        add(&mut scenario, P0, "Mountain", Zone::Battlefield),
        add(&mut scenario, P0, "Mountain", Zone::Battlefield),
    ];
    (build(scenario), shard, victim, island, mountains)
}

/// The OneOf alternative whose mana component holds `shard`.
fn alternative_paying(waiting_for: &WaitingFor, wanted: ManaCostShard) -> usize {
    fn holds(cost: &AbilityCost, wanted: ManaCostShard) -> bool {
        match cost {
            AbilityCost::Mana {
                cost: ManaCost::Cost { shards, .. },
            } => shards.contains(&wanted),
            AbilityCost::Composite { costs } => costs.iter().any(|cost| holds(cost, wanted)),
            _ => false,
        }
    }
    let WaitingFor::ActivationCostOneOfChoice { costs, .. } = waiting_for else {
        panic!("expected the cost alternative, got {waiting_for:?}");
    };
    costs
        .iter()
        .position(|cost| holds(cost, wanted))
        .expect("the alternative is offered")
}

#[test]
fn a_manual_alternative_cost_is_announced_before_the_window_that_prices_it() {
    let (mut runner, shard, victim, island, mountains) = shard_board();
    let waiting_for = activate(&mut runner, shard, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut runner, &[victim]);
    let index = alternative_paying(&waiting_for, ManaCostShard::Blue);
    assert!(
        !tapped(&runner, shard),
        "announcing the alternative pays nothing"
    );

    let waiting_for = act(
        &mut runner,
        GameAction::ChooseActivationCostBranch { index },
    );
    assert!(
        is_mana_payment(&waiting_for),
        "the window follows the selected alternative: {waiting_for:?}"
    );
    assert!(!tapped(&runner, shard), "{{T}} is paid after the window");
    tap_land_for_mana(&mut runner, island);
    let waiting_for = pass(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(tapped(&runner, shard));
    assert!(
        mountains.iter().all(|mountain| !tapped(&runner, *mountain)),
        "the {{U}} alternative owed one mana: the Mountains are untouched"
    );
    assert_eq!(runner.state().stack.len(), 1);
}

#[test]
fn a_manual_generic_alternative_owes_its_own_total() {
    let (mut runner, shard, victim, island, mountains) = shard_board();
    activate(&mut runner, shard, CastPaymentMode::Manual);
    let waiting_for = choose_targets(&mut runner, &[victim]);
    let index = alternative_paying(&waiting_for, ManaCostShard::Blue);
    // The other alternative: {3}, {T}.
    let index = 1 - index;
    let waiting_for = act(
        &mut runner,
        GameAction::ChooseActivationCostBranch { index },
    );
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    for mountain in mountains {
        tap_land_for_mana(&mut runner, mountain);
    }
    let waiting_for = pass(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(tapped(&runner, shard));
    assert!(
        !tapped(&runner, island),
        "the {{3}} alternative did not use the Island"
    );
}

// ---------------------------------------------------------------------------
// Formidable Speaker: target, then the window, then the effect (issue #6016)
// ---------------------------------------------------------------------------

/// Formidable Speaker ("When this creature enters, you may discard a card. If
/// you do, search your library for a creature card, reveal it, put it into your
/// hand, then shuffle. / {1}, {T}: Untap another target permanent."), already
/// on the battlefield (its enters trigger is not replayed), with two tapped
/// permanents it could untap, a Mountain to pay with and an unused Sol Ring.
struct SpeakerBoard {
    runner: GameRunner,
    speaker: ObjectId,
    bears: ObjectId,
    forest: ObjectId,
    mountain: ObjectId,
    sol_ring: ObjectId,
}

fn speaker_board(extra_hand: &[&str]) -> SpeakerBoard {
    let mut scenario = scenario_at_main();
    let speaker = add(&mut scenario, P0, "Formidable Speaker", Zone::Battlefield);
    let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let forest = add(&mut scenario, P0, "Forest", Zone::Battlefield);
    let mountain = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
    let sol_ring = add(&mut scenario, P0, "Sol Ring", Zone::Battlefield);
    for name in extra_hand {
        add(&mut scenario, P0, name, Zone::Hand);
    }
    let mut runner = build(scenario);
    assert!(
        runner.state().stack.is_empty(),
        "no enters trigger was queued"
    );
    // Fixture precondition: the two permanents the ability can untap are tapped.
    for id in [bears, forest] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("object")
            .tapped = true;
    }
    SpeakerBoard {
        runner,
        speaker,
        bears,
        forest,
        mountain,
        sol_ring,
    }
}

/// CR 602.2b + CR 601.2c + CR 601.2g: Manual chooses the target first, then
/// stops in the window with nothing tapped — not the Speaker, not a source —
/// and once the player taps the Mountain the ability is placed and untaps the
/// chosen permanent only.
#[test]
fn manual_formidable_speaker_targets_then_opens_the_window_and_untaps_the_target() {
    let SpeakerBoard {
        mut runner,
        speaker,
        bears,
        forest,
        mountain,
        sol_ring,
    } = speaker_board(&[]);

    let waiting_for = activate(&mut runner, speaker, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "CR 601.2c: the target is chosen first: {waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(
        is_mana_payment(&waiting_for),
        "CR 601.2g: the window follows the target: {waiting_for:?}"
    );
    assert!(!tapped(&runner, speaker), "{{T}} is paid after the window");
    assert!(!tapped(&runner, mountain) && !tapped(&runner, sol_ring));
    assert!(runner.state().stack.is_empty());

    tap_land_for_mana(&mut runner, mountain);
    let waiting_for = pass(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(runner.state().stack.len(), 1, "the untap ability");
    assert!(tapped(&runner, speaker), "{{T}} was paid once");
    assert!(!tapped(&runner, sol_ring), "Sol Ring was never used");

    runner.advance_until_stack_empty();
    assert!(!tapped(&runner, bears), "the chosen permanent untaps");
    assert!(tapped(&runner, forest), "the other permanent stays tapped");
}

/// The Auto control on the same board: the target is chosen, the {1} is paid
/// automatically and the ability is placed with no window.
#[test]
fn auto_formidable_speaker_pays_itself_after_its_target() {
    let SpeakerBoard {
        mut runner,
        speaker,
        bears,
        mountain,
        sol_ring,
        ..
    } = speaker_board(&[]);
    activate(&mut runner, speaker, CastPaymentMode::Auto);
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(runner.state().stack.len(), 1);
    assert!(tapped(&runner, speaker));
    assert!(tapped(&runner, mountain) || tapped(&runner, sol_ring));
    runner.advance_until_stack_empty();
    assert!(!tapped(&runner, bears));
}

/// "Another target permanent": the Speaker cannot target itself; a legal other
/// target on the same prompt proves the prompt was reached.
#[test]
fn manual_formidable_speaker_cannot_target_itself() {
    let SpeakerBoard {
        mut runner,
        speaker,
        bears,
        ..
    } = speaker_board(&[]);
    activate(&mut runner, speaker, CastPaymentMode::Manual);
    assert!(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(speaker)],
            })
            .is_err(),
        "the Speaker is not another permanent"
    );
    let waiting_for = choose_targets(&mut runner, &[bears]);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
}

/// CR 608.2b: the target is checked again on resolution, not chosen again during
/// payment. A Lightning Bolt cast in response destroys the chosen Grizzly Bears;
/// the untap ability does nothing, and the Forest — never chosen — stays tapped.
#[test]
fn manual_formidable_speaker_does_nothing_when_its_target_is_gone_on_resolution() {
    let SpeakerBoard {
        mut runner,
        speaker,
        bears,
        forest,
        mountain,
        ..
    } = speaker_board(&["Lightning Bolt"]);
    let bolt = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Lightning Bolt")
        .expect("the Bolt is in hand");
    activate(&mut runner, speaker, CastPaymentMode::Manual);
    choose_targets(&mut runner, &[bears]);
    tap_land_for_mana(&mut runner, mountain);
    pass(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the ability is placed"
    );

    // Fund the response with its own red mana, so the activation's payment is
    // untouched by it.
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    drop(runner.cast(bolt).target_object(bears).commit());
    runner.advance_until_stack_empty();
    assert!(
        in_zone(&runner, bears, Zone::Graveyard),
        "the Bolt resolved"
    );
    assert!(tapped(&runner, forest), "nothing else was untapped");
    assert!(tapped(&runner, speaker), "the cost stayed paid");
}

// ---------------------------------------------------------------------------
// Phyrexian symbols: announced while proposed, paid after the window
// ---------------------------------------------------------------------------
//
// CR 118.13a + CR 601.2b: how a Phyrexian symbol of an activation cost will be
// paid is announced as the ability is proposed — after its X, before its
// targets (CR 601.2c) and before its total cost is determined and locked
// (CR 601.2f). A symbol announced as 2 life is not mana in that total, so a
// "can't reduce the mana in that cost to less than one mana" floor (Zirda, the
// Dawnwaker) reads it out (CR 601.2f), and CR 601.2g's window opens only when
// the selected total still includes mana. The life itself is paid with the
// total (CR 601.2h), when CR 119.4 + CR 118.3 check it — after the window,
// whose mana abilities can gain life (Pristine Talisman). A payment-only
// permission (K'rrik, Son of Yawgmoth: "For each {B} in a cost, you may pay 2
// life rather than pay that mana.") changes how a locked {B} is paid, never the
// cost.
//
// Hex Parasite: "{X}{B/P}: Remove up to X counters from target permanent. For
// each counter removed this way, this creature gets +1/+0 until end of turn.
// ({B/P} can be paid with either {B} or 2 life.)" These rows assert the
// announcement, the lock, the window and the payment, that the chosen target is
// the one on the stack, and — once it resolves — that exactly the one chosen
// counter is removed (X = 1 caps the removal, CR 608.2d), the other target is
// untouched and Hex Parasite gets +1/+0 for it (CR 608.2c).

fn submit_phyrexian(runner: &mut GameRunner, choices: &[ShardChoice]) -> WaitingFor {
    act(
        runner,
        GameAction::SubmitPhyrexianChoices {
            choices: choices.to_vec(),
        },
    )
}

/// The open proposal root of the pending activation.
fn proposal(runner: &GameRunner) -> &engine::types::game_state::PendingCast {
    runner
        .state()
        .pending_cast
        .as_deref()
        .expect("the activation's root is pending")
}

fn lock_is_open(runner: &GameRunner) -> bool {
    matches!(
        proposal(runner)
            .activation_cost_snapshot
            .as_deref()
            .map(|snapshot| &snapshot.lock),
        Some(ActivationCostLock::Open { .. })
    )
}

/// Asserts `waiting_for` is the STRUCTURAL announcement prompt: one entry per
/// printed Phyrexian symbol, each offering both payments whatever the current
/// pool and life total (CR 118.13a + CR 601.2b announce; CR 601.2h pays).
fn assert_announcement(waiting_for: &WaitingFor, symbols: usize) {
    let WaitingFor::PhyrexianPayment { shards, .. } = waiting_for else {
        panic!("expected the Phyrexian announcement, got {waiting_for:?}");
    };
    assert_eq!(shards.len(), symbols, "{shards:?}");
    assert!(
        shards
            .iter()
            .all(|shard| shard.options == ShardOptions::ManaOrLife),
        "the announcement is structural: {shards:?}"
    );
}

/// The targets of `source`'s activation on the stack.
fn activation_targets(runner: &GameRunner, source: ObjectId) -> Vec<TargetRef> {
    runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::ActivatedAbility { source_id, ability } if *source_id == source => {
                Some(ability.targets.clone())
            }
            _ => None,
        })
        .expect("the activation is on the stack")
}

fn activate_mana_ability(runner: &mut GameRunner, source: ObjectId) -> WaitingFor {
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(is_mana_ability)
        .expect("a mana ability");
    act(
        runner,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        },
    )
}

struct HexBoard {
    runner: GameRunner,
    hex: ObjectId,
    chosen: ObjectId,
    other: ObjectId,
    swamp: ObjectId,
    mountain: ObjectId,
    wastes: ObjectId,
}

/// Real Hex Parasite with two distinguishable counter-bearing targets, a Swamp,
/// a Mountain and an unused Wastes, at `life`, optionally with Zirda and K'rrik.
fn hex_board(life_total: i32, zirda: bool, krrik: bool) -> HexBoard {
    let mut scenario = scenario_at_main();
    scenario.with_life(P0, life_total);
    let hex = add(&mut scenario, P0, "Hex Parasite", Zone::Battlefield);
    let chosen = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let other = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    scenario.with_counter(chosen, CounterType::Plus1Plus1, 1);
    scenario.with_counter(other, CounterType::Plus1Plus1, 2);
    let swamp = add(&mut scenario, P0, "Swamp", Zone::Battlefield);
    let mountain = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
    let wastes = add(&mut scenario, P0, "Wastes", Zone::Battlefield);
    if zirda {
        add(&mut scenario, P0, "Zirda, the Dawnwaker", Zone::Battlefield);
    }
    if krrik {
        add(
            &mut scenario,
            P0,
            "K'rrik, Son of Yawgmoth",
            Zone::Battlefield,
        );
    }
    HexBoard {
        runner: build(scenario),
        hex,
        chosen,
        other,
        swamp,
        mountain,
        wastes,
    }
}

/// Manual Hex Parasite: X, then the announcement on the open proposal — no
/// target declared, nothing paid — then `choice`, then the target.
fn hex_announce(board: &mut HexBoard, x: u32, choice: ShardChoice) -> WaitingFor {
    let life_before = life(&board.runner, P0);
    let waiting_for = activate_index(&mut board.runner, board.hex, 0, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::ChooseXValue { .. }),
        "CR 601.2b: X first: {waiting_for:?}"
    );
    let waiting_for = act(&mut board.runner, GameAction::ChooseX { value: x });
    assert_announcement(&waiting_for, 1);
    assert!(
        proposal(&board.runner).ability.targets.is_empty(),
        "CR 601.2c: no target is declared before the announcement"
    );
    assert!(
        lock_is_open(&board.runner),
        "CR 601.2f: the cost is not locked yet"
    );
    assert_eq!(
        life(&board.runner, P0),
        life_before,
        "nothing is paid by announcing"
    );
    assert!(
        [board.swamp, board.mountain, board.wastes]
            .iter()
            .all(|land| !tapped(&board.runner, *land)),
        "no mana is paid by announcing"
    );
    let waiting_for = submit_phyrexian(&mut board.runner, &[choice]);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "CR 601.2c: the target follows the announcement: {waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut board.runner, &[board.chosen]);
    assert_eq!(
        proposal_or_stack_targets(&board.runner, board.hex),
        vec![TargetRef::Object(board.chosen)]
    );
    waiting_for
}

/// Resolves the placed X = 1 Hex Parasite activation: its removal asks the
/// controller, capped at one counter on the declared target (CR 608.2d); the
/// controller removes the chosen target's one +1/+1 counter; the other target
/// keeps its counters; and Hex Parasite gets +1/+0 for the one counter removed
/// this way (CR 608.2c).
fn resolve_hex_removing_one(
    runner: &mut GameRunner,
    hex: ObjectId,
    chosen: ObjectId,
    other: ObjectId,
) {
    let other_before = plus_counters(runner, other);
    let power_before = runner.state().objects[&hex].power;
    let mut guard = 0;
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::RemoveCountersChoice { .. }
    ) {
        guard += 1;
        assert!(
            guard < 10 && !runner.state().stack.is_empty(),
            "the activation resolves into its removal choice: {:?}",
            runner.state().waiting_for
        );
        pass(runner);
    }
    let WaitingFor::RemoveCountersChoice {
        source_id,
        max_total,
        ..
    } = runner.state().waiting_for.clone()
    else {
        unreachable!()
    };
    assert_eq!(source_id, chosen, "the declared target");
    assert_eq!(max_total, Some(1), "up to X = 1 counter");
    act(
        runner,
        GameAction::ChooseCountersToRemove {
            selections: vec![CounterRemoveChoice {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            }],
        },
    );
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 10, "{:?}", runner.state().waiting_for);
        pass(runner);
    }
    assert_eq!(
        plus_counters(runner, chosen),
        0,
        "exactly the one chosen counter is removed"
    );
    assert_eq!(
        plus_counters(runner, other),
        other_before,
        "the other target is unchanged"
    );
    assert_eq!(
        runner.state().objects[&hex].power,
        power_before.map(|power| power + 1),
        "+1/+0 for the one counter removed this way"
    );
}

/// The declared target: on the pending root while paying, on the stack after.
fn proposal_or_stack_targets(runner: &GameRunner, source: ObjectId) -> Vec<TargetRef> {
    match runner.state().pending_cast.as_deref() {
        Some(pending) => engine::game::ability_utils::declared_targets_in_chain(&pending.ability),
        None => activation_targets(runner, source),
    }
}

/// CR 118.13a + CR 601.2g: announced as mana, X = 1 owes {1}{B}; the window
/// opens after the target and the player pays it with the Swamp and the
/// Mountain. No life is paid.
#[test]
fn manual_hex_parasite_phyrexian_intent_precedes_payment() {
    let mut board = hex_board(20, false, false);
    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayMana);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert!(board.runner.state().stack.is_empty());

    tap_land_for_mana(&mut board.runner, board.swamp);
    tap_land_for_mana(&mut board.runner, board.mountain);
    let waiting_for = pass(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 20, "the symbol was paid with mana");
    assert_eq!(
        activation_targets(&board.runner, board.hex),
        vec![TargetRef::Object(board.chosen)]
    );
    assert!(
        !tapped(&board.runner, board.wastes),
        "the unused source stays untapped"
    );
    assert_eq!(board.runner.state().stack.len(), 1, "placed once");
    assert_eq!(
        plus_counters(&board.runner, board.other),
        2,
        "the other target is untouched"
    );
    resolve_hex_removing_one(&mut board.runner, board.hex, board.chosen, board.other);
}

/// CR 118.13a + CR 601.2g + CR 601.2h: announced as life, X = 1 still owes {1},
/// so the window still opens; the Mountain pays the {1} and 2 life pay the
/// symbol. The Swamp is never used.
#[test]
fn manual_hex_parasite_life_intent_keeps_x_mana_opportunity() {
    let mut board = hex_board(20, false, false);
    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayLife);
    assert!(
        is_mana_payment(&waiting_for),
        "the selected total still includes {{1}}: {waiting_for:?}"
    );
    assert_eq!(
        life(&board.runner, P0),
        20,
        "the life is paid with the total"
    );

    tap_land_for_mana(&mut board.runner, board.mountain);
    let waiting_for = pass(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 18, "exactly 2 life");
    assert!(
        !tapped(&board.runner, board.swamp),
        "the black mana was not used"
    );
    assert_eq!(board.runner.state().stack.len(), 1);
    resolve_hex_removing_one(&mut board.runner, board.hex, board.chosen, board.other);
}

/// CR 601.2g: X = 0 announced as life has no mana in its total, so there is no
/// window: the 2 life are paid at once and the ability is placed — with and
/// without Zirda, whose floor cannot raise a zero-mana total to one.
#[test]
fn manual_hex_parasite_zero_x_life_intent_needs_no_mana_window() {
    for zirda in [false, true] {
        let mut board = hex_board(20, zirda, false);
        let waiting_for = hex_announce(&mut board, 0, ShardChoice::PayLife);
        assert!(
            is_priority(&waiting_for),
            "zirda {zirda}: no mana, no window: {waiting_for:?}"
        );
        assert_eq!(life(&board.runner, P0), 18, "zirda {zirda}");
        assert!(
            [board.swamp, board.mountain, board.wastes]
                .iter()
                .all(|land| !tapped(&board.runner, *land)),
            "zirda {zirda}: nothing was tapped"
        );
        assert_eq!(board.runner.state().stack.len(), 1, "zirda {zirda}");
        // CR 608.2d: up to X = 0 counters leaves no choice; nothing is removed.
        let power_before = board.runner.state().objects[&board.hex].power;
        while !board.runner.state().stack.is_empty() {
            let waiting_for = pass(&mut board.runner);
            assert!(
                !matches!(waiting_for, WaitingFor::RemoveCountersChoice { .. }),
                "zirda {zirda}: {waiting_for:?}"
            );
        }
        assert_eq!(
            plus_counters(&board.runner, board.chosen),
            1,
            "zirda {zirda}"
        );
        assert_eq!(
            plus_counters(&board.runner, board.other),
            2,
            "zirda {zirda}"
        );
        assert_eq!(
            board.runner.state().objects[&board.hex].power,
            power_before,
            "zirda {zirda}: no counter, no boost"
        );
    }
}

/// Pair of the row above: X = 0 announced as mana owes {B}, so the window opens.
#[test]
fn manual_hex_parasite_zero_x_mana_intent_owes_black() {
    let mut board = hex_board(20, false, false);
    let waiting_for = hex_announce(&mut board, 0, ShardChoice::PayMana);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    tap_land_for_mana(&mut board.runner, board.swamp);
    let waiting_for = pass(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 20);
    assert!(!tapped(&board.runner, board.mountain));
}

/// CR 601.2f + CR 118.13a: under real Zirda, X = 1 announced as life has ONE
/// mana in its selected total ({1}; the symbol is life), so Zirda's "less than
/// one mana" floor removes nothing: the lock is {1} plus 2 life and the window
/// opens for the {1}. Folding the printed {B/P} as mana would have removed the
/// {1} and skipped the window.
#[test]
fn manual_hex_parasite_zirda_selected_life_cost_keeps_one_generic() {
    let mut board = hex_board(20, true, false);
    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayLife);
    assert!(
        is_mana_payment(&waiting_for),
        "the locked total keeps its one generic mana: {waiting_for:?}"
    );
    tap_land_for_mana(&mut board.runner, board.mountain);
    let waiting_for = pass(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 18);
    assert!(
        tapped(&board.runner, board.mountain),
        "one generic mana was paid"
    );
    assert!(!tapped(&board.runner, board.swamp) && !tapped(&board.runner, board.wastes));
    resolve_hex_removing_one(&mut board.runner, board.hex, board.chosen, board.other);
}

/// Its PayMana sibling: {1}{B} has two mana, Zirda reduces it to {B}, which the
/// Swamp alone pays; no life is paid and the other lands stay untapped.
#[test]
fn manual_hex_parasite_zirda_selected_mana_cost_owes_black_alone() {
    let mut board = hex_board(20, true, false);
    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayMana);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    tap_land_for_mana(&mut board.runner, board.swamp);
    let waiting_for = pass(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 20);
    assert!(!tapped(&board.runner, board.mountain) && !tapped(&board.runner, board.wastes));
    resolve_hex_removing_one(&mut board.runner, board.hex, board.chosen, board.other);
}

/// CR 601.2f + CR 601.2h: K'rrik's permission is payment-only. Under Zirda and
/// K'rrik, X = 1 announced as MANA locks {B} (Zirda reduced the {1}); the window
/// opens for it, and only when that {B} is paid does K'rrik offer 2 life for it.
/// The announcement on the root stays "mana" while the payment pays life.
#[test]
fn manual_hex_zirda_krrik_preserves_native_route() {
    let mut board = hex_board(20, true, true);
    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayMana);
    assert!(
        is_mana_payment(&waiting_for),
        "CR 601.2g: the locked {{B}} is a mana payment: {waiting_for:?}"
    );
    let waiting_for = pass(&mut board.runner);
    let WaitingFor::PhyrexianPayment { shards, .. } = &waiting_for else {
        panic!("K'rrik's permission is offered when the {{B}} is paid: {waiting_for:?}");
    };
    assert_eq!(shards.len(), 1, "{shards:?}");
    assert_ne!(shards[0].options, ShardOptions::ManaOnly, "life is offered");
    let native = proposal(&board.runner)
        .activation_phyrexian_intent
        .clone()
        .expect("the announcement is bound")
        .native_choices;
    assert_eq!(
        native
            .iter()
            .map(|record| record.choice)
            .collect::<Vec<_>>(),
        vec![ShardChoice::PayMana],
        "the native announcement is never rewritten"
    );
    let waiting_for = submit_phyrexian(&mut board.runner, &[ShardChoice::PayLife]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 18, "zero mana and exactly 2 life");
    assert!(
        [board.swamp, board.mountain, board.wastes]
            .iter()
            .all(|land| !tapped(&board.runner, *land)),
        "no mana was spent"
    );
    assert_eq!(board.runner.state().stack.len(), 1);
    resolve_hex_removing_one(&mut board.runner, board.hex, board.chosen, board.other);
}

/// The native-life branch of the same board: the lock is {1} plus 2 life; the
/// window opens for the {1}; the symbol announced as life is paid with life and
/// is not offered again, K'rrik or not.
#[test]
fn manual_hex_zirda_krrik_native_life_owes_one_generic_and_two_life() {
    let mut board = hex_board(20, true, true);
    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayLife);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    tap_land_for_mana(&mut board.runner, board.mountain);
    let waiting_for = pass(&mut board.runner);
    assert!(
        is_priority(&waiting_for),
        "nothing is left to choose: {waiting_for:?}"
    );
    assert_eq!(life(&board.runner, P0), 18);
    assert!(!tapped(&board.runner, board.swamp));
    resolve_hex_removing_one(&mut board.runner, board.hex, board.chosen, board.other);
}

/// B3 — CR 601.2g + CR 601.2h + CR 119.3 + CR 119.4: at ONE life, the
/// announcement still offers life (it pays nothing), and the player announces
/// it. After the target and the {1}-plus-2-life lock, the player activates two
/// real Pristine Talismans ("{T}: Add {C}. You gain 1 life.") in the window:
/// +2 colorless and +2 life. The payment then spends one colorless and 2 life,
/// leaving 1 life and one colorless floating.
#[test]
fn manual_hex_parasite_life_paid_after_mana_ability_life_gain() {
    let mut scenario = scenario_at_main();
    scenario.with_life(P0, 1);
    let hex = add(&mut scenario, P0, "Hex Parasite", Zone::Battlefield);
    let chosen = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let other = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    scenario.with_counter(chosen, CounterType::Plus1Plus1, 1);
    scenario.with_counter(other, CounterType::Plus1Plus1, 1);
    let talismans = [
        add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
        add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
    ];
    let mut runner = build(scenario);

    activate_index(&mut runner, hex, 0, CastPaymentMode::Manual);
    let waiting_for = act(&mut runner, GameAction::ChooseX { value: 1 });
    assert_announcement(&waiting_for, 1);
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "the life announcement is accepted at one life: {waiting_for:?}"
    );
    assert_eq!(life(&runner, P0), 1, "nothing was paid");
    let waiting_for = choose_targets(&mut runner, &[chosen]);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");

    for talisman in talismans {
        activate_mana_ability(&mut runner, talisman);
    }
    assert_eq!(life(&runner, P0), 3, "each Talisman gained 1 life");
    assert_eq!(pool_total(&runner), 2, "each Talisman added {{C}}");

    let waiting_for = pass(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&runner, P0), 1, "exactly 2 life were paid");
    assert_eq!(pool_total(&runner), 1, "exactly one generic mana was paid");
    assert_eq!(
        activation_targets(&runner, hex),
        vec![TargetRef::Object(chosen)]
    );
    assert_eq!(runner.state().stack.len(), 1, "placed once");
    resolve_hex_removing_one(&mut runner, hex, chosen, other);
}

/// B3's negative: the same one-life announcement is accepted, but with no life
/// gained in the window the payment cannot be made (CR 119.4 + CR 118.3): it is
/// refused, the window stays and nothing is placed. A three-life sibling pays.
#[test]
fn manual_hex_parasite_life_intent_without_the_life_is_refused_at_payment() {
    for (life_total, payable) in [(1, false), (3, true)] {
        let mut scenario = scenario_at_main();
        scenario.with_life(P0, life_total);
        let hex = add(&mut scenario, P0, "Hex Parasite", Zone::Battlefield);
        let chosen = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
        let wastes = add(&mut scenario, P0, "Wastes", Zone::Battlefield);
        let mut runner = build(scenario);

        activate_index(&mut runner, hex, 0, CastPaymentMode::Manual);
        act(&mut runner, GameAction::ChooseX { value: 1 });
        let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
        assert!(
            matches!(waiting_for, WaitingFor::TargetSelection { .. }),
            "life {life_total}: announced: {waiting_for:?}"
        );
        let window = choose_targets(&mut runner, &[chosen]);
        assert!(is_mana_payment(&window), "life {life_total}: {window:?}");
        tap_land_for_mana(&mut runner, wastes);
        let result = runner.act(GameAction::PassPriority);
        if payable {
            assert!(result.is_ok(), "life {life_total}: {result:?}");
            assert_eq!(life(&runner, P0), life_total - 2);
            assert_eq!(runner.state().stack.len(), 1);
        } else {
            assert!(
                result.is_err(),
                "CR 119.4: the payment is refused at one life"
            );
            assert_eq!(runner.state().waiting_for, window, "the window stays");
            assert_eq!(life(&runner, P0), 1, "no life was paid");
            assert!(runner.state().stack.is_empty(), "nothing was placed");
        }
    }
}

/// Whether the priority legal actions offer `source`'s ability `ability_index`.
fn ability_offered(runner: &GameRunner, source: ObjectId, ability_index: usize) -> bool {
    !offered_payment_modes(runner, source, ability_index).is_empty()
}

/// The payment modes of the priority legal actions' offers of `source`'s
/// ability `ability_index`.
fn offered_payment_modes(
    runner: &GameRunner,
    source: ObjectId,
    ability_index: usize,
) -> Vec<CastPaymentMode> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped
        .get(&source)
        .into_iter()
        .flatten()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                ability_index: index,
                payment_mode,
                ..
            } if *index == ability_index => Some(*payment_mode),
            _ => None,
        })
        .collect()
}

/// B3 class remainder — CR 601.2g + CR 601.2h + CR 119.3 + CR 119.4: a manual
/// offer judges life the way it judges mana. At ONE life with two untapped
/// Pristine Talismans and no black source, Hex Parasite is offered for a manual
/// payment — its {B/P} can be paid with 2 life once the player has activated
/// the Talismans' mana abilities during the window, exactly as the mana the
/// Talismans can produce counts before they are tapped. It is NOT offered for
/// an automatic payment: the automatic payer never activates a mana ability for
/// its life, so it would refuse the 2 life at one life. On the same board with
/// a Wastes instead, nothing can supply the {B} or the life, and nothing is
/// offered in any mode. The offered activation completes: X = 1, life
/// announced, the Talismans activated in the window, one counter removed and
/// +1/+0.
#[test]
fn manual_hex_parasite_is_offered_at_one_life_when_mana_abilities_can_supply_the_life() {
    for talismans in [true, false] {
        let mut scenario = scenario_at_main();
        scenario.with_life(P0, 1);
        let hex = add(&mut scenario, P0, "Hex Parasite", Zone::Battlefield);
        let chosen = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
        let other = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
        scenario.with_counter(chosen, CounterType::Plus1Plus1, 1);
        scenario.with_counter(other, CounterType::Plus1Plus1, 1);
        let sources: Vec<ObjectId> = if talismans {
            vec![
                add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
                add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
            ]
        } else {
            vec![add(&mut scenario, P0, "Wastes", Zone::Battlefield)]
        };
        let mut runner = build(scenario);

        assert_eq!(
            ability_offered(&runner, hex, 0),
            talismans,
            "talismans {talismans}: offered iff the window can supply the life"
        );
        assert_eq!(
            offered_payment_modes(&runner, hex, 0),
            if talismans {
                vec![CastPaymentMode::Manual]
            } else {
                Vec::new()
            },
            "talismans {talismans}: offered for a manual payment only, never Auto"
        );
        assert_eq!(
            !offered_surfaces(&runner, &activation(hex, 0, CastPaymentMode::Manual)).is_empty(),
            talismans,
            "talismans {talismans}: the manual offer follows the window"
        );
        assert!(
            offered_surfaces(&runner, &activation(hex, 0, CastPaymentMode::Auto)).is_empty(),
            "talismans {talismans}: no automatic offer at one life"
        );
        if !talismans {
            continue;
        }

        activate_index(&mut runner, hex, 0, CastPaymentMode::Manual);
        let waiting_for = act(&mut runner, GameAction::ChooseX { value: 1 });
        assert_announcement(&waiting_for, 1);
        submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
        let waiting_for = choose_targets(&mut runner, &[chosen]);
        assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
        for talisman in sources {
            activate_mana_ability(&mut runner, talisman);
        }
        assert_eq!(life(&runner, P0), 3, "each Talisman gained 1 life");
        let waiting_for = pass(&mut runner);
        assert!(is_priority(&waiting_for), "{waiting_for:?}");
        assert_eq!(life(&runner, P0), 1, "exactly 2 life were paid");
        resolve_hex_removing_one(&mut runner, hex, chosen, other);
    }
}

/// The printed "Pay N life" sibling of the row above. Greed ("{B}, Pay 2 life:
/// Draw a card.") at ONE life with a Swamp and two untapped Pristine Talismans
/// is offered for a manual payment: its total includes the {B}, so the window
/// opens before the life is paid (CR 601.2g), and the Talismans can raise the
/// life to 3 in it. It is not offered for an automatic payment, whose payer
/// taps the Swamp for the {B} and then cannot pay 2 life at one life
/// (CR 119.4). The manual activation completes, pays {B} and exactly 2 life,
/// and draws. With no Talisman the same board offers nothing: one life cannot
/// pay 2.
#[test]
fn manual_greed_is_offered_at_one_life_when_mana_abilities_can_supply_the_life() {
    for talismans in [true, false] {
        let mut scenario = scenario_at_main();
        scenario.with_life(P0, 1);
        let greed = add(&mut scenario, P0, "Greed", Zone::Battlefield);
        let swamp = add(&mut scenario, P0, "Swamp", Zone::Battlefield);
        add(&mut scenario, P0, "Grizzly Bears", Zone::Library);
        let sources: Vec<ObjectId> = if talismans {
            vec![
                add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
                add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
            ]
        } else {
            vec![add(&mut scenario, P0, "Wastes", Zone::Battlefield)]
        };
        let mut runner = build(scenario);
        let index = ability_index_of(&runner, greed, "Pay 2 life");

        assert_eq!(
            ability_offered(&runner, greed, index),
            talismans,
            "talismans {talismans}: offered iff the window can supply the life"
        );
        assert_eq!(
            offered_payment_modes(&runner, greed, index),
            if talismans {
                vec![CastPaymentMode::Manual]
            } else {
                Vec::new()
            },
            "talismans {talismans}: offered for a manual payment only, never Auto"
        );
        assert_eq!(
            !offered_surfaces(&runner, &activation(greed, index, CastPaymentMode::Manual))
                .is_empty(),
            talismans,
            "talismans {talismans}: the manual offer follows the window"
        );
        assert!(
            offered_surfaces(&runner, &activation(greed, index, CastPaymentMode::Auto)).is_empty(),
            "talismans {talismans}: no automatic offer at one life"
        );
        if !talismans {
            continue;
        }

        let hand_before = hand_size(&runner, P0);
        let waiting_for = activate_index(&mut runner, greed, index, CastPaymentMode::Manual);
        assert!(
            is_mana_payment(&waiting_for),
            "CR 601.2g: the window precedes the life payment: {waiting_for:?}"
        );
        assert_eq!(life(&runner, P0), 1, "no life is paid before the window");
        tap_land_for_mana(&mut runner, swamp);
        for talisman in sources {
            activate_mana_ability(&mut runner, talisman);
        }
        assert_eq!(life(&runner, P0), 3, "each Talisman gained 1 life");
        let waiting_for = pass(&mut runner);
        assert!(is_priority(&waiting_for), "{waiting_for:?}");
        assert_eq!(life(&runner, P0), 1, "exactly 2 life were paid");
        assert_eq!(
            pool_total(&runner),
            2,
            "the {{B}} was paid; {{C}}{{C}} float"
        );
        assert_eq!(runner.state().stack.len(), 1, "placed once");
        while !runner.state().stack.is_empty() {
            pass(&mut runner);
        }
        assert_eq!(hand_size(&runner, P0), hand_before + 1, "Greed drew a card");
    }
}

/// CR 601.2g + CR 118.13a + CR 119.4: a manual offer's window must belong to a
/// route that can be played. Immolating Souleater ("{R/P}: This creature gets
/// +1/+0 until end of turn.") has no mana but its {R/P}. Announced
/// as 2 life, its total is only life, so no mana-ability opportunity precedes
/// the payment and one life cannot pay it; announced as {R}, two Pristine
/// Talismans only add {C}. At one life with the Talismans and no red source it
/// is offered in no mode. The same ability is offered on the boards that can
/// pay it now — at three life (2 life) and at one life with a Mountain ({R}) —
/// which proves each fixture reaches the offer.
#[test]
fn a_phyrexian_only_activation_with_no_window_route_is_not_offered_at_one_life() {
    for (life_total, mountain, offered) in [
        (1, false, Vec::new()),
        (3, false, vec![CastPaymentMode::Auto]),
        (1, true, vec![CastPaymentMode::Auto]),
    ] {
        let mut scenario = scenario_at_main();
        scenario.with_life(P0, life_total);
        let souleater = add(&mut scenario, P0, "Immolating Souleater", Zone::Battlefield);
        add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield);
        add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield);
        if mountain {
            add(&mut scenario, P0, "Mountain", Zone::Battlefield);
        }
        let runner = build(scenario);
        let index = ability_index_of(&runner, souleater, "+1/+0");

        assert_eq!(
            offered_payment_modes(&runner, souleater, index),
            offered,
            "life {life_total}, mountain {mountain}"
        );
        if offered.is_empty() {
            assert!(
                offered_surfaces(
                    &runner,
                    &activation(souleater, index, CastPaymentMode::Manual)
                )
                .is_empty(),
                "CR 601.2g: no manual route has a window that can supply the life"
            );
        }
    }
}

/// The K'rrik sibling of the row above keeps its offer: Pestilent Souleater
/// ("{B/P}: This creature gains infect until end of turn.") under K'rrik, Son
/// of Yawgmoth ("For each {B} in a cost, you may pay 2 life rather than pay
/// that mana."), at one life with two Pristine Talismans and no black source.
/// Announced as {B} (CR 118.13a), the total includes a mana payment, so the
/// window opens (CR 601.2g); the Talismans raise the life to 3 in it, and the
/// {B} is then paid with 2 life (CR 601.2h + CR 119.4). It is offered for a
/// manual payment only, and the manual activation completes that way.
#[test]
fn a_phyrexian_only_activation_under_krrik_keeps_its_window_route_at_one_life() {
    let mut scenario = scenario_at_main();
    scenario.with_life(P0, 1);
    let souleater = add(&mut scenario, P0, "Pestilent Souleater", Zone::Battlefield);
    add(
        &mut scenario,
        P0,
        "K'rrik, Son of Yawgmoth",
        Zone::Battlefield,
    );
    let talismans = [
        add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
        add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
    ];
    let mut runner = build(scenario);
    let index = ability_index_of(&runner, souleater, "infect");

    assert_eq!(
        offered_payment_modes(&runner, souleater, index),
        vec![CastPaymentMode::Manual],
        "offered for a manual payment only"
    );

    let waiting_for = activate_index(&mut runner, souleater, index, CastPaymentMode::Manual);
    assert_announcement(&waiting_for, 1);
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayMana]);
    assert!(
        is_mana_payment(&waiting_for),
        "CR 601.2g: the {{B}} opens the window: {waiting_for:?}"
    );
    assert_eq!(life(&runner, P0), 1, "nothing is paid before the window");
    for talisman in talismans {
        activate_mana_ability(&mut runner, talisman);
    }
    assert_eq!(life(&runner, P0), 3, "each Talisman gained 1 life");
    let waiting_for = pass(&mut runner);
    let WaitingFor::PhyrexianPayment { shards, .. } = &waiting_for else {
        panic!("K'rrik's permission is offered when the {{B}} is paid: {waiting_for:?}");
    };
    assert_eq!(shards.len(), 1, "{shards:?}");
    assert_ne!(shards[0].options, ShardOptions::ManaOnly, "life is offered");
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(
        life(&runner, P0),
        1,
        "the {{B}} was paid with exactly 2 life"
    );
    assert_eq!(pool_total(&runner), 2, "{{C}}{{C}} float");
    assert_eq!(runner.state().stack.len(), 1, "placed once");
}

const ZOPANDREL: &str = "Reach\nAt the beginning of each combat, double the power and toughness of each creature you control until end of turn.\n{G/P}{G/P}, Sacrifice two other creatures: Put an indestructible counter on Zopandrel. ({G/P} can be paid with either {G} or 2 life.)";

/// CR 601.2b + CR 118.13a + CR 601.2g + CR 119.3 + CR 119.4: each printed
/// Phyrexian symbol's payment is announced on its own, so a cost of two of them
/// has a route that mixes mana and life. Zopandrel, Hunger Dominus ("{G/P}{G/P},
/// Sacrifice two other creatures: Put an indestructible counter on Zopandrel.")
/// at one life with one Forest, two Pristine Talismans and two other creatures:
/// announced as {G}{G} there is one green source; announced as 4 life the total
/// is only life, which one life cannot pay; announced as {G} and 2 life, the
/// total includes a mana payment, the Talismans raise the life to 3 in the
/// window, the Forest pays the {G}, and the 2 life is then paid. It is offered
/// for a manual payment only, and completes that way. Reach guards: at five life
/// the same board is offered for an automatic payment, and at one life without
/// the Forest no route can be paid, so nothing is offered.
#[test]
fn a_mixed_phyrexian_announcement_keeps_its_window_route_at_one_life() {
    for (life_total, forest, offered) in [
        (1, true, vec![CastPaymentMode::Manual]),
        (5, true, vec![CastPaymentMode::Auto]),
        (1, false, Vec::new()),
    ] {
        let mut scenario = scenario_at_main();
        scenario.with_life(P0, life_total);
        let zopandrel = scenario
            .add_creature_from_oracle(P0, "Zopandrel, Hunger Dominus", 4, 6, ZOPANDREL)
            .id();
        let bears = [
            add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield),
            add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield),
        ];
        let talismans = [
            add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
            add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
        ];
        let forest_id = forest.then(|| add(&mut scenario, P0, "Forest", Zone::Battlefield));
        let mut runner = build(scenario);
        let index = ability_index_of(&runner, zopandrel, "indestructible counter");

        assert_eq!(
            offered_payment_modes(&runner, zopandrel, index),
            offered,
            "life {life_total}, forest {forest}"
        );
        let Some(forest_id) = forest_id.filter(|_| life_total == 1) else {
            continue;
        };

        let waiting_for = activate_index(&mut runner, zopandrel, index, CastPaymentMode::Manual);
        assert_announcement(&waiting_for, 2);
        let waiting_for =
            submit_phyrexian(&mut runner, &[ShardChoice::PayMana, ShardChoice::PayLife]);
        assert!(
            is_mana_payment(&waiting_for),
            "CR 601.2g: the announced {{G}} opens the window: {waiting_for:?}"
        );
        assert_eq!(life(&runner, P0), 1, "nothing is paid before the window");
        for talisman in talismans {
            activate_mana_ability(&mut runner, talisman);
        }
        assert_eq!(life(&runner, P0), 3, "each Talisman gained 1 life");
        tap_land_for_mana(&mut runner, forest_id);
        let mut waiting_for = pass(&mut runner);
        if matches!(
            waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ) {
            waiting_for = select_cards(&mut runner, &bears);
        }
        assert!(is_priority(&waiting_for), "{waiting_for:?}");
        assert_eq!(life(&runner, P0), 1, "the life symbol was paid with 2 life");
        assert_eq!(
            pool_total(&runner),
            2,
            "{{C}}{{C}} float; the Forest paid the {{G}}"
        );
        assert!(
            bears
                .iter()
                .all(|id| in_zone(&runner, *id, Zone::Graveyard)),
            "both other creatures were sacrificed"
        );
        assert_eq!(runner.state().stack.len(), 1, "placed once");
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&zopandrel]
                .counters
                .get(&CounterType::Keyword(KeywordKind::Indestructible))
                .copied(),
            Some(1),
            "the ability resolved once"
        );
    }
}

/// TYPED CONTRACT (Announcement election, manual-only offer). CR 601.2f +
/// CR 602.2b + CR 601.2g + CR 119.3 + CR 119.4: an activation offered only for a
/// manual payment, because its life is affordable only after the window, keeps
/// that offer when it raises the Announcement-point cost election: the
/// election's legality dry run runs in the activation's own manual mode.
/// "{3}, Pay 2 life: Draw a card." under a reducer of {2} floored at two mana
/// and an unfloored {1} reducer locks {1} or {2}, depending on the order. At one
/// life with two Pristine Talismans the activation is offered for a manual
/// payment only; activated so, it raises the election, and the elected {1} is
/// paid from the window with exactly 2 life. Reach guard: at twenty life the
/// automatic activation raises the same election.
#[test]
fn typed_manual_only_activation_raises_its_cost_election() {
    const TWO_MANA_FLOOR_TWO_LESS: &str = "Activated abilities of creatures you control cost {2} less to activate. This effect can't reduce the mana in that cost to less than two mana.";
    const CONTRACT: &str = "{3}, Pay 2 life: Draw a card.";
    for (life_total, mode) in [(20, CastPaymentMode::Auto), (1, CastPaymentMode::Manual)] {
        let mut scenario = scenario_at_main();
        scenario.with_life(P0, life_total);
        for name in ["L1", "L2", "L3"] {
            scenario.add_card_to_library_top(P0, name);
        }
        scenario.add_artifact_from_oracle(
            P0,
            "Typed Two-Mana Floor Reducer",
            TWO_MANA_FLOOR_TWO_LESS,
        );
        scenario.add_artifact_from_oracle(P0, "Typed Unfloored Reducer", UNFLOORED_ONE_LESS);
        let contract = scenario
            .add_creature_from_oracle(P0, "Typed Manual-Only Election Contract", 1, 1, CONTRACT)
            .id();
        let talismans = [
            add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
            add(&mut scenario, P0, "Pristine Talisman", Zone::Battlefield),
        ];
        let mut runner = build(scenario);

        assert_eq!(
            offered_payment_modes(&runner, contract, 0),
            vec![mode],
            "life {life_total}: the offered mode"
        );
        let waiting_for = activate_index(&mut runner, contract, 0, mode);
        assert_eq!(
            election_totals_of(&waiting_for),
            vec![1, 2],
            "life {life_total}: the two orders lock {{1}} and {{2}}"
        );
        assert_eq!(
            election_lock_point(&waiting_for),
            Some(ActivationCostLockPoint::Announcement)
        );
        if mode != CastPaymentMode::Manual {
            continue;
        }

        let hand_before = hand_size(&runner, P0);
        let waiting_for = elect_locking(&mut runner, 1);
        assert!(
            is_mana_payment(&waiting_for),
            "CR 601.2g: the elected {{1}} opens the window: {waiting_for:?}"
        );
        assert_eq!(life(&runner, P0), 1, "nothing is paid before the window");
        for talisman in talismans {
            activate_mana_ability(&mut runner, talisman);
        }
        assert_eq!(life(&runner, P0), 3, "each Talisman gained 1 life");
        let waiting_for = pass(&mut runner);
        assert!(is_priority(&waiting_for), "{waiting_for:?}");
        assert_eq!(life(&runner, P0), 1, "exactly 2 life were paid");
        assert_eq!(pool_total(&runner), 1, "exactly the elected {{1}} was paid");
        assert_eq!(runner.state().stack.len(), 1, "placed once");
        runner.advance_until_stack_empty();
        assert_eq!(hand_size(&runner, P0), hand_before + 1, "drew once");
    }
}

/// CR 733.1: the announcement prompt can be cancelled — nothing was paid — and
/// so can the window after it; the redo pays once.
#[test]
fn manual_hex_parasite_cancels_from_the_announcement_and_from_the_window() {
    let mut board = hex_board(20, false, false);
    activate_index(&mut board.runner, board.hex, 0, CastPaymentMode::Manual);
    let waiting_for = act(&mut board.runner, GameAction::ChooseX { value: 1 });
    assert_announcement(&waiting_for, 1);
    let waiting_for = cancel(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(board.runner.state().pending_cast.is_none());
    assert!(board.runner.state().stack.is_empty());
    assert_eq!(life(&board.runner, P0), 20);

    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayLife);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    let waiting_for = cancel(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(board.runner.state().stack.is_empty());
    assert_eq!(life(&board.runner, P0), 20, "no life was paid");

    let waiting_for = hex_announce(&mut board, 1, ShardChoice::PayLife);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    tap_land_for_mana(&mut board.runner, board.mountain);
    pass(&mut board.runner);
    assert_eq!(life(&board.runner, P0), 18, "the redo pays once");
    assert_eq!(board.runner.state().stack.len(), 1);
}

/// A raw and a trusted save at the window keep the bound announcement, the
/// target and the locked {1}-plus-2-life total: the restored game pays exactly
/// that, with no new announcement.
#[test]
fn manual_hex_parasite_announcement_survives_raw_and_trusted_restore() {
    for trusted in [false, true] {
        let mut board = hex_board(20, true, false);
        let window = hex_announce(&mut board, 1, ShardChoice::PayLife);
        assert!(is_mana_payment(&window), "{window:?}");
        let persisted = if trusted {
            PersistedGameState::capture(board.runner.state().clone())
        } else {
            PersistedGameState::Raw(Box::new(board.runner.state().clone()))
        };
        let json = serde_json::to_string(&persisted).expect("the window serializes");
        let db = shared_card_db().expect("the checked-in fixture contains the cards");
        let state = serde_json::from_str::<PersistedGameState>(&json)
            .expect("the save decodes")
            .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
            .expect("the paused save is admissible")
            .finalize_after_rehydration(|state| {
                rehydrate_game_from_card_db(state, db);
                Ok(())
            })
            .expect("the rehydrated save is publishable");
        board.runner = GameRunner::from_state(state);
        assert!(is_mana_payment(&board.runner.state().waiting_for));
        let intent = proposal(&board.runner)
            .activation_phyrexian_intent
            .clone()
            .unwrap_or_else(|| panic!("trusted {trusted}: the announcement survives"));
        assert_eq!(
            intent
                .native_choices
                .iter()
                .map(|r| r.choice)
                .collect::<Vec<_>>(),
            vec![ShardChoice::PayLife]
        );
        tap_land_for_mana(&mut board.runner, board.mountain);
        let waiting_for = pass(&mut board.runner);
        assert!(
            is_priority(&waiting_for),
            "trusted {trusted}: {waiting_for:?}"
        );
        assert_eq!(life(&board.runner, P0), 18, "trusted {trusted}");
        assert_eq!(
            activation_targets(&board.runner, board.hex),
            vec![TargetRef::Object(board.chosen)]
        );
        resolve_hex_removing_one(&mut board.runner, board.hex, board.chosen, board.other);
    }
}

/// The Auto sibling, independently derived from the card: an automatic Hex
/// Parasite with X = 1 and a Swamp available pays {1}{B} with mana or life by
/// its existing payment-time choice; Manual's announcement does not change it.
#[test]
fn auto_hex_parasite_keeps_its_payment_time_phyrexian_choice() {
    let mut board = hex_board(20, false, false);
    let waiting_for = activate_index(&mut board.runner, board.hex, 0, CastPaymentMode::Auto);
    assert!(
        matches!(waiting_for, WaitingFor::ChooseXValue { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = act(&mut board.runner, GameAction::ChooseX { value: 1 });
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "Auto makes no announcement before its target: {waiting_for:?}"
    );
}

/// The automatic X cap's life budget, on Hex Parasite ("{X}{B/P}: ...") with
/// a Swamp, a Mountain and a Wastes. CR 601.2b announces X before the {B/P};
/// CR 107.4f lets the symbol be paid with {B} or 2 life; CR 119.4 lets 2 life
/// be paid only from a life total of at least 2. At 20 life the symbol can be
/// life, so the three lands fund X = 3; at 1 life it must be the Swamp's {B},
/// leaving two lands for X: the cap is 2, X = 3 is refused, and X = 2 is paid
/// by tapping all three lands with no life paid.
#[test]
fn auto_hex_parasite_x_cap_counts_phyrexian_life_only_within_the_life_total() {
    fn auto_x_max(board: &mut HexBoard) -> u32 {
        let waiting_for = activate_index(&mut board.runner, board.hex, 0, CastPaymentMode::Auto);
        let WaitingFor::ChooseXValue { max, .. } = waiting_for else {
            panic!("CR 601.2b: X is announced first: {waiting_for:?}");
        };
        max
    }

    let mut rich = hex_board(20, false, false);
    assert_eq!(
        auto_x_max(&mut rich),
        3,
        "at 20 life the {{B/P}} may be 2 life, so every land funds X"
    );

    let mut board = hex_board(1, false, false);
    assert_eq!(
        auto_x_max(&mut board),
        2,
        "CR 119.4: 1 life cannot pay the {{B/P}}, so the Swamp's {{B}} is not X"
    );
    assert!(
        board.runner.act(GameAction::ChooseX { value: 3 }).is_err(),
        "X = 3 exceeds the cap"
    );
    let waiting_for = act(&mut board.runner, GameAction::ChooseX { value: 2 });
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut board.runner, &[board.chosen]);
    assert!(
        is_priority(&waiting_for),
        "the automatic payer pays X = 2 and the {{B}}: {waiting_for:?}"
    );
    assert_eq!(life(&board.runner, P0), 1, "no life was paid");
    assert!(
        [board.swamp, board.mountain, board.wastes]
            .iter()
            .all(|land| tapped(&board.runner, *land)),
        "the Swamp pays the {{B}} and the other two lands pay X = 2"
    );
    assert_eq!(
        activation_targets(&board.runner, board.hex),
        vec![TargetRef::Object(board.chosen)]
    );
}

/// Birthing Pod ("{1}{G/P}, {T}, Sacrifice a creature: Search your library for
/// a creature card with mana value equal to 1 plus the sacrificed creature's
/// mana value, put that card onto the battlefield, then shuffle. Activate only
/// as a sorcery."): no X and no target, so the announcement is the first
/// prompt; life still leaves {1} for the window; the tap and the sacrifice come
/// after it, once, and the search reads the sacrificed creature.
fn pod_board(zirda: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = scenario_at_main();
    let pod = add(&mut scenario, P0, "Birthing Pod", Zone::Battlefield);
    let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let forest = add(&mut scenario, P0, "Forest", Zone::Battlefield);
    let wastes = add(&mut scenario, P0, "Wastes", Zone::Battlefield);
    let courser = add(&mut scenario, P0, "Centaur Courser", Zone::Library);
    add(&mut scenario, P0, "Hill Giant", Zone::Library);
    if zirda {
        add(&mut scenario, P0, "Zirda, the Dawnwaker", Zone::Battlefield);
    }
    (build(scenario), pod, bears, forest, wastes, courser)
}

fn finish_pod(runner: &mut GameRunner, bears: ObjectId, courser: ObjectId) {
    let waiting_for = runner.state().waiting_for.clone();
    assert!(
        matches!(
            waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "the sacrifice follows the window: {waiting_for:?}"
    );
    let waiting_for = select_cards(runner, &[bears]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert!(in_zone(runner, bears, Zone::Graveyard), "sacrificed once");
    assert_eq!(runner.state().stack.len(), 1);
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(
                    cards,
                    vec![courser],
                    "mana value 3 = 1 + the sacrificed Grizzly Bears' 2"
                );
                select_cards(runner, &[courser]);
            }
            _ if runner.state().stack.is_empty() => break,
            _ => {
                pass(runner);
            }
        }
    }
    assert!(
        in_zone(runner, courser, Zone::Battlefield),
        "the search resolved"
    );
}

#[test]
fn manual_birthing_pod_phyrexian_intent_keeps_generic_opportunity() {
    let (mut runner, pod, bears, forest, wastes, courser) = pod_board(false);
    let waiting_for = activate(&mut runner, pod, CastPaymentMode::Manual);
    assert_announcement(&waiting_for, 1);
    assert!(lock_is_open(&runner));
    assert!(!tapped(&runner, pod) && in_zone(&runner, bears, Zone::Battlefield));
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
    assert!(
        is_mana_payment(&waiting_for),
        "{{1}} remains a mana payment: {waiting_for:?}"
    );
    assert!(!tapped(&runner, pod) && in_zone(&runner, bears, Zone::Battlefield));
    tap_land_for_mana(&mut runner, wastes);
    pass(&mut runner);
    assert_eq!(life(&runner, P0), 18, "2 life for {{G/P}}");
    assert!(!tapped(&runner, forest), "the green source was not needed");
    finish_pod(&mut runner, bears, courser);
    assert!(tapped(&runner, pod), "{{T}} was paid once");
}

#[test]
fn manual_birthing_pod_zirda_selected_life_cost_keeps_one_generic() {
    let (mut runner, pod, bears, forest, wastes, courser) = pod_board(true);
    activate(&mut runner, pod, CastPaymentMode::Manual);
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
    assert!(
        is_mana_payment(&waiting_for),
        "Zirda's floor keeps the one generic mana: {waiting_for:?}"
    );
    tap_land_for_mana(&mut runner, wastes);
    pass(&mut runner);
    assert_eq!(life(&runner, P0), 18);
    assert!(!tapped(&runner, forest));
    finish_pod(&mut runner, bears, courser);
}

#[test]
fn manual_birthing_pod_zirda_selected_mana_cost_owes_green_alone() {
    let (mut runner, pod, bears, forest, wastes, courser) = pod_board(true);
    activate(&mut runner, pod, CastPaymentMode::Manual);
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayMana]);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    tap_land_for_mana(&mut runner, forest);
    pass(&mut runner);
    assert_eq!(life(&runner, P0), 20, "the symbol was paid with green");
    assert!(!tapped(&runner, wastes), "Zirda removed the {{1}}");
    finish_pod(&mut runner, bears, courser);
}

/// B2 — Phyrexian Reclamation ("{1}{B}, Pay 2 life: Return target creature card
/// from your graveyard to your hand.") under Zirda and K'rrik. CR 601.2f: Zirda
/// reduces {1}{B} to {B} (the floor counts the ordinary {B}); the printed 2 life
/// stays. CR 601.2g: the locked {B} opens the window. CR 601.2h: K'rrik lets the
/// {B} be paid with 2 life — zero mana and four life in all. The Swamp tapped in
/// the window keeps its {B} unspent: the opportunity was taken, the permission
/// was still the player's choice.
#[test]
fn manual_reclamation_zirda_krrik_locks_black_then_pays_four_life() {
    for tap_swamp in [false, true] {
        let mut scenario = scenario_at_main();
        let reclamation = add(
            &mut scenario,
            P0,
            "Phyrexian Reclamation",
            Zone::Battlefield,
        );
        add(&mut scenario, P0, "Zirda, the Dawnwaker", Zone::Battlefield);
        add(
            &mut scenario,
            P0,
            "K'rrik, Son of Yawgmoth",
            Zone::Battlefield,
        );
        let swamp = add(&mut scenario, P0, "Swamp", Zone::Battlefield);
        let mountain = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
        let returned = add(&mut scenario, P0, "Grizzly Bears", Zone::Graveyard);
        let stays = add(&mut scenario, P0, "Hill Giant", Zone::Graveyard);
        let mut runner = build(scenario);

        let waiting_for = activate(&mut runner, reclamation, CastPaymentMode::Manual);
        assert!(
            matches!(waiting_for, WaitingFor::TargetSelection { .. }),
            "no announcement for an ordinary {{B}}: {waiting_for:?}"
        );
        let waiting_for = choose_targets(&mut runner, &[returned]);
        assert!(
            is_mana_payment(&waiting_for),
            "the locked {{B}} opens the window: {waiting_for:?}"
        );
        assert_eq!(life(&runner, P0), 20, "nothing is paid at the window");
        if tap_swamp {
            tap_land_for_mana(&mut runner, swamp);
            assert_eq!(pool_total(&runner), 1);
        }
        let waiting_for = pass(&mut runner);
        let WaitingFor::PhyrexianPayment { shards, .. } = &waiting_for else {
            panic!("K'rrik's permission is offered at payment: {waiting_for:?}");
        };
        assert_eq!(
            shards.len(),
            1,
            "one {{B}}: Zirda left no generic: {shards:?}"
        );
        let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
        assert!(is_priority(&waiting_for), "{waiting_for:?}");
        assert_eq!(life(&runner, P0), 16, "tap {tap_swamp}: four life in all");
        assert_eq!(
            pool_total(&runner),
            usize::from(tap_swamp),
            "tap {tap_swamp}: zero mana was spent"
        );
        assert!(!tapped(&runner, mountain), "no generic mana was owed");
        runner.advance_until_stack_empty();
        assert!(
            in_zone(&runner, returned, Zone::Hand),
            "the chosen card returned"
        );
        assert!(in_zone(&runner, stays, Zone::Graveyard), "the other stayed");
    }
}

/// The wire shape of the new root field: absent unless bound, an announcement
/// and a later payment selection round-trip unchanged, and a payload predating
/// the field reads as none.
#[test]
fn the_phyrexian_intent_of_a_pending_activation_is_optional_on_the_wire() {
    use engine::types::game_state::{
        ActivationPhyrexianIntent, ActivationShardChoice, PendingCast,
    };
    let mut board = hex_board(20, false, false);
    let window = hex_announce(&mut board, 1, ShardChoice::PayMana);
    assert!(is_mana_payment(&window));
    let pending: PendingCast = proposal(&board.runner).clone();
    let json = serde_json::to_value(&pending).expect("serializes");
    assert!(json.get("activation_phyrexian_intent").is_some());
    assert!(
        json["activation_phyrexian_intent"]
            .get("payment_choices")
            .is_none(),
        "no payment is selected yet: {json}"
    );
    let back: PendingCast = serde_json::from_value(json.clone()).expect("decodes");
    assert_eq!(
        back.activation_phyrexian_intent,
        pending.activation_phyrexian_intent
    );
    assert_eq!(back.payment_mode, CastPaymentMode::Manual);

    let mut legacy = json.clone();
    legacy
        .as_object_mut()
        .expect("an object")
        .remove("activation_phyrexian_intent");
    let legacy: PendingCast = serde_json::from_value(legacy).expect("a legacy root decodes");
    assert_eq!(legacy.activation_phyrexian_intent, None);

    let mixed = ActivationPhyrexianIntent {
        native_choices: vec![ActivationShardChoice {
            shard_index: 0,
            shard: ManaCostShard::PhyrexianBlack,
            choice: ShardChoice::PayMana,
        }],
        payment_choices: Some(vec![ActivationShardChoice {
            shard_index: 0,
            shard: ManaCostShard::PhyrexianBlack,
            choice: ShardChoice::PayLife,
        }]),
    };
    let grant_only = ActivationPhyrexianIntent {
        native_choices: Vec::new(),
        payment_choices: Some(vec![ActivationShardChoice {
            shard_index: 1,
            shard: ManaCostShard::Black,
            choice: ShardChoice::PayLife,
        }]),
    };
    for intent in [mixed, grant_only] {
        let text = serde_json::to_string(&intent).expect("serializes");
        assert_eq!(
            serde_json::from_str::<ActivationPhyrexianIntent>(&text).expect("decodes"),
            intent
        );
    }
}

// ---------------------------------------------------------------------------
// Typed contracts (parsed definitions, not printed cards)
// ---------------------------------------------------------------------------
//
// No printed activated ability is modal AND carries a Phyrexian symbol, and none
// mixes a grant-eligible ordinary symbol with repeated Phyrexian ones. These rows
// use clearly labelled parsed definitions to pin the two routing seams those
// shapes reach; their prices are derived from the typed costs themselves.

/// TYPED CONTRACT: a native Phyrexian alternative is selected while the manual
/// proposal is open (CR 118.13a + CR 602.2b). Reentry keeps the original OneOf
/// definition on the activation record even though the payment fold uses only
/// the selected branch.
#[test]
fn typed_manual_phyrexian_alternatives_preserve_the_original_cost_record() {
    for choose_phyrexian in [false, true] {
        let mut scenario = scenario_at_main();
        let contract = scenario
            .add_creature_from_oracle(
                P0,
                "Typed Phyrexian Alternative Contract",
                1,
                1,
                "{W/P} or {1}: You gain 1 life.",
            )
            .id();
        let mountain = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
        let mut runner = build(scenario);
        let original_cost = runner.state().objects[&contract].abilities[0]
            .cost
            .clone()
            .expect("the parsed contract has an activation cost");
        assert!(matches!(&original_cost, AbilityCost::OneOf { costs } if costs.len() == 2));

        let waiting_for = activate_index(&mut runner, contract, 0, CastPaymentMode::Manual);
        assert!(
            matches!(&waiting_for, WaitingFor::ActivationCostOneOfChoice { player, .. } if *player == P0)
        );
        let phyrexian_index = alternative_paying(&waiting_for, ManaCostShard::PhyrexianWhite);
        let index = if choose_phyrexian {
            phyrexian_index
        } else {
            1 - phyrexian_index
        };
        let waiting_for = act(
            &mut runner,
            GameAction::ChooseActivationCostBranch { index },
        );
        let result = if choose_phyrexian {
            assert_announcement(&waiting_for, 1);
            runner.act(GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayLife],
            })
        } else {
            assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
            tap_land_for_mana(&mut runner, mountain);
            runner.act(GameAction::PassPriority)
        }
        .expect("the selected activation cost is paid");
        assert!(is_priority(&result.waiting_for), "{:?}", result.waiting_for);
        assert_eq!(runner.state().stack.len(), 1);
        let activation_costs: Vec<_> = result
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::AbilityActivated {
                    source_id,
                    activation_cost,
                    ..
                } if *source_id == contract => Some(activation_cost),
                _ => None,
            })
            .collect();
        assert_eq!(
            activation_costs,
            vec![&BoundActivationCost::Cost(Box::new(original_cost))],
            "selecting a payment branch publishes the original cost exactly once"
        );
        assert_eq!(life(&runner, P0), if choose_phyrexian { 18 } else { 20 });
        assert_eq!(tapped(&runner, mountain), !choose_phyrexian);

        runner.advance_until_stack_empty();
        assert_eq!(life(&runner, P0), if choose_phyrexian { 19 } else { 21 });
        assert!(runner.state().stack.is_empty());
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
        );
    }
}

/// TYPED CONTRACT (mixed domain): "{B}{W/P}{W/P}{R}: You gain 1 life." under
/// K'rrik. The announcement covers exactly the two printed Phyrexian symbols, by
/// their own indices (1 and 2, the same symbol twice); the payment selection
/// covers the symbols the live permission makes life-payable on the payment
/// view — the ordinary {B} (index 0) and the symbol announced as life (index 2,
/// forced). The two domains differ in length and position and are never zipped.
#[test]
fn typed_mixed_phyrexian_domain_keeps_native_and_payment_records_apart() {
    let mut scenario = scenario_at_main();
    let contract = scenario
        .add_creature_from_oracle(
            P0,
            "Typed Phyrexian Domain Contract",
            1,
            1,
            "{B}{W/P}{W/P}{R}: You gain 1 life.",
        )
        .id();
    add(
        &mut scenario,
        P0,
        "K'rrik, Son of Yawgmoth",
        Zone::Battlefield,
    );
    let plains = add(&mut scenario, P0, "Plains", Zone::Battlefield);
    let mountain = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
    let swamp = add(&mut scenario, P0, "Swamp", Zone::Battlefield);
    let mut runner = build(scenario);

    let waiting_for = activate_index(&mut runner, contract, 0, CastPaymentMode::Manual);
    assert_announcement(&waiting_for, 2);
    let WaitingFor::PhyrexianPayment { shards, .. } = &waiting_for else {
        panic!("expected the announcement, got {waiting_for:?}");
    };
    assert_eq!(
        shards
            .iter()
            .map(|shard| shard.shard_index)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "only the printed Phyrexian symbols are announced; the {{B}} is not"
    );
    // Wrong length: refused, nothing bound.
    assert!(runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife],
        })
        .is_err());
    assert!(proposal(&runner).activation_phyrexian_intent.is_none());
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayMana, ShardChoice::PayLife]);
    assert!(
        is_mana_payment(&waiting_for),
        "{{B}}, {{W}} and {{R}} are mana in the selected total: {waiting_for:?}"
    );

    tap_land_for_mana(&mut runner, plains);
    tap_land_for_mana(&mut runner, mountain);
    let waiting_for = pass(&mut runner);
    let WaitingFor::PhyrexianPayment { shards, .. } = &waiting_for else {
        panic!("the payment selection is offered: {waiting_for:?}");
    };
    assert_eq!(
        shards
            .iter()
            .map(|shard| shard.shard_index)
            .collect::<Vec<_>>(),
        vec![0, 2],
        "the payment view's life-payable symbols: K'rrik's {{B}} and the announced life"
    );
    assert_eq!(
        shards[1].options,
        ShardOptions::LifeOnly,
        "the symbol announced as life is forced"
    );
    let intent = proposal(&runner)
        .activation_phyrexian_intent
        .clone()
        .expect("the announcement is bound");
    assert_eq!(
        intent
            .native_choices
            .iter()
            .map(|record| (record.shard_index, record.shard, record.choice))
            .collect::<Vec<_>>(),
        vec![
            (1, ManaCostShard::PhyrexianWhite, ShardChoice::PayMana),
            (2, ManaCostShard::PhyrexianWhite, ShardChoice::PayLife),
        ]
    );
    assert_eq!(intent.payment_choices, None, "no payment is selected yet");

    // Paying the announced-life symbol with mana is refused, with the prompt kept.
    assert!(runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife, ShardChoice::PayMana],
        })
        .is_err());
    assert_eq!(runner.state().waiting_for, waiting_for);

    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife, ShardChoice::PayLife]);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(
        life(&runner, P0),
        16,
        "2 life for the {{B}} and 2 for the {{W/P}}"
    );
    assert!(!tapped(&runner, swamp), "the {{B}} was paid with life");
    assert_eq!(runner.state().stack.len(), 1);
}

/// TYPED CONTRACT (modal): "{W/P}: Choose one — • You gain 1 life. • Draw a
/// card." CR 601.2b + CR 700.2a: the modes are announced first, then the
/// Phyrexian symbol, then the lock — the mode prompt carries the still-open
/// cost carrier. Announced as life, the total has no mana: no window.
#[test]
fn typed_modal_phyrexian_activation_announces_its_symbol_after_its_modes() {
    for choice in [ShardChoice::PayLife, ShardChoice::PayMana] {
        let mut scenario = scenario_at_main();
        let contract = scenario
            .add_creature_from_oracle(
                P0,
                "Typed Modal Phyrexian Contract",
                1,
                1,
                "{W/P}: Choose one —\n• You gain 1 life.\n• Draw a card.",
            )
            .id();
        let plains = add(&mut scenario, P0, "Plains", Zone::Battlefield);
        let mut runner = build(scenario);

        let waiting_for = activate_index(&mut runner, contract, 0, CastPaymentMode::Manual);
        let WaitingFor::AbilityModeChoice {
            activation_cost_snapshot,
            ..
        } = &waiting_for
        else {
            panic!("{choice:?}: the modes are announced first: {waiting_for:?}");
        };
        assert!(
            matches!(
                activation_cost_snapshot
                    .as_deref()
                    .map(|snapshot| &snapshot.lock),
                Some(ActivationCostLock::Open { .. })
            ),
            "{choice:?}: the lock waits for the announcement after the modes"
        );
        let waiting_for = act(&mut runner, GameAction::SelectModes { indices: vec![0] });
        assert_announcement(&waiting_for, 1);
        let waiting_for = submit_phyrexian(&mut runner, &[choice]);
        match choice {
            ShardChoice::PayLife => {
                assert!(
                    is_priority(&waiting_for),
                    "no mana, no window: {waiting_for:?}"
                );
                assert_eq!(life(&runner, P0), 18);
                assert!(!tapped(&runner, plains));
            }
            ShardChoice::PayMana => {
                assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
                tap_land_for_mana(&mut runner, plains);
                let waiting_for = pass(&mut runner);
                assert!(is_priority(&waiting_for), "{waiting_for:?}");
                assert_eq!(life(&runner, P0), 20);
            }
        }
        assert_eq!(runner.state().stack.len(), 1, "{choice:?}");
        runner.advance_until_stack_empty();
        assert_eq!(
            life(&runner, P0),
            match choice {
                ShardChoice::PayLife => 19,
                ShardChoice::PayMana => 21,
            },
            "{choice:?}: mode 0 resolved"
        );
    }
}

/// CR 605.3a + CR 601.2g: the life announcement does not depend on a source the
/// automatic payer would pick. Hex Parasite with X = 1 announced as life, with
/// Ashnod's Altar ("Sacrifice a creature: Add {C}{C}.") as the only mana source:
/// in the window the player activates the Altar, chooses the creature, and the
/// {1} plus 2 life are paid once from the same root.
#[test]
fn manual_hex_parasite_life_intent_funded_by_a_costed_mana_ability() {
    let mut scenario = scenario_at_main();
    let hex = add(&mut scenario, P0, "Hex Parasite", Zone::Battlefield);
    let chosen = add(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    scenario.with_counter(chosen, CounterType::Plus1Plus1, 1);
    let altar = add(&mut scenario, P0, "Ashnod's Altar", Zone::Battlefield);
    let fodder = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let mut runner = build(scenario);

    activate_index(&mut runner, hex, 0, CastPaymentMode::Manual);
    let waiting_for = act(&mut runner, GameAction::ChooseX { value: 1 });
    assert_announcement(&waiting_for, 1);
    let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut runner, &[chosen]);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");

    let waiting_for = activate_mana_ability(&mut runner, altar);
    assert!(
        matches!(
            waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "the Altar's sacrifice is chosen in the window: {waiting_for:?}"
    );
    let waiting_for = select_cards(&mut runner, &[fodder]);
    assert!(
        is_mana_payment(&waiting_for),
        "the same root's window resumes: {waiting_for:?}"
    );
    assert_eq!(pool_total(&runner), 2);
    let intent = proposal(&runner)
        .activation_phyrexian_intent
        .clone()
        .expect("the announcement survives the source's cost");
    assert_eq!(
        intent
            .native_choices
            .iter()
            .map(|record| record.choice)
            .collect::<Vec<_>>(),
        vec![ShardChoice::PayLife]
    );

    let waiting_for = pass(&mut runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&runner, P0), 18, "2 life, once");
    assert_eq!(pool_total(&runner), 1, "one generic mana, once");
    assert_eq!(
        activation_targets(&runner, hex),
        vec![TargetRef::Object(chosen)]
    );
}

/// A raw and a trusted save of the OPEN announcement — the activation proposed,
/// its X announced, nothing locked, targeted or paid — restore to the same
/// prompt, and the restored answer continues the same proposal.
#[test]
fn manual_hex_parasite_open_announcement_survives_raw_and_trusted_restore() {
    for trusted in [false, true] {
        let mut board = hex_board(20, false, false);
        activate_index(&mut board.runner, board.hex, 0, CastPaymentMode::Manual);
        let prompt = act(&mut board.runner, GameAction::ChooseX { value: 1 });
        assert_announcement(&prompt, 1);
        let persisted = if trusted {
            PersistedGameState::capture(board.runner.state().clone())
        } else {
            PersistedGameState::Raw(Box::new(board.runner.state().clone()))
        };
        let json = serde_json::to_string(&persisted).expect("the prompt serializes");
        let db = shared_card_db().expect("the checked-in fixture contains the cards");
        let state = serde_json::from_str::<PersistedGameState>(&json)
            .expect("the save decodes")
            .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
            .expect("the open proposal is admissible")
            .finalize_after_rehydration(|state| {
                rehydrate_game_from_card_db(state, db);
                Ok(())
            })
            .expect("the rehydrated proposal is publishable");
        board.runner = GameRunner::from_state(state);
        assert_eq!(
            board.runner.state().waiting_for,
            prompt,
            "trusted {trusted}"
        );
        assert!(lock_is_open(&board.runner), "trusted {trusted}");
        assert!(proposal(&board.runner)
            .activation_phyrexian_intent
            .is_none());

        let waiting_for = submit_phyrexian(&mut board.runner, &[ShardChoice::PayLife]);
        assert!(
            matches!(waiting_for, WaitingFor::TargetSelection { .. }),
            "trusted {trusted}: {waiting_for:?}"
        );
        let waiting_for = choose_targets(&mut board.runner, &[board.chosen]);
        assert!(
            is_mana_payment(&waiting_for),
            "trusted {trusted}: {waiting_for:?}"
        );
        tap_land_for_mana(&mut board.runner, board.mountain);
        pass(&mut board.runner);
        assert_eq!(life(&board.runner, P0), 18, "trusted {trusted}");
    }
}

// ---------------------------------------------------------------------------
// Typed Phyrexian election contracts: the three lock points
// ---------------------------------------------------------------------------
//
// No printed activated ability with a Phyrexian symbol meets two reductions
// whose orders lock different totals (Lashwrithe's "Equip {B/P}{B/P}" has no
// generic to reduce). These TYPED CONTRACTS are parsed definitions, not printed
// cards; their prices are derived from their typed costs. The reductions are
// Training Grounds' printed static ("Activated abilities of creatures you
// control cost {2} less to activate. This effect can't reduce the mana in that
// cost to less than one mana.") and a typed unfloored "{1} less" — or, at
// target settlement, Professor Hojo's printed target-dependent "{2} less".
//
// CR 601.2f + CR 118.13a: announced as life, the Phyrexian symbol is not mana
// in the selected total, so the floor counts only the generic: {2} locks {0}
// when the floored reduction goes first and {1} when the unfloored one does — an
// election. Announced as mana, {2}{W} locks {W} in either order — no election.
// Folding the life symbol as mana (the defect) would price the life route like
// the mana route and offer no election.

const TRAINING_GROUNDS: &str = "Activated abilities of creatures you control cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.";
const UNFLOORED_ONE_LESS: &str =
    "Activated abilities of creatures you control cost {1} less to activate.";
const PROFESSOR_HOJO: &str = "The first activated ability you activate during your turn that targets a creature you control costs {2} less to activate.\nWhenever one or more creatures you control become the target of an activated ability, draw a card. This ability triggers only once each turn.";

/// The mana an election outcome's locked total demands: its mana value less the
/// printed Phyrexian symbols the bound announcement pays with 2 life
/// (CR 118.13a), which stay in the locked total but are not mana.
fn outcome_mana(
    pending: &engine::types::game_state::PendingCast,
    outcome: &engine::types::casting_costs::CostReductionOutcome,
) -> u32 {
    let life_symbols = pending
        .activation_phyrexian_intent
        .as_ref()
        .map_or(0, |intent| {
            intent
                .native_choices
                .iter()
                .filter(|record| record.choice == ShardChoice::PayLife)
                .count() as u32
        });
    outcome.locked_cost.mana_value() - life_symbols
}

/// The distinct locked totals an election prompt offers, by the mana each
/// demands.
fn election_totals_of(waiting_for: &WaitingFor) -> Vec<u32> {
    let WaitingFor::OrderCostReductions {
        outcomes,
        pending_cast,
        ..
    } = waiting_for
    else {
        panic!("expected the cost election, got {waiting_for:?}");
    };
    let mut totals: Vec<u32> = outcomes
        .iter()
        .map(|outcome| outcome_mana(pending_cast, outcome))
        .collect();
    totals.sort_unstable();
    totals
}

/// The lock point the election prompt's carrier is open at.
fn election_lock_point(waiting_for: &WaitingFor) -> Option<ActivationCostLockPoint> {
    let WaitingFor::OrderCostReductions { pending_cast, .. } = waiting_for else {
        return None;
    };
    match pending_cast
        .activation_cost_snapshot
        .as_deref()
        .map(|snapshot| &snapshot.lock)
    {
        Some(ActivationCostLock::Open { point }) => Some(*point),
        _ => None,
    }
}

/// Answers the election with the order that locks a total of `total` mana.
fn elect_locking(runner: &mut GameRunner, total: u32) -> WaitingFor {
    let WaitingFor::OrderCostReductions {
        outcomes,
        pending_cast,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("expected the cost election");
    };
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome_mana(&pending_cast, outcome) == total)
        .unwrap_or_else(|| panic!("no order locks {total}"));
    act(
        runner,
        GameAction::OrderCostReductions {
            order: outcome.order.clone(),
            hybrid_announcement: Vec::new(),
        },
    )
}

/// A raw or trusted save of the current state, restored through the engine's
/// persisted pipeline and rehydrated from the same card database.
fn save_and_restore(runner: &GameRunner, trusted: bool) -> GameRunner {
    let persisted = if trusted {
        PersistedGameState::capture(runner.state().clone())
    } else {
        PersistedGameState::Raw(Box::new(runner.state().clone()))
    };
    let json = serde_json::to_string(&persisted).expect("the state serializes");
    let db = shared_card_db().expect("the checked-in fixture contains the cards");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("the save decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the paused save is admissible")
        .finalize_after_rehydration(|state| {
            rehydrate_game_from_card_db(state, db);
            Ok(())
        })
        .expect("the rehydrated save is publishable");
    GameRunner::from_state(state)
}

/// The announced native choices bound on the activation's root: the election
/// prompt's carrier while the election is asked, the pending root otherwise.
fn native_choices(runner: &GameRunner) -> Vec<ShardChoice> {
    let root = match &runner.state().waiting_for {
        WaitingFor::OrderCostReductions { pending_cast, .. } => pending_cast.as_ref(),
        _ => proposal(runner),
    };
    root.activation_phyrexian_intent
        .as_ref()
        .expect("the announcement is bound")
        .native_choices
        .iter()
        .map(|record| record.choice)
        .collect()
}

struct ElectionBoard {
    runner: GameRunner,
    contract: ObjectId,
    plains: ObjectId,
    mountain: ObjectId,
    own: ObjectId,
}

/// A typed contract creature with `oracle` under Training Grounds and either the
/// typed unfloored reducer or Professor Hojo, with a Plains, a Mountain and a
/// creature of P0's to target.
fn election_contract_board(oracle: &str, hojo: bool) -> ElectionBoard {
    let mut scenario = scenario_at_main();
    for name in ["L1", "L2", "L3"] {
        scenario.add_card_to_library_top(P0, name);
    }
    scenario.add_artifact_from_oracle(P0, "Training Grounds", TRAINING_GROUNDS);
    if hojo {
        scenario.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, PROFESSOR_HOJO);
    } else {
        scenario.add_artifact_from_oracle(P0, "Typed Unfloored Reducer", UNFLOORED_ONE_LESS);
    }
    let contract = scenario
        .add_creature_from_oracle(P0, "Typed Phyrexian Election Contract", 1, 1, oracle)
        .id();
    let own = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let plains = add(&mut scenario, P0, "Plains", Zone::Battlefield);
    let mountain = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
    ElectionBoard {
        runner: build(scenario),
        contract,
        plains,
        mountain,
        own,
    }
}

/// TYPED CONTRACT (Announcement): "{2}{W/P}: You gain 1 life." CR 118.13a +
/// CR 601.2b: the symbol is announced first; CR 601.2f: announced as life, the
/// two orders lock {0} and {1} at the Announcement point, and the elected total
/// is what is paid — {1} through the window (one Mountain) or {0} with no window
/// — plus exactly 2 life. Announced as mana there is no election: {W} only.
/// A raw and a trusted save at the election prompt restore to the same prompt,
/// keep the announcement, and the restored answer pays the elected total once.
#[test]
fn typed_phyrexian_announcement_election_prices_the_life_route_and_survives_restore() {
    const ORACLE: &str = "{2}{W/P}: You gain 1 life.";
    for (elected, restore) in [(1, None), (0, None), (1, Some(false)), (1, Some(true))] {
        let mut board = election_contract_board(ORACLE, false);
        let waiting_for = activate_index(
            &mut board.runner,
            board.contract,
            0,
            CastPaymentMode::Manual,
        );
        assert_announcement(&waiting_for, 1);
        let waiting_for = submit_phyrexian(&mut board.runner, &[ShardChoice::PayLife]);
        assert_eq!(
            election_totals_of(&waiting_for),
            vec![0, 1],
            "the life route's selected {{2}} meets the two floors"
        );
        assert_eq!(
            election_lock_point(&waiting_for),
            Some(ActivationCostLockPoint::Announcement)
        );
        assert_eq!(life(&board.runner, P0), 20, "nothing is paid by electing");
        if let Some(trusted) = restore {
            let prompt = waiting_for.clone();
            board.runner = save_and_restore(&board.runner, trusted);
            assert_eq!(
                board.runner.state().waiting_for,
                prompt,
                "trusted {trusted}: the election prompt is restored"
            );
            assert_eq!(native_choices(&board.runner), vec![ShardChoice::PayLife]);
        }
        let waiting_for = elect_locking(&mut board.runner, elected);
        if elected == 1 {
            assert!(
                is_mana_payment(&waiting_for),
                "{restore:?}: the elected {{1}} opens the window: {waiting_for:?}"
            );
            tap_land_for_mana(&mut board.runner, board.mountain);
            let waiting_for = pass(&mut board.runner);
            assert!(is_priority(&waiting_for), "{waiting_for:?}");
            assert!(tapped(&board.runner, board.mountain));
        } else {
            assert!(
                is_priority(&waiting_for),
                "the elected {{0}} has no window: {waiting_for:?}"
            );
            assert!(!tapped(&board.runner, board.mountain));
        }
        assert!(!tapped(&board.runner, board.plains), "no {{W}} was owed");
        assert_eq!(life(&board.runner, P0), 18, "{restore:?}: exactly 2 life");
        assert_eq!(board.runner.state().stack.len(), 1, "placed once");
        board.runner.advance_until_stack_empty();
        assert_eq!(life(&board.runner, P0), 19, "the ability resolved once");
    }

    // The mana route: {2}{W} locks {W} in either order, so there is no election.
    let mut board = election_contract_board(ORACLE, false);
    activate_index(
        &mut board.runner,
        board.contract,
        0,
        CastPaymentMode::Manual,
    );
    let waiting_for = submit_phyrexian(&mut board.runner, &[ShardChoice::PayMana]);
    assert!(
        is_mana_payment(&waiting_for),
        "no election for the mana route: {waiting_for:?}"
    );
    tap_land_for_mana(&mut board.runner, board.plains);
    let waiting_for = pass(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 20);
    assert!(
        !tapped(&board.runner, board.mountain),
        "the generic was reduced away"
    );
}

/// TYPED CONTRACT (XAnnounced): "{X}{W/P}: You gain X life." CR 601.2b: X is
/// announced, then the symbol; CR 601.2f: with X = 2 announced as life the
/// election is raised at the XAnnounced point with the same {0} / {1} totals,
/// and the elected {1} is paid in the window with exactly 2 life.
#[test]
fn typed_phyrexian_x_election_resumes_at_its_x_lock() {
    let mut board = election_contract_board("{X}{W/P}: You gain X life.", false);
    let waiting_for = activate_index(
        &mut board.runner,
        board.contract,
        0,
        CastPaymentMode::Manual,
    );
    assert!(
        matches!(waiting_for, WaitingFor::ChooseXValue { .. }),
        "CR 601.2b: X first: {waiting_for:?}"
    );
    let waiting_for = act(&mut board.runner, GameAction::ChooseX { value: 2 });
    assert_announcement(&waiting_for, 1);
    let waiting_for = submit_phyrexian(&mut board.runner, &[ShardChoice::PayLife]);
    assert_eq!(election_totals_of(&waiting_for), vec![0, 1]);
    assert_eq!(
        election_lock_point(&waiting_for),
        Some(ActivationCostLockPoint::XAnnounced)
    );
    let waiting_for = elect_locking(&mut board.runner, 1);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    assert_eq!(native_choices(&board.runner), vec![ShardChoice::PayLife]);
    tap_land_for_mana(&mut board.runner, board.mountain);
    let waiting_for = pass(&mut board.runner);
    assert!(is_priority(&waiting_for), "{waiting_for:?}");
    assert_eq!(life(&board.runner, P0), 18, "exactly 2 life");
    assert!(!tapped(&board.runner, board.plains));
    board.runner.advance_until_stack_empty();
    assert_eq!(life(&board.runner, P0), 20, "X = 2 life gained");
}

/// TYPED CONTRACT (TargetSettlement): "{3}{W/P}: Tap target creature." under
/// Training Grounds and Professor Hojo, targeting P0's own creature. CR 601.2c +
/// CR 601.2f: the symbol is announced before the target; announced as life, the
/// cost depends on the target, so the election is raised at target settlement
/// with totals {0} (Grounds first) and {1} (Hojo first); the elected {1} is
/// paid in the window with exactly 2 life and the target is tapped.
#[test]
fn typed_phyrexian_settlement_election_resumes_after_its_target() {
    let mut board = election_contract_board("{3}{W/P}: Tap target creature.", true);
    let waiting_for = activate_index(
        &mut board.runner,
        board.contract,
        0,
        CastPaymentMode::Manual,
    );
    assert_announcement(&waiting_for, 1);
    let waiting_for = submit_phyrexian(&mut board.runner, &[ShardChoice::PayLife]);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "CR 601.2c: the target follows the announcement: {waiting_for:?}"
    );
    let waiting_for = choose_targets(&mut board.runner, &[board.own]);
    assert_eq!(election_totals_of(&waiting_for), vec![0, 1]);
    assert_eq!(
        election_lock_point(&waiting_for),
        Some(ActivationCostLockPoint::TargetSettlement)
    );
    let waiting_for = elect_locking(&mut board.runner, 1);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
    tap_land_for_mana(&mut board.runner, board.mountain);
    pass(&mut board.runner);
    assert_eq!(life(&board.runner, P0), 18, "exactly 2 life");
    assert!(!tapped(&board.runner, board.plains));
    assert_eq!(
        activation_targets(&board.runner, board.contract),
        vec![TargetRef::Object(board.own)]
    );
    board.runner.advance_until_stack_empty();
    assert!(tapped(&board.runner, board.own), "the target was tapped");
}

// ---------------------------------------------------------------------------
// K'rrik's permission is read when the {B} is paid (CR 601.2h)
// ---------------------------------------------------------------------------
//
// Phyrexian Reclamation ("{1}{B}, Pay 2 life: Return target creature card from
// your graveyard to your hand.") under Zirda locks {B} plus the printed 2 life.
// A payment permission ("For each {B} in a cost, you may pay 2 life rather than
// pay that mana.") changes how that {B} is paid, not the cost, so it is the
// permission the player has WHEN PAYING that counts: one gained or lost in the
// window counts, and so does who controls the permanent granting it.

const LIFE_FOR_BLACK_GRANT: &str =
    "For each {B} in a cost, you may pay 2 life rather than pay that mana.";

/// The typed "for each {B} in a cost, you may pay 2 life" grant, optionally
/// conditioned on its source being tapped.
fn life_for_black_grant(condition: Option<StaticCondition>) -> StaticDefinition {
    let grant = StaticDefinition::new(StaticMode::PayLifeAsColoredMana {
        color: ManaColor::Black,
    })
    .affected(TargetFilter::Controller)
    .description(LIFE_FOR_BLACK_GRANT.to_string());
    match condition {
        Some(condition) => grant.condition(condition),
        None => grant,
    }
}

struct ReclamationBoard {
    scenario: GameScenario,
    reclamation: ObjectId,
    swamp: ObjectId,
    returned: ObjectId,
}

fn reclamation_board() -> ReclamationBoard {
    let mut scenario = scenario_at_main();
    let reclamation = add(
        &mut scenario,
        P0,
        "Phyrexian Reclamation",
        Zone::Battlefield,
    );
    add(&mut scenario, P0, "Zirda, the Dawnwaker", Zone::Battlefield);
    let swamp = add(&mut scenario, P0, "Swamp", Zone::Battlefield);
    let returned = add(&mut scenario, P0, "Grizzly Bears", Zone::Graveyard);
    // A second creature card makes the target a real choice: the engine selects
    // a lone legal target without asking.
    add(&mut scenario, P0, "Grizzly Bears", Zone::Graveyard);
    ReclamationBoard {
        scenario,
        reclamation,
        swamp,
        returned,
    }
}

/// Activates Reclamation manually, targets `returned` and asserts the {B}
/// window, with nothing paid.
fn reclamation_window(runner: &mut GameRunner, reclamation: ObjectId, returned: ObjectId) {
    let waiting_for = activate(runner, reclamation, CastPaymentMode::Manual);
    assert!(
        matches!(waiting_for, WaitingFor::TargetSelection { .. }),
        "{waiting_for:?}"
    );
    let waiting_for = choose_targets(runner, &[returned]);
    assert!(
        is_mana_payment(&waiting_for),
        "the locked {{B}} opens the window: {waiting_for:?}"
    );
    assert_eq!(life(runner, P0), 20, "nothing is paid at the window");
}

/// Sacrifices `creature` to Ashnod's Altar inside the window.
fn sacrifice_to_altar(runner: &mut GameRunner, altar: ObjectId, creature: ObjectId) {
    let waiting_for = activate_mana_ability(runner, altar);
    assert!(
        matches!(
            waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "{waiting_for:?}"
    );
    let waiting_for = select_cards(runner, &[creature]);
    assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
}

/// Pays the window and asserts the outcome: with a permission the {B} is offered
/// as 2 life (four life in all, Swamp untapped); without one the Swamp's {B}
/// pays it (two life in all).
fn finish_reclamation(
    runner: &mut GameRunner,
    swamp: ObjectId,
    returned: ObjectId,
    permission: bool,
    what: &str,
) {
    if !permission {
        tap_land_for_mana(runner, swamp);
    }
    let waiting_for = pass(runner);
    if permission {
        let WaitingFor::PhyrexianPayment { shards, .. } = &waiting_for else {
            panic!("{what}: the permission is offered at payment: {waiting_for:?}");
        };
        assert_eq!(shards.len(), 1, "{what}: one {{B}}: {shards:?}");
        let waiting_for = submit_phyrexian(runner, &[ShardChoice::PayLife]);
        assert!(is_priority(&waiting_for), "{what}: {waiting_for:?}");
        assert_eq!(life(runner, P0), 16, "{what}: the {{B}} cost 2 life");
        assert!(!tapped(runner, swamp), "{what}: no black mana was spent");
    } else {
        assert!(
            is_priority(&waiting_for),
            "{what}: no permission, no life option: {waiting_for:?}"
        );
        assert_eq!(life(runner, P0), 18, "{what}: only the printed 2 life");
    }
    assert_eq!(runner.state().stack.len(), 1, "{what}: placed once");
    runner.advance_until_stack_empty();
    assert!(
        in_zone(runner, returned, Zone::Hand),
        "{what}: the chosen card returned"
    );
}

/// CR 601.2h: K'rrik sacrificed to Ashnod's Altar in the window takes its
/// permission with it — the {B} is paid with the Swamp — while keeping K'rrik
/// offers the life. The lock ({B} plus 2 life) is the same either way.
#[test]
fn manual_krrik_permission_lost_in_the_window_is_not_offered_at_payment() {
    for sacrifice_krrik in [false, true] {
        let ReclamationBoard {
            mut scenario,
            reclamation,
            swamp,
            returned,
        } = reclamation_board();
        let krrik = add(
            &mut scenario,
            P0,
            "K'rrik, Son of Yawgmoth",
            Zone::Battlefield,
        );
        let altar = add(&mut scenario, P0, "Ashnod's Altar", Zone::Battlefield);
        let mut runner = build(scenario);
        reclamation_window(&mut runner, reclamation, returned);
        if sacrifice_krrik {
            sacrifice_to_altar(&mut runner, altar, krrik);
            assert!(in_zone(&runner, krrik, Zone::Graveyard));
        }
        finish_reclamation(
            &mut runner,
            swamp,
            returned,
            !sacrifice_krrik,
            &format!("sacrificed {sacrifice_krrik}"),
        );
    }
}

/// Two permanents grant the permission — K'rrik and a TYPED grant creature with
/// K'rrik's printed sentence. Losing one in the window leaves the other's
/// permission; losing both leaves none.
#[test]
fn manual_krrik_permission_survives_losing_one_of_two_grants() {
    for lose_both in [false, true] {
        let ReclamationBoard {
            mut scenario,
            reclamation,
            swamp,
            returned,
        } = reclamation_board();
        let krrik = add(
            &mut scenario,
            P0,
            "K'rrik, Son of Yawgmoth",
            Zone::Battlefield,
        );
        let typed_grant = scenario
            .add_creature(P0, "Typed Life Grant Contract", 1, 1)
            .with_static_definition(life_for_black_grant(None))
            .id();
        let altar = add(&mut scenario, P0, "Ashnod's Altar", Zone::Battlefield);
        let mut runner = build(scenario);
        reclamation_window(&mut runner, reclamation, returned);
        sacrifice_to_altar(&mut runner, altar, krrik);
        if lose_both {
            sacrifice_to_altar(&mut runner, altar, typed_grant);
        }
        finish_reclamation(
            &mut runner,
            swamp,
            returned,
            !lose_both,
            &format!("lose both {lose_both}"),
        );
    }
}

/// CR 601.2h: a permission gained in the window counts. A TYPED artifact
/// ("{T}: Add {C}." with the grant "as long as it is tapped") grants nothing
/// until the player taps it for mana in the window; then the {B} is offered as
/// 2 life. Left untapped, the Swamp pays the {B}.
#[test]
fn manual_krrik_permission_gained_in_the_window_is_offered_at_payment() {
    for tap_grant in [false, true] {
        let ReclamationBoard {
            mut scenario,
            reclamation,
            swamp,
            returned,
        } = reclamation_board();
        let grant = scenario
            .add_artifact_from_oracle(P0, "Typed Tapped Grant Contract", "{T}: Add {C}.")
            .with_static_definition(life_for_black_grant(Some(StaticCondition::SourceIsTapped)))
            .id();
        let mut runner = build(scenario);
        assert!(
            runner.state().objects[&grant]
                .static_definitions
                .iter_unchecked()
                .any(
                    |def| matches!(def.mode, StaticMode::PayLifeAsColoredMana { .. })
                        && def.condition.is_some()
                ),
            "reach guard: the conditional grant is on the artifact"
        );
        reclamation_window(&mut runner, reclamation, returned);
        if tap_grant {
            activate_mana_ability(&mut runner, grant);
            assert!(tapped(&runner, grant));
        }
        finish_reclamation(
            &mut runner,
            swamp,
            returned,
            tap_grant,
            &format!("tapped {tap_grant}"),
        );
    }
}

/// CR 601.2h + CR 110.2: the permission belongs to the player who controls its
/// source, not to its owner. K'rrik owned by P1 but controlled by P0 lets P0 pay
/// the {B} with life; K'rrik owned by P0 but controlled by P1 does not.
#[test]
fn manual_krrik_permission_follows_its_controller_not_its_owner() {
    for (owner, controller) in [(P1, P0), (P0, P1)] {
        let ReclamationBoard {
            mut scenario,
            reclamation,
            swamp,
            returned,
        } = reclamation_board();
        let krrik = add(
            &mut scenario,
            owner,
            "K'rrik, Son of Yawgmoth",
            Zone::Battlefield,
        );
        let mut runner = build(scenario);
        {
            let object = runner.state_mut().objects.get_mut(&krrik).expect("K'rrik");
            object.base_controller = Some(controller);
            object.controller = controller;
        }
        assert_eq!(runner.state().objects[&krrik].owner, owner, "setup");
        reclamation_window(&mut runner, reclamation, returned);
        finish_reclamation(
            &mut runner,
            swamp,
            returned,
            controller == P0,
            &format!("owner {owner:?}, controller {controller:?}"),
        );
    }
}

// ---------------------------------------------------------------------------
// Restore at a replacement-paused residual, and with owner != controller
// ---------------------------------------------------------------------------

/// Birthing Pod announced as life, its {1} paid in the window, then its
/// sacrifice of Grizzly Bears paused at a replacement choice (two real Rest in
/// Peace each exile the card instead). A raw and a trusted save at that pause
/// restore to the same choice with the announcement, the payment mode and the
/// paid {1} intact; answering it completes the activation once — 2 life, the
/// Wastes once, the Forest never — and the search reads the sacrificed Bears.
#[test]
fn manual_birthing_pod_replacement_paused_sacrifice_survives_raw_and_trusted_restore() {
    for trusted in [false, true] {
        let mut scenario = scenario_at_main();
        let pod = add(&mut scenario, P0, "Birthing Pod", Zone::Battlefield);
        let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
        let forest = add(&mut scenario, P0, "Forest", Zone::Battlefield);
        let wastes = add(&mut scenario, P0, "Wastes", Zone::Battlefield);
        let courser = add(&mut scenario, P0, "Centaur Courser", Zone::Library);
        add(&mut scenario, P0, "Hill Giant", Zone::Library);
        add(&mut scenario, P0, "Rest in Peace", Zone::Battlefield);
        add(&mut scenario, P0, "Rest in Peace", Zone::Battlefield);
        let mut runner = build(scenario);

        activate(&mut runner, pod, CastPaymentMode::Manual);
        let waiting_for = submit_phyrexian(&mut runner, &[ShardChoice::PayLife]);
        assert!(is_mana_payment(&waiting_for), "{waiting_for:?}");
        tap_land_for_mana(&mut runner, wastes);
        let waiting_for = pass(&mut runner);
        assert!(
            matches!(
                waiting_for,
                WaitingFor::PayCost {
                    kind: PayCostKind::Sacrifice,
                    ..
                }
            ),
            "the sacrifice follows the window: {waiting_for:?}"
        );
        let paused = select_cards(&mut runner, &[bears]);
        assert!(
            matches!(paused, WaitingFor::ReplacementChoice { .. }),
            "reach guard: the sacrifice is paused at the replacement: {paused:?}"
        );
        assert!(runner.state().stack.is_empty(), "nothing is placed yet");
        let life_at_pause = life(&runner, P0);

        runner = save_and_restore(&runner, trusted);
        assert_eq!(
            runner.state().waiting_for,
            paused,
            "trusted {trusted}: the replacement choice is restored"
        );
        assert_eq!(life(&runner, P0), life_at_pause, "trusted {trusted}");

        let waiting_for = act(&mut runner, GameAction::ChooseReplacement { index: 0 });
        assert!(
            is_priority(&waiting_for),
            "trusted {trusted}: {waiting_for:?}"
        );
        assert_eq!(life(&runner, P0), 18, "trusted {trusted}: 2 life, once");
        assert!(tapped(&runner, wastes) && !tapped(&runner, forest));
        assert!(
            tapped(&runner, pod),
            "trusted {trusted}: {{T}} was paid once"
        );
        assert!(
            in_zone(&runner, bears, Zone::Exile),
            "trusted {trusted}: the replacement exiled the sacrificed card"
        );
        assert_eq!(
            runner.state().stack.len(),
            1,
            "trusted {trusted}: placed once"
        );
        for _ in 0..8 {
            match runner.state().waiting_for.clone() {
                WaitingFor::SearchChoice { cards, .. } => {
                    assert_eq!(
                        cards,
                        vec![courser],
                        "mana value 3 = 1 + the sacrificed Grizzly Bears' 2"
                    );
                    select_cards(&mut runner, &[courser]);
                }
                _ if runner.state().stack.is_empty() => break,
                _ => {
                    pass(&mut runner);
                }
            }
        }
        assert!(
            in_zone(&runner, courser, Zone::Battlefield),
            "trusted {trusted}: the search resolved"
        );
    }
}

/// Formidable Speaker owned by P1 and controlled by P0, beside P0's own
/// Speaker. P0 activates the borrowed one manually; a raw and a trusted save at
/// the window keep the activating controller and the source: the restored game
/// pays the {1}, taps the borrowed Speaker (not P0's own) and untaps the target,
/// and the ability on the stack is P0's.
#[test]
fn manual_formidable_speaker_restore_keeps_the_controller_when_it_is_not_the_owner() {
    for trusted in [false, true] {
        let mut scenario = scenario_at_main();
        let borrowed = add(&mut scenario, P1, "Formidable Speaker", Zone::Battlefield);
        let own_speaker = add(&mut scenario, P0, "Formidable Speaker", Zone::Battlefield);
        let bears = add(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
        let mountain = add(&mut scenario, P0, "Mountain", Zone::Battlefield);
        let mut runner = build(scenario);
        {
            let object = runner
                .state_mut()
                .objects
                .get_mut(&borrowed)
                .expect("the borrowed Speaker");
            object.base_controller = Some(P0);
            object.controller = P0;
        }
        runner
            .state_mut()
            .objects
            .get_mut(&bears)
            .expect("the target")
            .tapped = true;

        let index = ability_index_of(&runner, borrowed, "Untap another target permanent");
        let waiting_for = activate_index(&mut runner, borrowed, index, CastPaymentMode::Manual);
        assert!(
            matches!(waiting_for, WaitingFor::TargetSelection { .. }),
            "{waiting_for:?}"
        );
        let window = choose_targets(&mut runner, &[bears]);
        assert!(is_mana_payment(&window), "{window:?}");

        runner = save_and_restore(&runner, trusted);
        assert_eq!(runner.state().waiting_for, window, "trusted {trusted}");
        assert_eq!(runner.state().objects[&borrowed].owner, P1);
        assert_eq!(runner.state().objects[&borrowed].controller, P0);
        assert_eq!(proposal(&runner).object_id, borrowed, "the selected source");

        tap_land_for_mana(&mut runner, mountain);
        let waiting_for = pass(&mut runner);
        assert!(
            is_priority(&waiting_for),
            "trusted {trusted}: {waiting_for:?}"
        );
        assert!(tapped(&runner, borrowed), "the borrowed Speaker paid {{T}}");
        assert!(
            !tapped(&runner, own_speaker),
            "P0's own Speaker was not used"
        );
        let controllers: Vec<PlayerId> = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == borrowed)
            .map(|entry| entry.controller)
            .collect();
        assert_eq!(controllers, vec![P0], "trusted {trusted}: P0's ability");
        runner.advance_until_stack_empty();
        assert!(
            !tapped(&runner, bears),
            "trusted {trusted}: the target untapped"
        );
    }
}
