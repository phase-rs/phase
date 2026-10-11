//! A mana ability's own mana cost that neither the pool nor auto-tap covers opens a payment
//! window (CR 605.3a + CR 117.1d + CR 601.2g via CR 602.2b), driven through `apply()` on
//! Grand Architect + Pili-Pala.
use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::mana_abilities::is_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::{play_trace_view, EntryKind};
use engine::types::ability::{AbilityCost, AbilityKind};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    GameState, LoopDetectionMode, ManaAbilityResume, ManaChoice, PayCostKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn ability(state: &GameState, id: ObjectId, mana: bool) -> usize {
    state.objects[&id]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated && is_mana_ability(a) == mana)
        .expect("the ability")
}

fn act(runner: &mut GameRunner, action: GameAction) {
    let shown = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{shown} rejected: {error:?}"));
}

fn activate(runner: &mut GameRunner, source: ObjectId, mana: bool) {
    let ability_index = ability(runner.state(), source, mana);
    act(
        runner,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        },
    );
}

fn pool_total(state: &GameState) -> usize {
    state.players[0].mana_pool.total()
}

struct Board {
    runner: GameRunner,
    architect: ObjectId,
    pili: ObjectId,
}

/// Grand Architect: "{U}: Target artifact creature becomes blue until end of turn." and "Tap an
/// untapped blue creature you control: Add {C}{C}. Spend this mana only to cast artifact spells
/// or activate abilities of artifacts." Pili-Pala: "{2}, {Q}: Add one mana of any color."
/// With `blue`, the Island pays Grand Architect's {U} to make Pili-Pala blue, leaving the pool
/// empty and Pili-Pala untapped.
fn board(blue: bool) -> Option<Board> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let architect = scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
    let pili = scenario.add_real_card(P0, "Pili-Pala", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let mut runner = scenario.build();
    if blue {
        activate(&mut runner, architect, false);
        while !runner.state().stack.is_empty() {
            act(&mut runner, GameAction::PassPriority);
        }
    }
    assert_eq!(
        pool_total(runner.state()),
        0,
        "reach: the pool starts empty"
    );
    assert!(
        !runner.state().objects[&pili].tapped,
        "reach: Pili-Pala untapped"
    );
    Some(Board {
        runner,
        architect,
        pili,
    })
}

fn open_window(board: &mut Board) {
    activate(&mut board.runner, board.pili, true);
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ManaAbilityManaPayment { .. }
        ),
        "Pili-Pala's {{2}} opens its payment window: {:?}",
        board.runner.state().waiting_for
    );
}

fn architect_activation(board: &Board) -> GameAction {
    GameAction::ActivateAbility {
        source_id: board.architect,
        ability_index: ability(board.runner.state(), board.architect, true),
    }
}

#[test]
fn grand_architect_pays_pili_pala_inside_its_payment_window() {
    let Some(mut board) = board(true) else { return };
    open_window(&mut board);
    let architect = architect_activation(&board);
    act(&mut board.runner, architect);
    // Grand Architect's cost taps Pili-Pala itself; the {Q} then untaps it (CR 601.2h).
    act(
        &mut board.runner,
        GameAction::SelectCards {
            cards: vec![board.pili],
        },
    );
    act(
        &mut board.runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        },
    );
    let state = board.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
    assert_eq!(
        pool_total(state),
        1,
        "the {{C}}{{C}} paid Pili-Pala's {{2}}"
    );
    assert!(
        !state.objects[&board.pili].tapped,
        "{{Q}} untapped Pili-Pala"
    );
}

#[test]
fn the_ai_drive_pays_pili_pala_from_its_window_candidates() {
    let Some(mut board) = board(true) else { return };
    open_window(&mut board);
    let architect = architect_activation(&board);
    assert!(
        legal_actions(board.runner.state()).contains(&architect),
        "the window offers Grand Architect's mana ability"
    );
    act(&mut board.runner, architect);
    let tap_pili = GameAction::SelectCards {
        cards: vec![board.pili],
    };
    for _ in 0..8 {
        if matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ) {
            break;
        }
        let legal = legal_actions(board.runner.state());
        let action = if matches!(board.runner.state().waiting_for, WaitingFor::PayCost { .. }) {
            assert!(
                legal.contains(&tap_pili),
                "the answer that taps Pili-Pala is offered"
            );
            tap_pili.clone()
        } else {
            legal
                .into_iter()
                .find(|action| !matches!(action, GameAction::CancelCast))
                .expect("an answer")
        };
        act(&mut board.runner, action);
    }
    let state = board.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(pool_total(state), 1, "Pili-Pala's mana was added");
}

#[test]
fn cancelling_the_window_withdraws_pili_palas_activation() {
    let Some(mut board) = board(true) else { return };
    open_window(&mut board);
    assert!(legal_actions(board.runner.state()).contains(&GameAction::CancelCast));
    act(&mut board.runner, GameAction::CancelCast);
    let state = board.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(pool_total(state), 0);
    assert!(!state.objects[&board.pili].tapped);
    assert!(state.stack.is_empty());
}

#[test]
fn no_window_opens_without_a_mana_ability_that_could_pay() {
    // Pili-Pala is not blue and Grand Architect is tapped, so no untapped blue creature can pay
    // Grand Architect's cost; the Island alone cannot pay {2}.
    let Some(mut board) = board(false) else {
        return;
    };
    let (architect, pili) = (board.architect, board.pili);
    board
        .runner
        .state_mut()
        .objects
        .get_mut(&architect)
        .unwrap()
        .tapped = true;
    let ability_index = ability(board.runner.state(), pili, true);
    let refused = board.runner.act(GameAction::ActivateAbility {
        source_id: pili,
        ability_index,
    });
    assert!(
        refused.is_err(),
        "no window: {:?}",
        board.runner.state().waiting_for
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    // Reach guard: with Grand Architect untapped it can tap itself, so the window opens.
    let Some(mut untapped) = self::board(false) else {
        return;
    };
    open_window(&mut untapped);
}

struct AltarBoard {
    runner: GameRunner,
    altar: ObjectId,
    bears: ObjectId,
    pili: ObjectId,
    island: ObjectId,
}

/// Phyrexian Altar: "Sacrifice a creature: Add one mana of any color." Pili-Pala is tapped and
/// the pool empty, so the Island alone cannot pay its {2} and the window opens.
fn altar_board() -> Option<AltarBoard> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let pili = scenario.add_real_card(P0, "Pili-Pala", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&pili).unwrap().tapped = true;
    activate(&mut runner, pili, true);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaAbilityManaPayment { .. }
    ));
    Some(AltarBoard {
        runner,
        altar,
        bears,
        pili,
        island,
    })
}

fn tap_island(board: &mut AltarBoard) {
    let action = legal_actions_full(board.runner.state())
        .2
        .get(&board.island)
        .and_then(|actions| {
            actions
                .iter()
                .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
                .cloned()
        })
        .expect("the window offers the Island");
    act(&mut board.runner, action);
}

fn sacrifice_bears_for_red(board: &mut AltarBoard) {
    activate(&mut board.runner, board.altar, true);
    act(
        &mut board.runner,
        GameAction::SelectCards {
            cards: vec![board.bears],
        },
    );
    act(
        &mut board.runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        },
    );
}

fn assert_pili_pala_paid(board: &mut AltarBoard) {
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "the {{2}} is paid and Pili-Pala asks its color: {:?}",
        board.runner.state().waiting_for
    );
    act(
        &mut board.runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        },
    );
    let state = board.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    assert_eq!(pool_total(state), 1);
    assert!(!state.objects[&board.pili].tapped);
    assert_eq!(state.objects[&board.bears].zone, Zone::Graveyard);
}

#[test]
fn a_sacrifice_mana_ability_completes_the_window_after_a_land() {
    let Some(mut board) = altar_board() else {
        return;
    };
    tap_island(&mut board);
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::ManaAbilityManaPayment { .. }
        ),
        "one mana of the {{2}}: the window reopens"
    );
    assert_eq!(pool_total(board.runner.state()), 1);
    sacrifice_bears_for_red(&mut board);
    assert_pili_pala_paid(&mut board);
}

#[test]
fn after_a_sacrifice_mana_ability_auto_tap_pays_the_rest() {
    let Some(mut board) = altar_board() else {
        return;
    };
    sacrifice_bears_for_red(&mut board);
    // CR 601.2g: the Island now covers the rest, so the re-entered activation auto-taps it.
    assert!(board.runner.state().objects[&board.island].tapped);
    assert_pili_pala_paid(&mut board);
}

