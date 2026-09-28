//! Paying life down to 0 while casting a spell must not end the game mid-cast.
//!
//! CR 119.4: paying life equal to your life total is a legal payment. CR 104.3b:
//! a player at 0 or less life loses "the next time a player would receive
//! priority", and CR 704.3 checks state-based actions only then. No player
//! receives priority while a spell is being cast (CR 601.2h) or an ability
//! activated (CR 602.2b), so the caster finishes the cast and only then loses.
//! The same holds while a mana ability is being activated (CR 605.3b), with
//! priority or mid-cast: its color choice stays open at 0 life.
//!
//! The Platinum Angel and Yawgmoth tests are controls: the "can't lose"
//! exception was already honoured, and an activation's life cost is paid as
//! its last cost component, in the same action that puts it on the stack.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    PlayerFilter, QuantityExpr, QuantityModification, ReplacementDefinition, TargetFilter,
    TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const FUMAROLE: &str =
    "As an additional cost to cast this spell, pay 3 life.\nDestroy target creature and target land.";
const TOXIC_DELUGE: &str = "As an additional cost to cast this spell, pay X life.\nAll creatures get -X/-X until end of turn.";
const MANA_CONFLUENCE: &str = "{T}, Pay 1 life: Add one mana of any color.";
const PLATINUM_ANGEL: &str =
    "Flying\nYou can't lose the game and your opponents can't win the game.";
const YAWGMOTH: &str = "Protection from Humans\nPay 1 life, Sacrifice another creature: Put a -1/-1 counter on up to one target creature and draw a card.\n{B}{B}, Discard a card: Proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)";

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn eliminated(runner: &GameRunner) -> bool {
    runner.state().players[P0.0 as usize].is_eliminated
}

fn is_mana_payment(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. })
}

fn p1_won(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(winner) } if winner == P1
    )
}

struct DelugeBoard {
    runner: GameRunner,
    deluge: ObjectId,
    victim: ObjectId,
    confluence: ObjectId,
}

/// Announces Toxic Deluge with X = `x`, paying the X life as the additional
/// cost. Manual payment keeps the {2}{B} window open afterwards.
fn cast_toxic_deluge(
    life: i32,
    x: u32,
    floating: &[ManaType],
    platinum_angel: bool,
) -> DelugeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, life);
    let deluge = scenario
        .add_spell_to_hand_from_oracle(P0, "Toxic Deluge", false, TOXIC_DELUGE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 2,
        })
        .id();
    let confluence = scenario
        .add_land_from_oracle(P0, "Mana Confluence", MANA_CONFLUENCE)
        .id();
    if platinum_angel {
        scenario.add_creature_from_oracle(P0, "Platinum Angel", 4, 4, PLATINUM_ANGEL);
    }
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.with_mana_pool(P0, pool(floating));
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&deluge].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: deluge,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce Toxic Deluge");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "Toxic Deluge announces X first, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseX { value: x })
        .expect("announce X, paying X life");
    DelugeBoard {
        runner,
        deluge,
        victim,
        confluence,
    }
}

fn spell_cast(events: &[GameEvent], spell: ObjectId) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
}

fn pays_life(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::PayLife { .. } => true,
        AbilityCost::Composite { costs } => costs.iter().any(pays_life),
        _ => false,
    }
}

#[test]
fn spell_paid_down_to_zero_life_is_cast_before_the_player_loses() {
    let DelugeBoard {
        mut runner,
        deluge,
        victim,
        ..
    } = cast_toxic_deluge(
        3,
        3,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Black],
        false,
    );

    // CR 601.2h: Toxic Deluge is still being cast; its {2}{B} is owed.
    assert_eq!(runner.life(P0), 0);
    assert!(
        is_mana_payment(&runner),
        "Toxic Deluge must still be paying its mana cost, got {:?}",
        runner.state().waiting_for
    );

    let events = runner
        .act(GameAction::PassPriority)
        .expect("finish paying {2}{B} from the pool")
        .events;

    // CR 601.2i: the spell became cast...
    assert!(
        spell_cast(&events, deluge),
        "Toxic Deluge must become cast: {events:?}"
    );
    // ...then CR 104.3b + CR 704.5a: P0 loses before it can resolve.
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
}

