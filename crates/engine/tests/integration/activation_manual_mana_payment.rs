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
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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