/// What a human seat is shown: the interaction authority bound, the state filtered for the
/// viewer, and the projection derived from that copy.
fn viewer_projection(
    state: &GameState,
) -> (GameState, engine::types::interaction::ViewerInteraction) {
    use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
    use engine::types::interaction::InteractionSessionId;
    let mut bound = state.clone();
    bind_interaction_authority(&mut bound, InteractionSessionId("viewer".to_string()))
        .expect("the interaction authority binds");
    let filtered = engine::game::visibility::filter_state_for_viewer(&bound, P0);
    let view = derive_viewer_interaction(&bound, &filtered, P0);
    (filtered, view)
}

fn offers(view: &engine::types::interaction::ViewerInteraction, action: &GameAction) -> bool {
    use engine::types::interaction::{
        InteractionOpportunityResponse, InteractionPresentationSurface,
    };
    let wanted = engine::game::interaction::interaction_action_id(action);
    view.opportunities
        .iter()
        .flat_map(|opportunity| match &opportunity.response {
            InteractionOpportunityResponse::ExactChoices { choices } => choices.iter(),
            InteractionOpportunityResponse::Schema { candidates, .. } => candidates.iter(),
        })
        .flat_map(|choice| choice.surfaces.iter())
        .any(|surface| {
            matches!(
                surface,
                InteractionPresentationSurface::Action { action_id: Some(id), .. } if *id == wanted
            )
        })
}

/// The mana ability of `id` whose cost is mana alone.
fn mana_costed(state: &GameState, id: ObjectId) -> GameAction {
    use engine::types::ability::AbilityCost;
    let ability_index = state.objects[&id]
        .abilities
        .iter()
        .position(|a| is_mana_ability(a) && matches!(a.cost, Some(AbilityCost::Mana { .. })))
        .expect("a mana ability with a mana cost");
    GameAction::ActivateAbility {
        source_id: id,
        ability_index,
    }
}

/// The floating pip is journaled and verified in the game's own state, and the viewer's copy
/// holds neither the producer record nor the verification.
fn assert_the_viewers_copy_drops_the_pools_verification(state: &GameState, filtered: &GameState) {
    let pips: Vec<_> = state.players[0]
        .mana_pool
        .units()
        .map(|unit| unit.pip_id)
        .collect();
    assert!(!pips.is_empty(), "reach: mana floats");
    assert!(
        pips.iter()
            .all(|pip| state.resolved_rules_journal.has_produced_pip(*pip)),
        "reach: the game's journal produced the floating mana"
    );
    assert!(
        state.players[0].mana_pool.is_verified(),
        "the game's own pool stays verified"
    );
    assert!(
        pips.iter()
            .all(|pip| !filtered.resolved_rules_journal.has_produced_pip(*pip)),
        "reach: the viewer's copy carries no producer record"
    );
    assert!(
        !filtered.players[0].mana_pool.is_verified(),
        "a copy without the journal does not claim its pips are journaled"
    );
}

/// Gruul Signet: "{1}, {T}: Add {R}{G}." While Grizzly Bears is being paid for with a Forest's
/// {G} floating, the Signet's {1} is payable from the pool (CR 605.3a).
#[test]
fn a_viewer_is_offered_a_costed_mana_ability_while_mana_floats_in_a_spell_payment() {
    use engine::types::game_state::CastPaymentMode;
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let signet = scenario.add_real_card(P0, "Gruul Signet", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner = scenario.build();
    let signet_ability = GameAction::ActivateAbility {
        source_id: signet,
        ability_index: ability(runner.state(), signet, true),
    };

    // With the same {G} floating and no payment in progress the Signet is offered.
    let mut at_priority = GameRunner::from_state(runner.state().clone());
    activate(&mut at_priority, forest, true);
    assert_eq!(pool_total(at_priority.state()), 1, "reach: {{G}} floats");
    assert!(offers(
        &viewer_projection(at_priority.state()).1,
        &signet_ability
    ));

    let card_id = runner.state().objects[&bears].card_id;
    act(
        &mut runner,
        GameAction::CastSpell {
            object_id: bears,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        },
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    // With the pool empty the Forest can pay the Signet's {1}.
    assert!(offers(
        &viewer_projection(runner.state()).1,
        &signet_ability
    ));

    activate(&mut runner, forest, true);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    let (filtered, view) = viewer_projection(runner.state());
    assert!(offers(&view, &signet_ability));
    assert_the_viewers_copy_drops_the_pools_verification(runner.state(), &filtered);
}

/// Skyshroud Elf: "{T}: Add {G}." and "{1}: Add {R} or {W}." In Pili-Pala's payment window with
/// the Island's {U} floating, the tapped Elf's {1} is payable from the pool (CR 605.3a).
#[test]
fn a_viewer_is_offered_a_costed_mana_ability_while_mana_floats_in_a_mana_abilitys_payment() {
    let Some(db) = shared_card_db() else { return };
    for with_elf in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
        let pili = scenario.add_real_card(P0, "Pili-Pala", Zone::Battlefield, db);
        let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
        let elf =
            with_elf.then(|| scenario.add_real_card(P0, "Skyshroud Elf", Zone::Battlefield, db));
        let mut runner = scenario.build();
        if let Some(elf) = elf {
            runner.state_mut().objects.get_mut(&elf).unwrap().tapped = true;
        }
        activate(&mut runner, pili, true);
        activate(&mut runner, island, true);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ManaAbilityManaPayment { .. }
            ),
            "reach: Pili-Pala's window stands: {:?}",
            runner.state().waiting_for
        );
        assert_eq!(pool_total(runner.state()), 1, "reach: {{U}} floats");
        let (filtered, view) = viewer_projection(runner.state());
        assert!(offers(&view, &GameAction::CancelCast));
        // Without the Elf no mana ability's cost is payable from the pool.
        let Some(elf) = elf else { continue };
        assert!(offers(&view, &mana_costed(runner.state(), elf)));
        assert_the_viewers_copy_drops_the_pools_verification(runner.state(), &filtered);
    }
}

/// The index of `id`'s mana ability whose cost `pick` accepts.
pub(crate) fn mana_ability_costing(
    state: &GameState,
    id: ObjectId,
    pick: impl Fn(&AbilityCost) -> bool,
) -> usize {
    state.objects[&id]
        .abilities
        .iter()
        .position(|a| is_mana_ability(a) && a.cost.as_ref().is_some_and(&pick))
        .expect("a mana ability with that cost")
}

/// The mana ability of `id` whose cost is {T} alone.
fn tap_costed(state: &GameState, id: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: id,
        ability_index: mana_ability_costing(state, id, |cost| *cost == AbilityCost::Tap),
    }
}

/// The mana ability of `id` whose cost has several parts ("{1}, {T}", "{1}, Remove X counters").
fn composite_costed(state: &GameState, id: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: id,
        ability_index: mana_ability_costing(state, id, |cost| {
            matches!(cost, AbilityCost::Composite { .. })
        }),
    }
}

/// The activations suspended at the standing payment window, innermost first.
fn suspended(state: &GameState) -> Vec<GameAction> {
    let WaitingFor::ManaAbilityManaPayment {
        pending_mana_ability,
        ..
    } = &state.waiting_for
    else {
        return Vec::new();
    };
    std::iter::successors(Some(pending_mana_ability), |pending| {
        match &pending.resume {
            ManaAbilityResume::ManaAbilityManaPayment {
                pending_mana_ability,
            } => Some(pending_mana_ability),
            _ => None,
        }
    })
    .map(|pending| GameAction::ActivateAbility {
        source_id: pending.source_id,
        ability_index: pending.ability_index.expect("an enumerated ability"),
    })
    .collect()
}

/// Submits `action` and requires the engine to refuse it and leave the game as it stood.
fn assert_refused(runner: &mut GameRunner, action: &GameAction) {
    let before = runner.state().clone();
    assert!(
        runner.act(action.clone()).is_err(),
        "{action:?} is refused: {:?}",
        suspended(runner.state())
    );
    assert!(
        *runner.state() == before,
        "a refused {action:?} changes nothing"
    );
}

/// Grand Architect taps itself for {C}{C}, spendable only on artifacts.
fn tap_architect(runner: &mut GameRunner, architect: ObjectId) {
    activate(runner, architect, true);
    act(
        runner,
        GameAction::SelectCards {
            cards: vec![architect],
        },
    );
}

/// The tap the engine authors for `land` at this prompt, as a human seat is offered it.
fn land_tap(state: &GameState, land: ObjectId) -> Option<GameAction> {
    legal_actions_full(state)
        .2
        .get(&land)
        .into_iter()
        .flatten()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .cloned()
}

/// Whether the display sweep reports a mana ability of `id` as available.
fn swept_ready(state: &GameState, id: ObjectId) -> bool {
    let mut shown = state.clone();
    engine::game::public_state::mark_mana_display_dirty(&mut shown);
    engine::game::derived::derive_display_state(&mut shown);
    shown.objects[&id].has_mana_ability
}

