//! Wizard's Rockets: `{X}, {T}, Sacrifice this artifact: Add X mana in any
//! combination of colors.` — a mana ability whose activation cost carries an
//! `{X}` mana leg.
//!
//! The controller announces X as the ability is activated, before any cost is
//! paid (CR 107.3a; CR 601.2b via CR 602.2b). The `{X}` leg is then paid as X
//! generic mana from the pool or other mana sources — never from this source,
//! whose `{T}` is part of the same cost (CR 605.3c, CR 118.3) — and the ability
//! adds X mana using that same announced X (CR 107.3i, CR 605.3a). The
//! activation can be begun inside another payment and returns to it.
//!
//! Every test drives the real pipeline through `GameRunner::act`:
//!   ActivateAbility → PayAmountChoice{ManaGeneric} (announce X)
//!     → SubmitPayAmount → cost paid → ChooseManaColor{AnyCombination count=X}.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityCost, AbilityKind};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaAbilityResume, ManaChoice, ManaChoicePrompt, PayCostKind,
    PayableResource, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WIZARDS_ROCKETS: &str = "This artifact enters tapped.\n\
{X}, {T}, Sacrifice this artifact: Add X mana in any combination of colors.\n\
When this artifact is put into a graveyard from the battlefield, draw a card.";

const PHYREXIAN_ALTAR: &str = "Sacrifice a creature: Add one mana of any color.";
const AZORIUS_SIGNET: &str = "{1}, {T}: Add {W}{U}.";
const MORBID_CURIOSITY: &str =
    "As an additional cost to cast this spell, sacrifice an artifact or creature.\n\
Draw cards equal to the mana value of the sacrificed permanent.";

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn add_rockets(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Wizard's Rockets", WIZARDS_ROCKETS)
        .id()
}

fn cost_has_x_mana_leg(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Mana { cost } => cost.has_x(),
        AbilityCost::Composite { costs } => costs.iter().any(cost_has_x_mana_leg),
        _ => false,
    }
}

/// The activated ability whose cost carries an `{X}` mana leg.
fn x_mana_index(state: &GameState, id: ObjectId) -> usize {
    state.objects[&id]
        .abilities
        .iter()
        .position(|a| {
            a.kind == AbilityKind::Activated && a.cost.as_ref().is_some_and(cost_has_x_mana_leg)
        })
        .expect("an activated ability with an {X} mana leg")
}

fn act(runner: &mut GameRunner, action: GameAction) {
    let shown = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{shown} rejected: {error:?}"));
}

fn activate_action(state: &GameState, source: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: source,
        ability_index: x_mana_index(state, source),
        payment_mode: CastPaymentMode::Auto,
    }
}

fn activate(runner: &mut GameRunner, source: ObjectId) {
    let action = activate_action(runner.state(), source);
    act(runner, action);
}

/// True iff `legal_actions_full` offers this source's `{X}` mana ability, in the
/// flat list or the per-object grouping.
fn lists_activation(state: &GameState, source: ObjectId) -> bool {
    let index = x_mana_index(state, source);
    let (flat, _, grouped) = legal_actions_full(state);
    flat.iter().chain(grouped.values().flatten()).any(|action| {
        matches!(action, GameAction::ActivateAbility { source_id, ability_index, .. }
                if *source_id == source && *ability_index == index)
    })
}

/// Asserts the X announcement prompt for `source` and returns its `max`.
fn expect_announcement(state: &GameState, source: ObjectId) -> u32 {
    match &state.waiting_for {
        WaitingFor::PayAmountChoice {
            resource: PayableResource::ManaGeneric { base_cost },
            min,
            max,
            accumulated,
            source_id,
            pending_mana_ability: Some(pending),
            ..
        } => {
            assert_eq!(
                *base_cost,
                ManaCost::Cost {
                    shards: vec![ManaCostShard::X],
                    generic: 0
                },
                "the announced leg is {{X}}"
            );
            assert_eq!(*min, 0, "CR 107.1b: X may be 0");
            assert_eq!(*accumulated, 0);
            assert_eq!(*source_id, source);
            assert_eq!(pending.source_id, source);
            assert_eq!(pending.chosen_x, None, "X is not yet announced");
            *max
        }
        other => {
            panic!("expected the X announcement PayAmountChoice{{ManaGeneric}}, got {other:?}")
        }
    }
}