#[test]
fn mana_ability_paying_the_last_life_mid_cast_does_not_end_the_game_yet() {
    let DelugeBoard {
        mut runner,
        deluge,
        confluence,
        ..
    } = cast_toxic_deluge(4, 3, &[ManaType::Colorless, ManaType::Colorless], false);
    assert_eq!(runner.life(P0), 1);
    assert!(
        is_mana_payment(&runner),
        "Toxic Deluge must be paying its mana cost, got {:?}",
        runner.state().waiting_for
    );

    // CR 117.1d: mana abilities may be activated while paying a spell's cost.
    // Tap Confluence for the {B} the pool is missing, paying the last life.
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&confluence)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } if selection.mana_type == ManaType::Black => {
                Some(selection.clone())
            }
            _ => None,
        })
        .expect("Mana Confluence's {B} is offered mid-cast");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap Mana Confluence for {B}");

    // CR 601.2h: still casting; the life payment ended no one's game.
    assert_eq!(runner.life(P0), 0);
    assert!(
        is_mana_payment(&runner),
        "Toxic Deluge must still be paying its mana cost, got {:?}",
        runner.state().waiting_for
    );
    let events = runner
        .act(GameAction::PassPriority)
        .expect("finish paying {2}{B}")
        .events;

    // CR 601.2i, then CR 104.3b + CR 704.5a.
    assert!(
        spell_cast(&events, deluge),
        "Toxic Deluge must become cast: {events:?}"
    );
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
}

/// CR 605.3b: Mana Confluence activated as an ability (not a pre-selected tap)
/// pays its life before asking which color to add. That color choice is part
/// of activating the mana ability, so no one has priority yet.
fn activate_confluence_paying_last_life(runner: &mut GameRunner, confluence: ObjectId) {
    assert_eq!(runner.life(P0), 1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: confluence,
            ability_index: 0,
        })
        .expect("activate Mana Confluence");
    assert_eq!(runner.life(P0), 0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { player, .. } if player == P0
        ),
        "the color choice must still be open to P0, got {:?}",
        runner.state().waiting_for
    );
}

fn choose_black(runner: &mut GameRunner) -> Vec<GameEvent> {
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Black),
            count: 1,
        })
        .expect("choose black")
        .events
}

fn black_mana_added(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::ManaAdded {
                mana_type: ManaType::Black,
                ..
            }
        )
    })
}

#[test]
fn mana_ability_color_choice_after_the_last_life_mid_cast_stays_open() {
    let DelugeBoard {
        mut runner,
        deluge,
        confluence,
        ..
    } = cast_toxic_deluge(4, 3, &[ManaType::Colorless, ManaType::Colorless], false);
    assert!(is_mana_payment(&runner));

    // CR 117.1d: activated while paying Toxic Deluge's {2}{B}.
    activate_confluence_paying_last_life(&mut runner, confluence);
    let mut events = choose_black(&mut runner);
    assert!(black_mana_added(&events), "{events:?}");
    if is_mana_payment(&runner) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("finish paying {2}{B}")
                .events,
        );
    }

    // CR 601.2i, then CR 104.3b + CR 704.5a.
    assert!(
        spell_cast(&events, deluge),
        "Toxic Deluge must become cast: {events:?}"
    );
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
}

#[test]
fn mana_ability_color_choice_after_the_last_life_at_priority_stays_open() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 1);
    let confluence = scenario
        .add_land_from_oracle(P0, "Mana Confluence", MANA_CONFLUENCE)
        .id();
    let mut runner = scenario.build();

    activate_confluence_paying_last_life(&mut runner, confluence);
    let events = choose_black(&mut runner);

    // CR 605.3b: the mana ability finishes and adds its mana; then
    // CR 104.3b + CR 704.5a end the game as P0 would receive priority.
    assert!(black_mana_added(&events), "{events:?}");
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
}

#[test]
fn cant_lose_player_keeps_playing_after_paying_down_to_zero_life() {
    let DelugeBoard {
        mut runner, deluge, ..
    } = cast_toxic_deluge(
        3,
        3,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Black],
        true,
    );
    assert!(is_mana_payment(&runner));
    let events = runner
        .act(GameAction::PassPriority)
        .expect("finish paying {2}{B} from the pool")
        .events;
    assert!(spell_cast(&events, deluge));

    // CR 101.2 + CR 704.5a: Platinum Angel's "can't lose" beats the 0-life SBA,
    // so the priority check after the cast eliminates no one.
    assert_eq!(runner.life(P0), 0);
    assert!(!eliminated(&runner));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&deluge].zone, Zone::Stack);
}