/// Prismite: "{2}: Add one mana of any color." (CR 605.3c)
#[test]
fn a_suspended_mana_ability_cannot_be_activated_again() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let prismite = scenario.add_real_card(P0, "Prismite", Zone::Battlefield, db);
    let architect = scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let begun = mana_costed(runner.state(), prismite);
    act(&mut runner, begun.clone());
    assert_eq!(
        suspended(runner.state()),
        std::slice::from_ref(&begun),
        "reach: Prismite's {{2}} opens its payment window"
    );

    assert_refused(&mut runner, &begun);
    assert!(!legal_actions(runner.state()).contains(&begun));

    tap_architect(&mut runner, architect);
    assert!(
        runner.state().objects[&architect].tapped
            && matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseManaColor { .. }
            ),
        "another source pays the window and Prismite asks its color: {:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        },
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(pool_total(runner.state()), 1);
}

/// Skyshroud Elf: "{T}: Add {G}." and "{1}: Add {R} or {W}." CR 605.3c names the ability, so the
/// Elf's other ability and Phyrexian Altar ("Sacrifice a creature: Add one mana of any color.")
/// stay legal while its {1} ability is suspended.
#[test]
fn a_suspended_mana_abilitys_other_ability_and_other_sources_still_pay() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elf = scenario.add_real_card(P0, "Skyshroud Elf", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let begun = mana_costed(runner.state(), elf);
    act(&mut runner, begun.clone());
    assert_eq!(
        suspended(runner.state()),
        std::slice::from_ref(&begun),
        "reach: the Elf's {{1}} opens its payment window"
    );

    assert_refused(&mut runner, &begun);

    let mut other_ability = GameRunner::from_state(runner.state().clone());
    let tap = tap_costed(other_ability.state(), elf);
    act(&mut other_ability, tap);
    assert!(
        other_ability.state().objects[&elf].tapped
            && matches!(
                other_ability.state().waiting_for,
                WaitingFor::ChooseManaColor { .. }
            ),
        "the Elf's own {{G}} pays its {{1}}: {:?}",
        other_ability.state().waiting_for
    );

    activate(&mut runner, altar, true);
    act(&mut runner, GameAction::SelectCards { cards: vec![bears] });
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
}

/// CR 605.3c: an ability suspended beneath the standing window has not resolved either.
#[test]
fn a_suspended_ancestor_mana_ability_cannot_be_activated_again() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let prismite = scenario.add_real_card(P0, "Prismite", Zone::Battlefield, db);
    let elf = scenario.add_real_card(P0, "Skyshroud Elf", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let ancestor = mana_costed(runner.state(), prismite);
    let inner = mana_costed(runner.state(), elf);
    act(&mut runner, ancestor.clone());
    act(&mut runner, inner.clone());
    assert_eq!(
        suspended(runner.state()),
        [inner, ancestor.clone()],
        "reach: the Elf's window stands inside Prismite's"
    );

    assert_refused(&mut runner, &ancestor);
}

struct ArchitectBoard {
    runner: GameRunner,
    altar: ObjectId,
    architect: ObjectId,
    /// The permanents named to the builder, in the order they were created.
    outer: Vec<ObjectId>,
    elf: ObjectId,
}

/// Phyrexian Altar, Grizzly Bears, Grand Architect, each of `outer` in order, and Skyshroud Elf.
fn architect_board(outer: &[&str], elf_tapped: bool) -> Option<ArchitectBoard> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let architect = scenario.add_real_card(P0, "Grand Architect", Zone::Battlefield, db);
    let outer: Vec<ObjectId> = outer
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
    let elf = scenario.add_real_card(P0, "Skyshroud Elf", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&elf).unwrap().tapped = elf_tapped;
    Some(ArchitectBoard {
        runner,
        altar,
        architect,
        outer,
        elf,
    })
}

/// Whether the pool is exactly Grand Architect's two artifact-only mana.
fn holds_only_architect_mana(state: &GameState) -> bool {
    let pool = &state.players[0].mana_pool;
    pool.total() == 2 && pool.units().all(|unit| !unit.restrictions.is_empty())
}

/// Suspends `outer`, nests the Elf's "{1}" window in it, then taps Grand Architect for mana.
/// Returns the two suspended activations, innermost first.
fn nest_then_tap_architect(board: &mut ArchitectBoard, outer: GameAction) -> [GameAction; 2] {
    let inner = mana_costed(board.runner.state(), board.elf);
    act(&mut board.runner, outer.clone());
    act(&mut board.runner, inner.clone());
    let chain = [inner, outer];
    assert_eq!(
        suspended(board.runner.state()),
        chain,
        "reach: the Elf's window stands inside the outer one"
    );
    tap_architect(&mut board.runner, board.architect);
    chain
}

/// With Prismite's "{2}" suspended beneath the Elf's window and Grand Architect's {C}{C} in the
/// pool, the pool could pay Prismite again; no reader of readiness may say so (CR 605.3c).
#[test]
fn a_suspended_ancestor_is_not_ready_once_the_pool_could_pay_it() {
    let Some(mut board) = architect_board(&["Prismite"], false) else {
        return;
    };
    let (prismite, elf, altar) = (board.outer[0], board.elf, board.altar);
    let ancestor = mana_costed(board.runner.state(), prismite);
    let chain = nest_then_tap_architect(&mut board, ancestor.clone());
    let state = board.runner.state();
    assert!(
        suspended(state) == chain && holds_only_architect_mana(state),
        "reach: artifact-only mana cannot pay the Elf, so its window stands: {:?}",
        state.waiting_for
    );

    let view = viewer_projection(state).1;
    assert_eq!(
        (
            legal_actions(state).contains(&ancestor),
            swept_ready(state, prismite),
            offers(&view, &ancestor),
        ),
        (false, false, false),
        "legal actions, the display sweep and the viewer's moves all leave the suspended \
         Prismite out"
    );
    let still_legal = [
        tap_costed(state, elf),
        GameAction::ActivateAbility {
            source_id: altar,
            ability_index: ability(state, altar, true),
        },
    ];
    for action in &still_legal {
        assert!(legal_actions(state).contains(action), "{action:?}");
        assert!(offers(&view, action), "{action:?}");
    }
    assert!(offers(&view, &GameAction::CancelCast));
    assert!(swept_ready(state, elf) && swept_ready(state, altar));
    assert_refused(&mut board.runner, &ancestor);

    // With no window open, both readers show a pool-funded Prismite as ready.
    let Some(mut free) = architect_board(&["Prismite"], false) else {
        return;
    };
    tap_architect(&mut free.runner, free.architect);
    let state = free.runner.state();
    assert!(swept_ready(state, free.outer[0]));
    assert!(offers(
        &viewer_projection(state).1,
        &mana_costed(state, free.outer[0])
    ));
}

/// Dimir Signet: "{1}, {T}: Add {U}{B}." Auto-tap picks its own sources, and a Signet suspended
/// beneath the Elf's window is not one of them (CR 605.3c).
#[test]
fn the_auto_payer_does_not_tap_a_suspended_ancestor() {
    let Some(mut board) = architect_board(&["Dimir Signet"], true) else {
        return;
    };
    let signet = board.outer[0];
    let outer = composite_costed(board.runner.state(), signet);
    let chain = nest_then_tap_architect(&mut board, outer);
    let state = board.runner.state();
    assert_eq!(
        suspended(state),
        chain,
        "the Elf's window stands over the Signet"
    );
    assert!(!state.objects[&signet].tapped);
    assert!(holds_only_architect_mana(state), "nothing was spent");

    // Not suspended, the same Signet is the payer's choice.
    let Some(mut free) = architect_board(&["Dimir Signet"], true) else {
        return;
    };
    tap_architect(&mut free.runner, free.architect);
    let inner = mana_costed(free.runner.state(), free.elf);
    act(&mut free.runner, inner);
    assert!(
        free.runner.state().objects[&free.outer[0]].tapped
            && matches!(
                free.runner.state().waiting_for,
                WaitingFor::ChooseManaColor { .. }
            ),
        "auto-tap pays the Elf with the Signet: {:?}",
        free.runner.state().waiting_for
    );

    // Prismite has no {T} in its cost and is never the payer's choice.
    let Some(mut board) = architect_board(&["Prismite"], true) else {
        return;
    };
    let outer = mana_costed(board.runner.state(), board.outer[0]);
    let chain = nest_then_tap_architect(&mut board, outer);
    assert_eq!(suspended(board.runner.state()), chain);
    assert!(holds_only_architect_mana(board.runner.state()));

    // Izzet Signet: "{1}, {T}: Add {U}{R}." Created after the suspended Dimir Signet, it is the
    // one the payer taps.
    let Some(mut board) = architect_board(&["Dimir Signet", "Izzet Signet"], true) else {
        return;
    };
    let (dimir, izzet) = (board.outer[0], board.outer[1]);
    let outer = composite_costed(board.runner.state(), dimir);
    nest_then_tap_architect(&mut board, outer);
    let state = board.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }),
        "the Elf is paid and asks its color: {:?}",
        state.waiting_for
    );
    assert!(state.objects[&izzet].tapped && !state.objects[&dimir].tapped);
}