fn expect_any_combination(state: &GameState) -> usize {
    match &state.waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::AnyCombination { count, .. },
            ..
        } => *count,
        other => panic!("expected ChooseManaColor{{AnyCombination}}, got {other:?}"),
    }
}

fn choose_colors(runner: &mut GameRunner, colors: Vec<ManaType>) {
    act(
        runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(colors),
            count: 1,
        },
    );
}

fn pool_total(state: &GameState) -> usize {
    state.players[0].mana_pool.total()
}

fn count(state: &GameState, mana: ManaType) -> usize {
    state.players[0].mana_pool.count_color(mana)
}

fn zone(state: &GameState, id: ObjectId) -> Zone {
    state.objects[&id].zone
}

fn tapped(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id].tapped
}

fn untap(runner: &mut GameRunner, id: ObjectId) {
    // "This artifact enters tapped." does not apply: the scenario places the
    // artifact as a permanent that entered on an earlier turn.
    runner.state_mut().objects.get_mut(&id).unwrap().tapped = false;
}

/// T1: X is announced before any cost is paid, the `{X}` leg is paid from
/// other sources, and the ability adds exactly X mana.
#[test]
fn rockets_announces_x_before_costs_and_adds_x_mana() {
    let mut scenario = scenario();
    let rockets = add_rockets(&mut scenario);
    let m1 = scenario.add_basic_land(P0, ManaColor::Red);
    let m2 = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    untap(&mut runner, rockets);

    activate(&mut runner, rockets);

    // CR 107.3a + CR 601.2b via CR 602.2b: the announcement comes first.
    let max = expect_announcement(runner.state(), rockets);
    assert_eq!(max, 2, "CR 118.3: X is bounded by the two Mountains");
    let state = runner.state();
    assert!(
        !tapped(state, rockets),
        "no cost paid yet: Rockets untapped"
    );
    assert_eq!(
        zone(state, rockets),
        Zone::Battlefield,
        "not yet sacrificed"
    );
    assert!(!tapped(state, m1) && !tapped(state, m2), "no mana paid yet");
    assert_eq!(pool_total(state), 0);

    // Over the cap is refused and the prompt stays.
    assert!(runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .is_err());
    assert_eq!(expect_announcement(runner.state(), rockets), 2);

    act(&mut runner, GameAction::SubmitPayAmount { amount: 2 });

    // CR 107.3i + CR 605.3a: the cost was paid with X = 2 and the yield reads
    // the same X.
    assert_eq!(expect_any_combination(runner.state()), 2);
    let state = runner.state();
    assert!(
        tapped(state, m1) && tapped(state, m2),
        "the {{X}} leg was paid as two generic mana"
    );
    assert_eq!(zone(state, rockets), Zone::Graveyard, "Rockets sacrificed");
    assert_eq!(pool_total(state), 0, "the Mountains' mana paid the cost");

    choose_colors(&mut runner, vec![ManaType::Green, ManaType::Blue]);
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(pool_total(state), 2);
    assert_eq!(count(state, ManaType::Green), 1);
    assert_eq!(count(state, ManaType::Blue), 1);
    assert_eq!(count(state, ManaType::Red), 0, "no Mountain mana left over");
}

/// T2: X = 0 is legal; the tap and sacrifice are still paid and no mana is
/// spent or added.
#[test]
fn rockets_x_zero_pays_tap_and_sacrifice_only() {
    let mut scenario = scenario();
    let rockets = add_rockets(&mut scenario);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    untap(&mut runner, rockets);

    activate(&mut runner, rockets);
    assert_eq!(expect_announcement(runner.state(), rockets), 1);
    act(&mut runner, GameAction::SubmitPayAmount { amount: 0 });

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "X = 0 adds no mana, so there is no color prompt: {:?}",
        state.waiting_for
    );
    assert!(!tapped(state, mountain), "nothing paid for {{0}}");
    assert_eq!(zone(state, rockets), Zone::Graveyard, "Rockets sacrificed");
    assert_eq!(pool_total(state), 0);
}