#[test]
fn activation_paid_down_to_zero_life_goes_on_the_stack_before_the_player_loses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 1);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    scenario.add_creature(P0, "Other Fodder", 1, 1);
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.add_card_to_library_top(P0, "Yawgmoth Draw");
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&yawgmoth]
        .abilities
        .iter()
        .position(|ability| ability.cost.as_ref().is_some_and(pays_life))
        .expect("Yawgmoth's pay-life ability parses");
    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("activate Yawgmoth");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(victim)],
        })
        .expect("target the Bears");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));
    assert_eq!(runner.life(P0), 1);
    let events = runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("sacrifice the Fodder, completing the cost")
        .events;

    // CR 602.2b + CR 601.2i: the ability was activated and put on the stack...
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::StackPushed { .. })),
        "Yawgmoth's ability must reach the stack: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == fodder
        )),
        "the sacrifice cost was paid: {events:?}"
    );
    // ...then CR 104.3b + CR 704.5a: P0 loses as they would receive priority,
    // before the ability can resolve.
    assert_eq!(runner.life(P0), 0);
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
}

/// A `LoseLife` replacement that keeps the payment's amount but then runs an
/// interactive "gain 1 life" choice (CR 616.1). The life is already paid when
/// that choice opens.
fn life_loss_rider() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Plus { value: 0 })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )],
            },
        ))
}

/// The rider's choice opens with P0 at 0 life, still paying the cost.
fn assert_rider_open_at_zero_life(runner: &GameRunner) {
    assert_eq!(runner.life(P0), 0);
    assert!(!eliminated(runner));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { player, .. } if player == P0
        ),
        "the replacement's choice must stay open to P0, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn replacement_choice_after_a_mana_ability_paid_the_last_life_stays_open() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 1);
    let confluence = scenario
        .add_land_from_oracle(P0, "Mana Confluence", MANA_CONFLUENCE)
        .id();
    scenario
        .add_creature(P0, "Life-Loss Rider", 0, 1)
        .with_replacement_definition(life_loss_rider());
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: confluence,
            ability_index: 0,
        })
        .expect("activate Mana Confluence");
    // CR 605.3b + CR 616.1: the replacement's choice is part of paying the mana
    // ability's cost, so no one has priority yet.
    assert_rider_open_at_zero_life(&runner);

    // CR 118.3b + CR 119.4: the life was paid when the rider's choice opened;
    // answering it gains 1 life and the activation continues without paying
    // (or asking) again.
    let mut events = runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("gain 1 life")
        .events;
    assert_eq!(runner.life(P0), 1);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { player, .. } if player == P0
        ),
        "the rider must not reopen; the activation asks for its color next, got {:?}",
        runner.state().waiting_for
    );
    events.extend(choose_black(&mut runner));

    // CR 605.3b: the mana ability resolves and adds its mana.
    assert!(black_mana_added(&events), "{events:?}");
    assert_eq!(runner.life(P0), 1);
    assert!(!eliminated(&runner));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn mana_ability_life_cost_awaiting_replacement_ordering_is_paid_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 10);
    let confluence = scenario
        .add_land_from_oracle(P0, "Mana Confluence", MANA_CONFLUENCE)
        .id();
    scenario
        .add_creature(P0, "Life-Loss Modifiers", 0, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::DOUBLE)
                .description("Double".to_string()),
        )
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::Plus { value: 1 })
                .description("Plus one".to_string()),
        );
    let mut runner = scenario.build();

    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: confluence,
            ability_index: 0,
        })
        .expect("activate Mana Confluence")
        .events;
    // CR 616.1: the life payment waits for its replacements to be ordered;
    // nothing is deducted yet.
    assert_eq!(runner.life(P0), 10);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("order the replacements")
            .events,
    );
    if matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ) {
        events.extend(choose_black(&mut runner));
    }

    // The ordered payment is delivered once, and the activation finishes.
    let losses: Vec<i32> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged {
                player_id, amount, ..
            } if *player_id == P0 && *amount < 0 => Some(*amount),
            _ => None,
        })
        .collect();
    assert_eq!(
        losses.len(),
        1,
        "the life cost is paid exactly once: {losses:?}"
    );
    assert_eq!(runner.life(P0), 10 + losses[0]);
    assert!(black_mana_added(&events), "{events:?}");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
}

/// A synthetic mana ability whose own cost includes a Phyrexian mana sub-cost:
/// `{1}{B/P}, {T}: Add {G}`, with a Plains to fund the {1} (so the sub-cost is
/// paid by auto-tapping another source). No printed card is verified for this
/// exact cost; the engine accepts the shape, so it is pinned by a fixture.
fn phyrexian_costed_mana_source(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Phyrexian-Costed Mana Witness", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::PhyrexianBlack],
                            generic: 1,
                        },
                    },
                    AbilityCost::Tap,
                ],
            }),
        )
        .id()
}