/// Calciform Pools: "{T}: Add {C}." and "{1}, Remove X storage counters from this land: Add X
/// mana in any combination of {W} and/or {U}." Celestial Prism: "{2}, {T}: Add one mana of any
/// color." With the Pools' third ability suspended beneath the Prism's window, auto-tap leaves
/// the Pools alone and its "{T}: Add {C}" is the player's to activate (CR 605.3c).
#[test]
fn a_suspended_ancestors_other_mana_ability_is_left_for_the_player_to_tap() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let pools = scenario.add_real_card(P0, "Calciform Pools", Zone::Battlefield, db);
    let prism = scenario.add_real_card(P0, "Celestial Prism", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let storage = CounterType::Generic("storage".to_string());
    runner
        .state_mut()
        .objects
        .get_mut(&pools)
        .unwrap()
        .counters
        .insert(storage.clone(), 2);
    let outer = composite_costed(runner.state(), pools);
    let inner = composite_costed(runner.state(), prism);
    act(&mut runner, outer.clone());
    act(&mut runner, GameAction::SubmitPayAmount { amount: 1 });
    act(&mut runner, inner.clone());
    let chain = [inner, outer];
    assert_eq!(
        suspended(runner.state()),
        chain,
        "reach: the Prism's window stands inside the Pools'"
    );
    activate(&mut runner, altar, true);
    act(&mut runner, GameAction::SelectCards { cards: vec![bears] });
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "reach: the Altar's cost was paid"
    );

    let color = runner.act(GameAction::ChooseManaColor {
        choice: ManaChoice::SingleColor(ManaType::Red),
        count: 1,
    });
    assert!(color.is_ok(), "the Altar's color is accepted: {color:?}");
    let state = runner.state();
    assert_eq!(
        suspended(state),
        chain,
        "one mana of the Prism's {{2}}: its window stands"
    );
    assert!(!state.objects[&pools].tapped);
    let tap = land_tap(state, pools).expect("the window offers the Pools' {T}: Add {C}");

    act(&mut runner, tap);
    assert!(
        runner.state().objects[&prism].tapped
            && matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseManaColor { .. }
            ),
        "the Prism is paid and asks its color: {:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::White),
            count: 1,
        },
    );
    // The Prism's mana pays the Pools' {1}; X is 1, so the Pools adds one of {W} or {U}.
    let combination = legal_actions(runner.state())
        .into_iter()
        .find(|action| matches!(action, GameAction::ChooseManaColor { .. }))
        .expect("the Pools asks which mana it adds");
    act(&mut runner, combination);
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(pool_total(state), 1);
    assert_eq!(state.objects[&pools].counters.get(&storage), Some(&1));
}

/// The plays and answers the play trace holds, in order.
fn trace(state: &GameState) -> Vec<GameAction> {
    play_trace_view(state)
        .map_or_else(Vec::new, |view| view.entries)
        .into_iter()
        .filter_map(|entry| match entry.kind {
            EntryKind::Play { action, .. } | EntryKind::Answer { action, .. } => Some(action),
            EntryKind::Resolution { .. } => None,
        })
        .collect()
}

fn at_priority(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
}

/// P0's pool by mana type, in WUBRGC order.
fn pool(state: &GameState) -> Vec<(ManaType, usize)> {
    [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ]
    .into_iter()
    .map(|mana| (mana, state.players[0].mana_pool.count_color(mana)))
    .filter(|(_, count)| *count > 0)
    .collect()
}

fn color(mana: ManaType) -> GameAction {
    GameAction::ChooseManaColor {
        choice: ManaChoice::SingleColor(mana),
        count: 1,
    }
}

/// Each of `battlefield` and then each of `elsewhere` under P0, with the play trace kept.
fn traced_with(
    battlefield: &[&str],
    elsewhere: &[(&str, Zone)],
) -> Option<(GameRunner, Vec<ObjectId>)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let placed = battlefield
        .iter()
        .map(|name| (*name, Zone::Battlefield))
        .chain(elsewhere.iter().copied());
    let ids = placed
        .map(|(name, zone)| scenario.add_real_card(P0, name, zone, db))
        .collect();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    Some((runner, ids))
}

fn traced(battlefield: &[&str]) -> Option<(GameRunner, Vec<ObjectId>)> {
    traced_with(battlefield, &[])
}

/// Activates Phyrexian Altar and sacrifices `creature` to it; returns the two actions.
fn sacrifice_to_altar(
    runner: &mut GameRunner,
    altar: ObjectId,
    creature: ObjectId,
) -> [GameAction; 2] {
    let activation = GameAction::ActivateAbility {
        source_id: altar,
        ability_index: ability(runner.state(), altar, true),
    };
    let pick = GameAction::SelectCards {
        cards: vec![creature],
    };
    act(runner, activation.clone());
    act(runner, pick.clone());
    [activation, pick]
}

/// Whether any action offered at this prompt activates a mana ability of `id`.
fn offers_mana_ability_of(state: &GameState, id: ObjectId) -> bool {
    let per_object = legal_actions_full(state).2.into_values().flatten();
    legal_actions(state)
        .into_iter()
        .chain(per_object)
        .any(|action| match action {
            GameAction::ActivateAbility { source_id, .. } => source_id == id,
            GameAction::TapLandForMana { selection }
            | GameAction::ActivateManaSource { selection } => selection.source.object_id == id,
            _ => false,
        })
}

fn set_tapped(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
}

/// Celestial Prism: "{2}, {T}: Add one mana of any color." One mana is all the board can make, so
/// the Prism's activation, submitted directly, cannot be completed and is reversed (CR 602.2);
/// Phyrexian Altar's resolved ability stands (CR 733.1).
#[test]
fn a_window_left_short_of_its_cost_reverses_the_suspended_activation() {
    let Some((mut runner, ids)) = traced(&["Phyrexian Altar", "Grizzly Bears", "Celestial Prism"])
    else {
        return;
    };
    let (altar, bears, prism) = (ids[0], ids[1], ids[2]);
    let begun = composite_costed(runner.state(), prism);
    act(&mut runner, begun.clone());
    assert_eq!(
        suspended(runner.state()),
        [begun],
        "reach: the Prism's window"
    );
    let altar_entries = sacrifice_to_altar(&mut runner, altar, bears);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "reach: the Altar's color choice: {:?}",
        runner.state().waiting_for
    );

    act(&mut runner, color(ManaType::Red));

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Red, 1)]);
    assert_eq!(state.objects[&bears].zone, Zone::Graveyard);
    assert!(!state.objects[&prism].tapped);
    assert_eq!(
        trace(state),
        [
            altar_entries[0].clone(),
            altar_entries[1].clone(),
            color(ManaType::Red)
        ]
    );
}

/// With a second creature to sacrifice the Prism's {2} can still be funded, so the window of its
/// activation, submitted directly, stands and the player may withdraw it (CR 601.2g).
#[test]
fn a_window_that_can_still_be_funded_stands_and_can_be_cancelled() {
    let Some((mut runner, ids)) = traced(&[
        "Phyrexian Altar",
        "Grizzly Bears",
        "Grizzly Bears",
        "Celestial Prism",
    ]) else {
        return;
    };
    let (altar, bears, prism) = (ids[0], ids[1], ids[3]);
    let begun = composite_costed(runner.state(), prism);
    act(&mut runner, begun.clone());
    let altar_entries = sacrifice_to_altar(&mut runner, altar, bears);
    act(&mut runner, color(ManaType::Red));
    assert_eq!(suspended(runner.state()), [begun], "the window stands");
    assert_eq!(pool(runner.state()), [(ManaType::Red, 1)]);
    assert!(legal_actions(runner.state()).contains(&GameAction::CancelCast));

    act(&mut runner, GameAction::CancelCast);

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Red, 1)]);
    assert!(!state.objects[&prism].tapped);
    assert_eq!(
        trace(state),
        [
            altar_entries[0].clone(),
            altar_entries[1].clone(),
            color(ManaType::Red)
        ]
    );
}