/// T3: payment enumeration sees the concretized cost, funded by the pool and a
/// land together.
#[test]
fn rockets_x_leg_paid_from_pool_and_land() {
    let mut scenario = scenario();
    let rockets = add_rockets(&mut scenario);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    untap(&mut runner, rockets);

    activate(&mut runner, rockets);
    assert_eq!(
        expect_announcement(runner.state(), rockets),
        2,
        "pool 1 + Mountain 1"
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 2 });

    assert_eq!(expect_any_combination(runner.state()), 2);
    let state = runner.state();
    assert_eq!(pool_total(state), 0, "the pooled mana paid part of {{2}}");
    assert!(tapped(state, mountain), "the Mountain paid the rest");

    choose_colors(&mut runner, vec![ManaType::White, ManaType::Black]);
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(pool_total(state), 2);
    assert_eq!(count(state, ManaType::White), 1);
    assert_eq!(count(state, ManaType::Black), 1);
}

/// T9: when the floating mana alone covers the announced X, the pool pays the
/// whole `{X}` leg and no land is tapped.
#[test]
fn rockets_x_leg_paid_entirely_from_pool() {
    let mut scenario = scenario();
    let rockets = add_rockets(&mut scenario);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    untap(&mut runner, rockets);

    activate(&mut runner, rockets);
    assert_eq!(
        expect_announcement(runner.state(), rockets),
        3,
        "pool 2 + Mountain 1"
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 2 });

    assert_eq!(expect_any_combination(runner.state()), 2);
    let state = runner.state();
    assert_eq!(pool_total(state), 0, "the two floating mana paid {{2}}");
    assert!(!tapped(state, mountain), "no land was needed");
    assert_eq!(zone(state, rockets), Zone::Graveyard, "Rockets sacrificed");

    choose_colors(&mut runner, vec![ManaType::Green, ManaType::Green]);
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(pool_total(state), 2);
    assert_eq!(count(state, ManaType::Green), 2);
    assert_eq!(
        count(state, ManaType::Colorless),
        0,
        "the pooled mana was spent"
    );
    assert!(!tapped(state, mountain));
}

/// T4: with the pool empty and no auto-tappable source, the `{X}` leg opens the
/// mana ability's payment window with X bound; Phyrexian Altar pays it there,
/// and Rockets is not offered again while it is suspended (CR 605.3c).
#[test]
fn rockets_x_leg_paid_inside_its_payment_window() {
    let mut scenario = scenario();
    let rockets = add_rockets(&mut scenario);
    let altar = scenario
        .add_artifact_from_oracle(P0, "Phyrexian Altar", PHYREXIAN_ALTAR)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    untap(&mut runner, rockets);

    activate(&mut runner, rockets);
    assert_eq!(
        expect_announcement(runner.state(), rockets),
        1,
        "the Altar can make one mana by sacrificing the Bears"
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 1 });

    let state = runner.state();
    match &state.waiting_for {
        WaitingFor::ManaAbilityManaPayment {
            pending_mana_ability,
            ..
        } => {
            assert_eq!(pending_mana_ability.source_id, rockets);
            assert_eq!(pending_mana_ability.chosen_x, Some(1), "X stays bound");
        }
        other => panic!("expected Rockets' payment window, got {other:?}"),
    }
    let altar_index = state.objects[&altar]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated)
        .expect("Altar's mana ability");
    let (flat, _, grouped) = legal_actions_full(state);
    assert!(
        flat.iter()
            .chain(grouped.values().flatten())
            .any(|a| matches!(
                a,
                GameAction::ActivateAbility { source_id, ability_index, .. }
                    if *source_id == altar && *ability_index == altar_index
            )),
        "the window offers the Altar"
    );
    assert!(
        !lists_activation(state, rockets),
        "CR 605.3c: Rockets can't be activated again until it has resolved"
    );

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: altar,
            ability_index: altar_index,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ..
        }
    ));
    act(&mut runner, GameAction::SelectCards { cards: vec![bears] });
    act(
        &mut runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Black),
            count: 1,
        },
    );

    // The Altar's Black paid Rockets' {1}; Rockets now asks for its one color.
    assert_eq!(expect_any_combination(runner.state()), 1);
    choose_colors(&mut runner, vec![ManaType::Green]);

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(pool_total(state), 1);
    assert_eq!(count(state, ManaType::Green), 1);
    assert_eq!(zone(state, bears), Zone::Graveyard);
    assert_eq!(zone(state, rockets), Zone::Graveyard);
}