#[test]
fn phyrexian_life_in_a_mana_ability_sub_cost_is_paid_once_around_a_replacement_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 10);
    let source = phyrexian_costed_mana_source(&mut scenario);
    scenario
        .add_creature(P0, "Life-Loss Rider", 0, 1)
        .with_replacement_definition(life_loss_rider());
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            matches!(ability.cost, Some(AbilityCost::Composite { .. }))
                && matches!(*ability.effect, Effect::Mana { .. })
        })
        .expect("the witness has its mana ability");

    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("activate the witness")
        .events;
    // CR 107.4f + CR 118.3b: a Plains is tapped for {1} and {B/P} is paid with
    // 2 life; the rider's choice is still part of paying the cost.
    assert_eq!(
        runner.life(P0),
        8,
        "waiting {:?} events {events:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { player, .. } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    events.extend(
        runner
            .act(GameAction::ChooseBranch { index: 0 })
            .expect("gain 1 life; the activation finishes without paying again")
            .events,
    );

    // One mana spend (the Plains' {W} for {1}) and one life payment, then the
    // ability adds its {G}.
    assert_eq!(runner.life(P0), 9);
    let pool_after = &runner.state().players[P0.0 as usize].mana_pool;
    assert_eq!(pool_after.count_color(ManaType::White), 0);
    assert_eq!(pool_after.count_color(ManaType::Green), 1);
    let losses = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::LifeChanged { player_id, amount, .. } if *player_id == P0 && *amount < 0
            )
        })
        .count();
    assert_eq!(losses, 1, "{events:?}");
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ManaAdded {
                mana_type: ManaType::Green,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn replacement_choice_after_a_spell_cost_paid_the_last_life_stays_open() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 3);
    let fumarole = scenario
        .add_spell_to_hand_from_oracle(P0, "Fumarole", false, FUMAROLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Red],
            generic: 3,
        })
        .id();
    scenario
        .add_creature(P0, "Life-Loss Rider", 0, 1)
        .with_replacement_definition(life_loss_rider());
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let land = scenario.add_basic_land(P1, ManaColor::Green);
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Red,
        ]),
    );
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&fumarole].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fumarole,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce Fumarole");
    // CR 601.2c: one target per slot, in written order.
    for target in [bears, land] {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ),
            "got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("choose the target");
    }
    // CR 601.2h + CR 616.1: the 3 life is paid, and the replacement's choice is
    // still part of paying Fumarole's cost.
    assert_rider_open_at_zero_life(&runner);
    let events = runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("gain 1 life")
        .events;
    assert_eq!(runner.life(P0), 1);

    assert!(
        spell_cast(&events, fumarole),
        "Fumarole must become cast: {events:?}"
    );
    assert!(!eliminated(&runner));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
}

/// A `Moved` replacement sending a card headed to exile to the graveyard.
/// Two of them compete, so CR 616.1 asks the affected card's controller to
/// order them.
fn redirect_exile_to_graveyard() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Exile)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Graveyard,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

#[test]
fn foretell_paid_with_the_last_life_finishes_before_the_player_loses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 1);
    let foretold = scenario
        .add_spell_to_hand(P0, "Foretell Witness", false)
        .with_mana_cost(ManaCost::generic(5))
        .with_keyword(Keyword::Foretell(ManaCost::generic(3)))
        .id();
    for name in ["First Exile Redirect", "Second Exile Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_exile_to_graveyard());
    }
    scenario
        .add_land_from_oracle(P0, "Mana Confluence", MANA_CONFLUENCE)
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&foretold].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: foretold,
            card_id,
        })
        .expect("foretell pays {2}, the second with Mana Confluence's life");

    // CR 116.2h: foretelling is a special action; the exile move's CR 616.1
    // ordering choice is part of it, so no one has priority yet.
    assert_eq!(runner.life(P0), 0);
    assert!(!eliminated(&runner));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "the exile move's replacement choice must stay open to P0, got {:?}",
        runner.state().waiting_for
    );

    let events = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("order the redirects")
        .events;

    // The special action completes (the card is redirected to the graveyard,
    // so it is not foretold), then CR 104.3b + CR 704.5a end the game.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Graveyard,
                ..
            } if *object_id == foretold
        )),
        "the foretell move must finish before the loss: {events:?}"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(!runner.state().objects[&foretold].foretold);
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
}