/// Prismatic Lens: "{T}: Add {C}." and "{1}, {T}: Add one mana of any color." Tapping the Lens for
/// {C} inside the Elf's nested window leaves the Lens's suspended "{1}, {T}", submitted directly,
/// unpayable (CR 118.3): that ability is reversed and the Lens's completed tap stands.
#[test]
fn a_suspended_sources_own_tap_reverses_its_suspended_ability() {
    let Some((mut runner, ids)) = traced(&[
        "Phyrexian Altar",
        "Grizzly Bears",
        "Prismatic Lens",
        "Skyshroud Elf",
    ]) else {
        return;
    };
    let (lens, elf) = (ids[2], ids[3]);
    set_tapped(&mut runner, elf);
    let lens_costed = composite_costed(runner.state(), lens);
    let lens_tap = tap_costed(runner.state(), lens);
    let elf_costed = mana_costed(runner.state(), elf);
    act(&mut runner, lens_costed.clone());
    act(&mut runner, elf_costed.clone());
    assert_eq!(
        suspended(runner.state()),
        [elf_costed.clone(), lens_costed],
        "reach: the Elf's window stands inside the Lens's"
    );
    act(&mut runner, lens_tap.clone());
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. })
            && state.objects[&lens].tapped
            && pool(state).is_empty(),
        "reach: the Lens's {{C}} paid the Elf, which asks its color: {:?}",
        state.waiting_for
    );

    act(&mut runner, color(ManaType::Red));

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Red, 1)]);
    assert!(state.objects[&lens].tapped);
    assert_eq!(trace(state), [elf_costed, lens_tap, color(ManaType::Red)]);
}

/// Tapping Grand Architect itself for its own cost pays Pili-Pala's {2} and leaves Pili-Pala
/// untapped, so its {Q} cannot be paid (CR 107.6) and its activation, submitted directly, is
/// reversed.
#[test]
fn an_untap_cost_that_cannot_be_paid_reverses_pili_palas_activation() {
    for blue in [false, true] {
        let Some(mut board) = board(blue) else { return };
        board.runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        open_window(&mut board);
        let architect = architect_activation(&board);
        act(&mut board.runner, architect.clone());
        let offered: Vec<GameAction> = legal_actions(board.runner.state())
            .into_iter()
            .filter(|action| matches!(action, GameAction::SelectCards { .. }))
            .collect();
        let tap_architect = GameAction::SelectCards {
            cards: vec![board.architect],
        };

        act(&mut board.runner, tap_architect.clone());

        let state = board.runner.state();
        assert!(at_priority(state), "blue={blue}: {:?}", state.waiting_for);
        assert_eq!(pool_total(state), 2, "blue={blue}");
        assert!(!state.objects[&board.pili].tapped, "blue={blue}");
        assert_eq!(
            trace(state),
            [architect.clone(), tap_architect.clone()],
            "blue={blue}"
        );
        assert!(offered.contains(&tap_architect), "blue={blue}: {offered:?}");
        assert_eq!(
            offered.len(),
            if blue { 2 } else { 1 },
            "blue={blue}: {offered:?}"
        );
    }
}

/// The reversal forecloses nothing: tapping the blue Pili-Pala for Grand Architect's next
/// activation lets Pili-Pala's ability, submitted directly, be paid in full.
#[test]
fn after_the_reversal_pili_palas_activation_can_be_made_and_completed() {
    let Some(mut board) = board(true) else { return };
    open_window(&mut board);
    let architect = architect_activation(&board);
    act(&mut board.runner, architect.clone());
    act(
        &mut board.runner,
        GameAction::SelectCards {
            cards: vec![board.architect],
        },
    );
    assert!(
        at_priority(board.runner.state()) && pool_total(board.runner.state()) == 2,
        "reach: Pili-Pala's activation was reversed: {:?}",
        board.runner.state().waiting_for
    );

    act(&mut board.runner, architect);
    act(
        &mut board.runner,
        GameAction::SelectCards {
            cards: vec![board.pili],
        },
    );
    activate(&mut board.runner, board.pili, true);
    act(&mut board.runner, color(ManaType::Blue));

    let state = board.runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Blue, 1), (ManaType::Colorless, 2)]);
    assert!(!state.objects[&board.pili].tapped);
}

/// Sunken Ruins: "{T}: Add {C}." and "{U/B}, {T}: Add {U}{U}, {U}{B}, or {B}{B}." The Ruins' own
/// tap pays the Signet nested in the window of its filter ability, submitted directly; the filter
/// ability then asks which of the Signet's {U}{B} pays {U/B}, and neither answer can tap the
/// Ruins again (CR 118.3).
#[test]
fn a_hybrid_answer_that_cannot_be_carried_out_reverses_the_filter_ability() {
    for payment in [ManaType::Blue, ManaType::Black] {
        let Some((mut runner, ids)) = traced(&[
            "Phyrexian Altar",
            "Grizzly Bears",
            "Sunken Ruins",
            "Dimir Signet",
        ]) else {
            return;
        };
        let (ruins, signet) = (ids[2], ids[3]);
        let filter = composite_costed(runner.state(), ruins);
        let signet_activation = composite_costed(runner.state(), signet);
        act(&mut runner, filter.clone());
        assert_eq!(
            suspended(runner.state()),
            std::slice::from_ref(&filter),
            "reach: the Ruins' window"
        );
        assert!(
            legal_actions(runner.state()).contains(&signet_activation),
            "reach: the Signet is offered"
        );
        act(&mut runner, signet_activation.clone());
        assert_eq!(
            suspended(runner.state()),
            [signet_activation.clone(), filter],
            "reach: the Signet's window stands inside the Ruins'"
        );
        let tap = land_tap(runner.state(), ruins).expect("reach: the Ruins' tap is offered");
        act(&mut runner, tap.clone());
        assert!(
            matches!(
                &runner.state().waiting_for,
                WaitingFor::PayManaAbilityMana { pending_mana_ability, .. }
                    if pending_mana_ability.source_id == ruins
            ),
            "reach: the filter ability asks which mana pays {{U/B}}: {:?}",
            runner.state().waiting_for
        );

        act(
            &mut runner,
            GameAction::PayManaAbilityMana {
                payment: vec![payment],
            },
        );

        let state = runner.state();
        assert!(at_priority(state), "{payment:?}: {:?}", state.waiting_for);
        assert_eq!(pool(state), [(ManaType::Blue, 1), (ManaType::Black, 1)]);
        assert!(state.objects[&ruins].tapped && state.objects[&signet].tapped);
        assert_eq!(trace(state), [signet_activation, tap], "{payment:?}");
    }
}

/// CR 733.1: no effect applies as a result of an undone action. Paying the suspended filter
/// ability, submitted directly, activates the Signet before the Ruins turns out to be tapped; the
/// action's events are those of the Ruins' tap alone.
#[test]
fn a_reversed_activation_emits_no_events() {
    let Some((mut runner, ids)) = traced(&[
        "Phyrexian Altar",
        "Grizzly Bears",
        "Sunken Ruins",
        "Dimir Signet",
    ]) else {
        return;
    };
    let (ruins, signet) = (ids[2], ids[3]);
    let tap = tap_costed(runner.state(), ruins);
    let filter = composite_costed(runner.state(), ruins);
    let mut nothing_suspended = GameRunner::from_state(runner.state().clone());
    let tap_events = nothing_suspended
        .act(tap.clone())
        .expect("the Ruins taps for mana")
        .events;
    act(&mut runner, filter.clone());
    assert_eq!(
        suspended(runner.state()),
        [filter],
        "reach: the Ruins' window"
    );

    let result = runner.act(tap);

    let state = runner.state();
    let events = result.expect("the Ruins' tap is accepted").events;
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Colorless, 1)]);
    assert!(state.objects[&ruins].tapped && !state.objects[&signet].tapped);
    let signet_id = format!("{signet:?}");
    let about_signet: Vec<String> = events
        .iter()
        .map(|event| format!("{event:?}"))
        .filter(|event| event.contains(&signet_id))
        .collect();
    assert!(about_signet.is_empty(), "{about_signet:?}");
    assert_eq!(events.len(), tap_events.len());
}

/// CR 733.2: after the reversal the player goes on with what the activation was paying for, here
/// the spell's mana payment. The Lens's "{1}, {T}" is submitted directly.
#[test]
fn a_reversal_inside_a_spells_payment_returns_to_that_payment() {
    let Some((mut runner, ids)) = traced_with(
        &[
            "Phyrexian Altar",
            "Grizzly Bears",
            "Grizzly Bears",
            "Prismatic Lens",
        ],
        &[("Grizzly Bears", Zone::Hand)],
    ) else {
        return;
    };
    let (lens, spell) = (ids[3], ids[4]);
    let cast = GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: Vec::new(),
        payment_mode: engine::types::game_state::CastPaymentMode::Manual,
    };
    act(&mut runner, cast.clone());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the spell's payment: {:?}",
        runner.state().waiting_for
    );
    let lens_costed = composite_costed(runner.state(), lens);
    let lens_tap = tap_costed(runner.state(), lens);
    act(&mut runner, lens_costed.clone());
    assert_eq!(
        suspended(runner.state()),
        [lens_costed],
        "reach: the Lens's window"
    );

    act(&mut runner, lens_tap.clone());

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
        "{:?}",
        state.waiting_for
    );
    assert!(state.stack.len() == 1 && state.pending_cast.is_some());
    assert_eq!(pool(state), [(ManaType::Colorless, 1)]);
    assert!(state.objects[&lens].tapped);
    assert_eq!(trace(state), [cast, lens_tap.clone()]);

    act(&mut runner, GameAction::CancelCast);
    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert!(state.stack.is_empty() && state.pending_cast.is_none());
    assert_eq!(state.objects[&spell].zone, Zone::Hand);
    assert_eq!(trace(state), [lens_tap]);
}