/// T5: the X cap leaves out the activating source, even when it has another
/// mana ability of its own.
#[test]
fn rockets_x_cap_excludes_the_activating_source() {
    let mut scenario = scenario();
    let rig = scenario
        .add_artifact_from_oracle(
            P0,
            "Rocket Test Rig",
            "{T}: Add {C}.\n\
             {X}, {T}, Sacrifice this artifact: Add X mana in any combination of colors.",
        )
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();

    activate(&mut runner, rig);
    let max = expect_announcement(runner.state(), rig);
    assert_eq!(
        max, 1,
        "CR 605.3c + CR 118.3: only the Mountain can pay; the Rig's own {{T}} is in this cost"
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 1 });
    assert_eq!(expect_any_combination(runner.state()), 1);
    choose_colors(&mut runner, vec![ManaType::Red]);

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(tapped(state, mountain));
    assert_eq!(pool_total(state), 1);
}

/// T6: X is per activation, and two X-announcing sources on one board both
/// list and announce.
#[test]
fn two_rockets_announce_independent_x() {
    let mut scenario = scenario();
    let rockets_a = add_rockets(&mut scenario);
    let rockets_b = add_rockets(&mut scenario);
    let mountains: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let mut runner = scenario.build();
    untap(&mut runner, rockets_a);
    untap(&mut runner, rockets_b);

    activate(&mut runner, rockets_a);
    assert_eq!(expect_announcement(runner.state(), rockets_a), 3);
    act(&mut runner, GameAction::SubmitPayAmount { amount: 2 });
    choose_colors(&mut runner, vec![ManaType::Green, ManaType::Green]);
    assert_eq!(pool_total(runner.state()), 2);

    activate(&mut runner, rockets_b);
    assert_eq!(
        expect_announcement(runner.state(), rockets_b),
        3,
        "a fresh announcement: pool 2 + the last Mountain"
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 3 });
    choose_colors(
        &mut runner,
        vec![ManaType::White, ManaType::Blue, ManaType::Black],
    );

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(pool_total(state), 3);
    assert_eq!(count(state, ManaType::White), 1);
    assert_eq!(count(state, ManaType::Blue), 1);
    assert_eq!(count(state, ManaType::Black), 1);
    assert!(mountains.iter().all(|m| tapped(state, *m)));
    assert_eq!(zone(state, rockets_a), Zone::Graveyard);
    assert_eq!(zone(state, rockets_b), Zone::Graveyard);
}

/// T7: the cap counts a costed source's net yield, and the listing answers on a
/// board with a costed mana source.
#[test]
fn rockets_x_cap_counts_signet_net_yield() {
    let mut scenario = scenario();
    let rockets = add_rockets(&mut scenario);
    let signet = scenario
        .add_artifact_from_oracle(P0, "Azorius Signet", AZORIUS_SIGNET)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    untap(&mut runner, rockets);

    assert!(
        lists_activation(runner.state(), rockets),
        "Rockets is offered at priority"
    );
    activate(&mut runner, rockets);
    assert_eq!(
        expect_announcement(runner.state(), rockets),
        2,
        "the Mountain pays the Signet, which adds two"
    );
    act(&mut runner, GameAction::SubmitPayAmount { amount: 0 });

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(!tapped(state, mountain));
    assert!(!tapped(state, signet));
    assert_eq!(zone(state, rockets), Zone::Graveyard);
}