/// The Altar's {R} cannot pay the Prism's {2}, so the Prism is reversed; the Lens's window it was
/// nested in decides its payment again, and {R} pays the Lens's {1}. The Lens's "{1}, {T}" is
/// submitted directly.
#[test]
fn a_reversed_activations_window_parent_decides_its_payment_again() {
    let Some((mut runner, ids)) = traced(&[
        "Phyrexian Altar",
        "Grizzly Bears",
        "Prismatic Lens",
        "Celestial Prism",
    ]) else {
        return;
    };
    let (altar, bears, lens, prism) = (ids[0], ids[1], ids[2], ids[3]);
    let lens_costed = composite_costed(runner.state(), lens);
    let prism_costed = composite_costed(runner.state(), prism);
    act(&mut runner, lens_costed.clone());
    act(&mut runner, prism_costed.clone());
    assert_eq!(
        suspended(runner.state()),
        [prism_costed, lens_costed.clone()],
        "reach: the Prism's window stands inside the Lens's"
    );
    let altar_entries = sacrifice_to_altar(&mut runner, altar, bears);

    act(&mut runner, color(ManaType::Red));

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }),
        "the Lens asks its color: {:?}",
        state.waiting_for
    );
    assert!(!state.objects[&prism].tapped && state.objects[&lens].tapped);
    assert!(pool(state).is_empty());

    act(&mut runner, color(ManaType::Blue));
    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Blue, 1)]);
    assert_eq!(
        trace(state),
        [
            lens_costed,
            altar_entries[0].clone(),
            altar_entries[1].clone(),
            color(ManaType::Red),
            color(ManaType::Blue)
        ]
    );
}

/// Two Celestial Prisms, the first submitted directly and the second nested in its window: one
/// mana completes neither, and each is reversed by its own activation.
#[test]
fn a_window_parent_that_cannot_be_completed_is_reversed_in_turn() {
    let Some((mut runner, ids)) = traced(&[
        "Phyrexian Altar",
        "Grizzly Bears",
        "Celestial Prism",
        "Celestial Prism",
    ]) else {
        return;
    };
    let (altar, bears, outer, inner) = (ids[0], ids[1], ids[2], ids[3]);
    let outer_costed = composite_costed(runner.state(), outer);
    let inner_costed = composite_costed(runner.state(), inner);
    act(&mut runner, outer_costed.clone());
    act(&mut runner, inner_costed.clone());
    assert_eq!(
        suspended(runner.state()),
        [inner_costed, outer_costed],
        "reach: one Prism's window stands inside the other's"
    );
    let altar_entries = sacrifice_to_altar(&mut runner, altar, bears);

    act(&mut runner, color(ManaType::Red));

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Red, 1)]);
    assert!(!state.objects[&outer].tapped && !state.objects[&inner].tapped);
    assert_eq!(
        trace(state),
        [
            altar_entries[0].clone(),
            altar_entries[1].clone(),
            color(ManaType::Red)
        ]
    );
}

/// Sunken Ruins tapped, the Signet's {U}{B} in the pool, and the Ruins' filter ability submitted
/// directly: returns the board at the prompt asking which mana pays {U/B}, with the two actions
/// the trace holds.
fn tapped_ruins_at_its_hybrid_prompt() -> Option<(GameRunner, [GameAction; 2])> {
    let (mut runner, ids) = traced(&["Sunken Ruins", "Dimir Signet"])?;
    let (ruins, signet) = (ids[0], ids[1]);
    let tap = tap_costed(runner.state(), ruins);
    let signet_activation = composite_costed(runner.state(), signet);
    let filter = composite_costed(runner.state(), ruins);
    act(&mut runner, tap.clone());
    act(&mut runner, signet_activation.clone());
    assert_eq!(
        pool(runner.state()),
        [(ManaType::Blue, 1), (ManaType::Black, 1)],
        "reach: the Signet's mana"
    );
    assert!(
        !offers_mana_ability_of(runner.state(), ruins),
        "reach: the tapped Ruins is not offered"
    );
    act(&mut runner, filter);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayManaAbilityMana { .. }
        ),
        "reach: the filter ability asks which mana pays {{U/B}}: {:?}",
        runner.state().waiting_for
    );
    Some((runner, [tap, signet_activation]))
}

/// With no payment window anywhere, a legal answer to the tapped Ruins' hybrid prompt reverses
/// the activation (CR 602.2).
#[test]
fn a_hybrid_answer_with_no_window_reverses_the_activation() {
    let Some((mut runner, entries)) = tapped_ruins_at_its_hybrid_prompt() else {
        return;
    };

    act(
        &mut runner,
        GameAction::PayManaAbilityMana {
            payment: vec![ManaType::Blue],
        },
    );

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Blue, 1), (ManaType::Black, 1)]);
    assert_eq!(trace(state), entries);
}

/// An answer that is not one of the prompt's options is refused and the activation stands; a
/// legal one is then accepted.
#[test]
fn an_answer_outside_the_prompts_options_is_refused() {
    let Some((mut runner, _)) = tapped_ruins_at_its_hybrid_prompt() else {
        return;
    };

    assert_refused(
        &mut runner,
        &GameAction::PayManaAbilityMana {
            payment: vec![ManaType::Green],
        },
    );

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayManaAbilityMana { .. }
    ));
    act(
        &mut runner,
        GameAction::PayManaAbilityMana {
            payment: vec![ManaType::Black],
        },
    );
    assert!(at_priority(runner.state()));
}

/// Shimmering Grotto: "{T}: Add {C}." and "{1}, {T}: Add one mana of any color." The tap that
/// strands the Grotto's suspended ability, submitted directly, stays on the trace as a land's
/// tap, which an untap reverses.
#[test]
fn a_land_route_reversal_leaves_the_tap_untappable() {
    let Some((mut runner, ids)) =
        traced(&["Phyrexian Altar", "Grizzly Bears", "Shimmering Grotto"])
    else {
        return;
    };
    let grotto = ids[2];
    let begun = composite_costed(runner.state(), grotto);
    act(&mut runner, begun.clone());
    assert_eq!(
        suspended(runner.state()),
        [begun],
        "reach: the Grotto's window"
    );
    let tap = land_tap(runner.state(), grotto).expect("reach: the Grotto's tap is offered");

    act(&mut runner, tap.clone());

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Colorless, 1)]);
    assert_eq!(trace(state), [tap]);

    act(
        &mut runner,
        GameAction::UntapLandForMana { object_id: grotto },
    );
    assert!(trace(runner.state()).is_empty());
    assert!(!runner.state().objects[&grotto].tapped);
}

/// Submits `opener`, which is not offered, and answers the cost prompt it raises with `choice`,
/// its one option. The activation cannot go on, so it is reversed (CR 602.2): the player has
/// priority, and nothing was produced or recorded.
fn assert_the_cost_answer_reverses_the_activation(
    runner: &mut GameRunner,
    opener: GameAction,
    choice: ObjectId,
) {
    let GameAction::ActivateAbility { source_id, .. } = opener else {
        panic!("an ability activation: {opener:?}");
    };
    assert!(
        !offers_mana_ability_of(runner.state(), source_id),
        "reach: the opener is not offered"
    );
    act(runner, opener);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::PayCost { choices, .. } if *choices == [choice]
        ),
        "reach: the cost prompt offers the one choice: {:?}",
        runner.state().waiting_for
    );

    act(
        runner,
        GameAction::SelectCards {
            cards: vec![choice],
        },
    );

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert!(pool(state).is_empty());
    assert!(trace(state).is_empty(), "{:?}", trace(state));
}

/// Transmogrant Altar: "{B}, {T}, Sacrifice a creature: Add {C}{C}{C}." Nothing pays {B}; the
/// ability is submitted directly.
#[test]
fn a_sacrifice_answer_that_cannot_be_carried_out_reverses_the_activation() {
    let Some((mut runner, ids)) = traced(&["Transmogrant Altar", "Grizzly Bears"]) else {
        return;
    };
    let (altar, bears) = (ids[0], ids[1]);
    let opener = composite_costed(runner.state(), altar);
    assert_the_cost_answer_reverses_the_activation(&mut runner, opener, bears);
    let state = runner.state();
    assert_eq!(state.objects[&bears].zone, Zone::Battlefield);
    assert!(!state.objects[&altar].tapped);
}

/// Bog Witch: "{B}, {T}, Discard a card: Add {B}{B}{B}." Nothing pays {B}; the ability is
/// submitted directly.
#[test]
fn a_discard_answer_that_cannot_be_carried_out_reverses_the_activation() {
    let Some((mut runner, ids)) = traced_with(&["Bog Witch"], &[("Grizzly Bears", Zone::Hand)])
    else {
        return;
    };
    let (witch, card) = (ids[0], ids[1]);
    runner
        .state_mut()
        .objects
        .get_mut(&witch)
        .unwrap()
        .summoning_sick = false;
    let opener = composite_costed(runner.state(), witch);
    assert_the_cost_answer_reverses_the_activation(&mut runner, opener, card);
    let state = runner.state();
    assert_eq!(state.objects[&card].zone, Zone::Hand);
    assert!(!state.objects[&witch].tapped);
}

/// Springleaf Drum: "{T}, Tap an untapped creature you control: Add one mana of any color." The
/// Drum is already tapped; the ability is submitted directly.
#[test]
fn a_tapped_creature_answer_that_cannot_be_carried_out_reverses_the_activation() {
    let Some((mut runner, ids)) = traced(&["Springleaf Drum", "Grizzly Bears"]) else {
        return;
    };
    let (drum, bears) = (ids[0], ids[1]);
    set_tapped(&mut runner, drum);
    let opener = composite_costed(runner.state(), drum);
    assert_the_cost_answer_reverses_the_activation(&mut runner, opener, bears);
    assert!(!runner.state().objects[&bears].tapped);
}

/// Molt Tender: "{T}, Exile a card from your graveyard: Add one mana of any color." The Tender is
/// already tapped; the ability is submitted directly.
#[test]
fn an_exile_answer_that_cannot_be_carried_out_reverses_the_activation() {
    let Some((mut runner, ids)) =
        traced_with(&["Molt Tender"], &[("Grizzly Bears", Zone::Graveyard)])
    else {
        return;
    };
    let (tender, card) = (ids[0], ids[1]);
    runner
        .state_mut()
        .objects
        .get_mut(&tender)
        .unwrap()
        .summoning_sick = false;
    set_tapped(&mut runner, tender);
    let opener = composite_costed(runner.state(), tender);
    assert_the_cost_answer_reverses_the_activation(&mut runner, opener, card);
    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
}

/// Calciform Pools' "{1}, Remove X storage counters from this land" with the Pools tapped and
/// nothing to pay {1}; the ability is submitted directly.
#[test]
fn an_amount_answer_that_cannot_be_carried_out_reverses_the_activation() {
    let Some((mut runner, ids)) = traced(&["Calciform Pools"]) else {
        return;
    };
    let pools = ids[0];
    let storage = CounterType::Generic("storage".to_string());
    set_tapped(&mut runner, pools);
    runner
        .state_mut()
        .objects
        .get_mut(&pools)
        .unwrap()
        .counters
        .insert(storage.clone(), 2);
    let opener = composite_costed(runner.state(), pools);
    assert!(
        !offers_mana_ability_of(runner.state(), pools),
        "reach: the opener is not offered"
    );
    act(&mut runner, opener);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { .. }
        ),
        "reach: the Pools asks X: {:?}",
        runner.state().waiting_for
    );

    act(&mut runner, GameAction::SubmitPayAmount { amount: 2 });

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(state.objects[&pools].counters.get(&storage), Some(&2));
    assert!(pool(state).is_empty());
    assert!(trace(state).is_empty(), "{:?}", trace(state));
}

/// The Food Court: "Sacrifice three Foods: Add {W}{U}{B}{R}{G}." One Food named three times is
/// not three Foods (CR 118.3), so that answer is refused and the prompt stands for a legal one.
#[test]
fn one_food_named_three_times_is_refused_at_the_food_courts_prompt() {
    let Some((mut runner, ids)) = traced(&[
        "The Food Court",
        "Carrot Cake",
        "Golden Egg",
        "Tough Cookie",
        "Heaped Harvest",
    ]) else {
        return;
    };
    let (court, foods) = (ids[0], &ids[1..]);
    assert!(
        offers_mana_ability_of(runner.state(), court),
        "reach: the opener is offered"
    );
    activate(&mut runner, court, true);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::PayCost { kind: PayCostKind::Sacrifice, choices, count: 3, .. }
                if choices == foods
        ),
        "reach: the prompt asks for three of the four Foods: {:?}",
        runner.state().waiting_for
    );

    assert_refused(
        &mut runner,
        &GameAction::SelectCards {
            cards: vec![foods[0]; 3],
        },
    );

    act(
        &mut runner,
        GameAction::SelectCards {
            cards: foods[..3].to_vec(),
        },
    );
    assert_eq!(
        pool(runner.state()),
        [
            (ManaType::White, 1),
            (ManaType::Blue, 1),
            (ManaType::Black, 1),
            (ManaType::Red, 1),
            (ManaType::Green, 1),
        ]
    );
}

/// The creature chosen for Transmogrant Altar's cost before its window opened is sacrificed to
/// Phyrexian Altar inside it, so the cost can no longer be paid (CR 601.2h). Transmogrant Altar's
/// ability is submitted directly.
#[test]
fn a_selection_spent_inside_the_window_reverses_the_activation() {
    let Some((mut runner, ids)) =
        traced(&["Transmogrant Altar", "Phyrexian Altar", "Grizzly Bears"])
    else {
        return;
    };
    let (transmogrant, phyrexian, bears) = (ids[0], ids[1], ids[2]);
    let opener = composite_costed(runner.state(), transmogrant);
    assert!(
        !offers_mana_ability_of(runner.state(), transmogrant),
        "reach: the opener is not offered"
    );
    let pick = GameAction::SelectCards { cards: vec![bears] };
    act(&mut runner, opener.clone());
    act(&mut runner, pick.clone());
    assert_eq!(
        suspended(runner.state()),
        [opener],
        "reach: Transmogrant Altar's window"
    );
    let altar = GameAction::ActivateAbility {
        source_id: phyrexian,
        ability_index: ability(runner.state(), phyrexian, true),
    };
    assert!(
        legal_actions(runner.state()).contains(&altar),
        "reach: Phyrexian Altar is offered"
    );
    act(&mut runner, altar.clone());
    assert!(
        legal_actions(runner.state()).contains(&pick),
        "reach: the chosen creature is offered"
    );
    act(&mut runner, pick.clone());

    act(&mut runner, color(ManaType::Black));

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Black, 1)]);
    assert_eq!(state.objects[&bears].zone, Zone::Graveyard);
    assert!(!state.objects[&transmogrant].tapped);
    assert_eq!(trace(state), [altar, pick, color(ManaType::Black)]);
}

/// Cancelling the Prism nested in the Pools' window reverses the Prism alone (CR 733.1); the
/// Pools' activation, submitted directly, decides its payment again, and the Altar's {R} pays its
/// {1}.
#[test]
fn an_inner_cancel_continues_the_outer_activation() {
    let Some((mut runner, ids)) = traced(&[
        "Phyrexian Altar",
        "Grizzly Bears",
        "Calciform Pools",
        "Celestial Prism",
    ]) else {
        return;
    };
    let (altar, bears, pools, prism) = (ids[0], ids[1], ids[2], ids[3]);
    let storage = CounterType::Generic("storage".to_string());
    set_tapped(&mut runner, pools);
    runner
        .state_mut()
        .objects
        .get_mut(&pools)
        .unwrap()
        .counters
        .insert(storage.clone(), 2);
    let outer = composite_costed(runner.state(), pools);
    let inner = composite_costed(runner.state(), prism);
    let amount = GameAction::SubmitPayAmount { amount: 1 };
    act(&mut runner, outer.clone());
    act(&mut runner, amount.clone());
    act(&mut runner, inner.clone());
    let chain = [inner, outer.clone()];
    assert_eq!(
        suspended(runner.state()),
        chain,
        "reach: the Prism's window stands inside the Pools'"
    );
    let altar_entries = sacrifice_to_altar(&mut runner, altar, bears);
    act(&mut runner, color(ManaType::Red));
    assert_eq!(
        suspended(runner.state()),
        chain,
        "reach: one mana of the Prism's {{2}}: its window stands"
    );
    assert_eq!(pool(runner.state()), [(ManaType::Red, 1)]);

    act(&mut runner, GameAction::CancelCast);

    let mut answers = Vec::new();
    for _ in 0..4 {
        if at_priority(runner.state()) {
            break;
        }
        let Some(answer) = legal_actions(runner.state())
            .into_iter()
            .find(|action| !matches!(action, GameAction::CancelCast))
        else {
            break;
        };
        act(&mut runner, answer.clone());
        answers.push(answer);
    }
    let state = runner.state();
    assert!(
        at_priority(state),
        "the Pools' activation went on: {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&pools].counters.get(&storage), Some(&1));
    assert_eq!(pool_total(state), 1);
    assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 0);
    assert!(!state.objects[&prism].tapped);
    let mut expected = vec![
        outer,
        amount,
        altar_entries[0].clone(),
        altar_entries[1].clone(),
        color(ManaType::Red),
    ];
    expected.extend(answers);
    assert_eq!(trace(state), expected);
}