/// T8: activated during a spell's mana payment, the announcement resumes that
/// payment once the mana is added.
#[test]
fn rockets_activated_inside_spell_mana_payment_returns_to_it() {
    let mut scenario = scenario();
    let rockets = add_rockets(&mut scenario);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let sliver = scenario
        .add_creature_to_hand(P0, "Metallic Sliver", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    untap(&mut runner, rockets);

    let card_id = runner.state().objects[&sliver].card_id;
    act(
        &mut runner,
        GameAction::CastSpell {
            object_id: sliver,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "manual payment opens: {:?}",
        runner.state().waiting_for
    );

    activate(&mut runner, rockets);
    assert_eq!(expect_announcement(runner.state(), rockets), 1);
    match &runner.state().waiting_for {
        WaitingFor::PayAmountChoice {
            pending_mana_ability: Some(pending),
            ..
        } => assert!(
            matches!(pending.resume, ManaAbilityResume::ManaPayment { .. }),
            "the activation resumes the spell's payment: {:?}",
            pending.resume
        ),
        other => panic!("expected the announcement, got {other:?}"),
    }
    act(&mut runner, GameAction::SubmitPayAmount { amount: 1 });
    assert_eq!(expect_any_combination(runner.state()), 1);
    choose_colors(&mut runner, vec![ManaType::Red]);

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
        "back in the spell's payment: {:?}",
        state.waiting_for
    );
    assert_eq!(pool_total(state), 1);
    assert!(tapped(state, mountain));

    act(&mut runner, GameAction::PassPriority);
    let state = runner.state();
    assert_eq!(zone(state, sliver), Zone::Stack, "Metallic Sliver was cast");
    assert_eq!(pool_total(state), 0, "Rockets' mana paid for it");
}

/// T10: a Rockets already chosen for a pending spell's sacrifice cost, whose
/// zone move waits for the spell's mana payment, can't also be sacrificed for
/// its own mana ability (CR 601.2g + CR 118.3): it is not offered, and
/// activating it is refused before X is announced. A second, unreserved
/// Rockets is still offered.
#[test]
fn rockets_reserved_for_a_spell_sacrifice_is_neither_listed_nor_announced() {
    let mut scenario = scenario();
    let reserved = add_rockets(&mut scenario);
    let sibling = add_rockets(&mut scenario);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let curiosity = scenario
        .add_spell_to_hand_from_oracle(P0, "Morbid Curiosity", false, MORBID_CURIOSITY)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    untap(&mut runner, reserved);
    untap(&mut runner, sibling);

    let card_id = runner.state().objects[&curiosity].card_id;
    act(
        &mut runner,
        GameAction::CastSpell {
            object_id: curiosity,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "the sacrifice additional cost is chosen first: {:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![reserved],
        },
    );

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
        "the spell's mana payment opens: {:?}",
        state.waiting_for
    );
    // Reach guard: the reservation is really installed, and the chosen Rockets
    // is still on the battlefield waiting for the payment commit.
    assert!(
        state.pending_cast.as_ref().is_some_and(|pending| pending
            .deferred_sacrificed_permanents
            .iter()
            .any(|selection| selection.object_id == reserved)),
        "Rockets is reserved as the spell's deferred sacrifice"
    );
    assert_eq!(zone(state, reserved), Zone::Battlefield);
    assert!(
        lists_activation(state, sibling),
        "the unreserved Rockets is offered in the payment window"
    );
    assert!(
        !lists_activation(state, reserved),
        "CR 601.2g: the reserved Rockets is not offered"
    );

    let error = runner
        .act(activate_action(runner.state(), reserved))
        .expect_err("one permanent can't pay both sacrifices");
    assert!(
        error
            .to_string()
            .contains("already committed to a spell sacrifice cost"),
        "refused for the reservation: {error}"
    );
    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::PayAmountChoice { .. }),
        "no X announcement for a refused activation"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
        "still in the spell's payment: {:?}",
        state.waiting_for
    );
    assert_eq!(zone(state, reserved), Zone::Battlefield);
    assert!(!tapped(state, reserved));
}