/// The Lens's own tap funds one mana of the Prism nested in the window of the Lens's "{1}, {T}",
/// submitted directly. Cancelling the Prism leaves the Lens's suspended ability unpayable
/// (CR 118.3), so it is reversed with it; the Lens's completed tap stands (CR 733.1).
#[test]
fn an_inner_cancel_reverses_an_outer_activation_it_leaves_unpayable() {
    let Some((mut runner, ids)) = traced(&[
        "Phyrexian Altar",
        "Grizzly Bears",
        "Prismatic Lens",
        "Celestial Prism",
    ]) else {
        return;
    };
    let (lens, prism) = (ids[2], ids[3]);
    let lens_costed = composite_costed(runner.state(), lens);
    let lens_tap = tap_costed(runner.state(), lens);
    let prism_costed = composite_costed(runner.state(), prism);
    act(&mut runner, lens_costed.clone());
    assert_eq!(
        suspended(runner.state()),
        std::slice::from_ref(&lens_costed),
        "reach: the Lens's window"
    );
    act(&mut runner, prism_costed.clone());
    let chain = [prism_costed, lens_costed];
    assert_eq!(
        suspended(runner.state()),
        chain,
        "reach: the Prism's window stands inside the Lens's"
    );
    act(&mut runner, lens_tap.clone());
    let state = runner.state();
    assert!(
        suspended(state) == chain
            && pool(state) == [(ManaType::Colorless, 1)]
            && state.objects[&lens].tapped,
        "reach: one mana of the Prism's {{2}}: its window stands: {:?}",
        state.waiting_for
    );

    act(&mut runner, GameAction::CancelCast);

    let state = runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Colorless, 1)]);
    assert!(state.objects[&lens].tapped && !state.objects[&prism].tapped);
    assert_eq!(trace(state), [lens_tap]);
}

/// The index among `holder`'s abilities of its copy of `of`'s first ability.
fn granted(state: &GameState, holder: ObjectId, of: ObjectId) -> usize {
    let ability = &state.objects[&of].abilities[0];
    state.objects[&holder]
        .abilities
        .iter()
        .position(|held| held == ability)
        .expect("the granted ability")
}

struct MimicBoard {
    runner: GameRunner,
    marvin: ObjectId,
    /// The index Marvin's "{2}" ability was activated at.
    begun_at: usize,
    /// The index it has since Rishkar left.
    moved_to: usize,
    /// The Altar's completed activation.
    altar_entries: Vec<GameAction>,
}

/// Marvin, Murderous Mimic: "Marvin has all activated abilities of creatures you control that
/// don't have the same name as this creature." Rishkar, Peema Renegade: "Each creature you
/// control with a counter on it has "{T}: Add {G}."" Bog Initiate: "{1}: Add {B}." Marvin's copy
/// of Prismite's "{2}: Add one mana of any color." is suspended at its window when Rishkar is
/// sacrificed to Phyrexian Altar, so Rishkar's ability leaves Marvin and Marvin's other abilities
/// each move down one.
fn mimic_board_after_rishkar_leaves() -> Option<MimicBoard> {
    let (mut runner, ids) = traced(&[
        "Rishkar, Peema Renegade",
        "Prismite",
        "Bog Initiate",
        "Marvin, Murderous Mimic",
        "Phyrexian Altar",
    ])?;
    let (rishkar, prismite, initiate, marvin, altar) = (ids[0], ids[1], ids[2], ids[3], ids[4]);
    runner
        .state_mut()
        .objects
        .get_mut(&marvin)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    engine::game::layers::evaluate_layers(runner.state_mut());
    let begun_at = granted(runner.state(), marvin, prismite);
    let begun = GameAction::ActivateAbility {
        source_id: marvin,
        ability_index: begun_at,
    };
    act(&mut runner, begun.clone());
    assert_eq!(
        suspended(runner.state()),
        std::slice::from_ref(&begun),
        "reach: Marvin's {{2}} opens its payment window"
    );
    let mut altar_entries = sacrifice_to_altar(&mut runner, altar, rishkar).to_vec();
    act(&mut runner, color(ManaType::Red));
    altar_entries.push(color(ManaType::Red));
    let state = runner.state();
    let moved_to = granted(state, marvin, prismite);
    assert!(
        state.objects[&rishkar].zone == Zone::Graveyard
            && suspended(state) == [begun]
            && moved_to + 1 == begun_at
            && granted(state, marvin, initiate) == begun_at,
        "reach: Rishkar's ability left Marvin inside the standing window: {:?}",
        state.waiting_for
    );
    Some(MimicBoard {
        runner,
        marvin,
        begun_at,
        moved_to,
        altar_entries,
    })
}

/// CR 605.3c names the ability, wherever it now sits among its source's abilities.
#[test]
fn a_suspended_mana_ability_cannot_be_activated_again_where_it_has_moved() {
    let Some(mut board) = mimic_board_after_rishkar_leaves() else {
        return;
    };
    let moved = GameAction::ActivateAbility {
        source_id: board.marvin,
        ability_index: board.moved_to,
    };

    assert_refused(&mut board.runner, &moved);
}

/// Bog Initiate's ability, now where the suspended one was activated, is another ability
/// (CR 605.3c); cancelling afterwards withdraws the suspended "{2}" alone (CR 733.1).
#[test]
fn another_ability_where_the_suspended_one_was_is_activated_and_survives_its_cancel() {
    let Some(mut board) = mimic_board_after_rishkar_leaves() else {
        return;
    };
    let other = GameAction::ActivateAbility {
        source_id: board.marvin,
        ability_index: board.begun_at,
    };

    act(&mut board.runner, other.clone());

    let state = board.runner.state();
    assert!(
        suspended(state).len() == 1 && pool(state) == [(ManaType::Black, 1)],
        "the Altar's mana paid for {{B}} and the window stands: {:?}",
        state.waiting_for
    );

    act(&mut board.runner, GameAction::CancelCast);

    let state = board.runner.state();
    assert!(at_priority(state), "{:?}", state.waiting_for);
    assert_eq!(pool(state), [(ManaType::Black, 1)]);
    let mut completed = board.altar_entries;
    completed.push(other);
    assert_eq!(trace(state), completed);
}

/// Two Prismites give Marvin two "{2}: Add one mana of any color." abilities, each functioning
/// independently (CR 113.2c); the engine keeps one of equal granted abilities, so Marvin's list is
/// set by hand to what its text gives. One being suspended leaves the other to be activated.
#[test]
fn an_identical_second_ability_is_activated_while_the_first_is_suspended() {
    let Some((mut runner, ids)) = traced(&[
        "Prismite",
        "Prismite",
        "Marvin, Murderous Mimic",
        "Phyrexian Altar",
    ]) else {
        return;
    };
    let (prismite, marvin) = (ids[0], ids[2]);
    engine::game::layers::evaluate_layers(runner.state_mut());
    let twin = runner.state().objects[&prismite].abilities[0].clone();
    runner
        .state_mut()
        .objects
        .get_mut(&marvin)
        .unwrap()
        .abilities = vec![twin.clone(), twin].into();
    let [first, second] = [0, 1].map(|ability_index| GameAction::ActivateAbility {
        source_id: marvin,
        ability_index,
    });
    act(&mut runner, first.clone());
    assert_eq!(
        suspended(runner.state()),
        std::slice::from_ref(&first),
        "reach: the first {{2}} opens its payment window"
    );

    act(&mut runner, second.clone());

    assert_eq!(
        suspended(runner.state()),
        [second.clone(), first.clone()],
        "the second {{2}} opens its window inside the first's"
    );
    assert_refused(&mut runner, &first);
    assert_refused(&mut runner, &second);

    act(&mut runner, GameAction::CancelCast);

    let state = runner.state();
    assert_eq!(suspended(state), std::slice::from_ref(&first));
    assert_eq!(trace(state), [first]);
    assert_eq!(state.objects[&marvin].abilities.len(), 2);
}
