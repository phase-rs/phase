//! Void Winnower against spells with `{X}` in their mana costs — the announced X
//! decides the spell's mana value, and so its parity.
//!
//! Void Winnower (verbatim Oracle): "Your opponents can't cast spells with even
//! mana values. (Zero is even.) / Your opponents can't block with creatures with
//! even mana values."
//!
//! CR 601.3a: "If an effect prohibits a player from casting a spell with certain
//! qualities, that player may consider any choices to be made during that
//! spell's proposal that may cause those qualities to change." Its own example
//! is this card and Rolling Thunder ({X}{R}{R}): the opponent may BEGIN to cast
//! it because a chosen X may make its mana value odd. CR 202.3e: X is the
//! announced value while the spell is on the stack (a spell being cast is on the
//! stack, CR 601.2a). CR 601.6: a spell its caster may no longer cast once its
//! proposal is complete is an illegal cast.
//!
//! So the two directions the casting pipeline must get right:
//! - Rolling Thunder: X = 0 gives mana value 2 (even), yet the cast may begin,
//!   and an odd announced X (mana value odd) is a legal cast.
//! - Fireball ({X}{R}): X = 0 gives mana value 1 (odd), so the cast begins, but
//!   an announced X that makes the mana value even is not a legal announcement.
//!
//! Every test drives the public action pipeline (`CastSpell` → `ChooseX` →
//! targets → resolution) from a live parse of the printed Void Winnower text.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::types::ability::{CardPlayMode, CastingPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::{Duration, PlayerId};

const VOID_WINNOWER_ORACLE: &str =
    "Your opponents can't cast spells with even mana values. (Zero is even.)\n\
     Your opponents can't block with creatures with even mana values.";
const ROLLING_THUNDER_ORACLE: &str =
    "Rolling Thunder deals X damage divided as you choose among any number of targets.";
const FIREBALL_ORACLE: &str =
    "This spell costs {1} more to cast for each target beyond the first.\n\
     Fireball deals X damage divided evenly, rounded down, among any number of targets.";

/// Which player controls Void Winnower; the caster is always P0.
fn scenario_with_winnower(winnower_controller: PlayerId) -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        winnower_controller,
        "Void Winnower",
        11,
        9,
        VOID_WINNOWER_ORACLE,
    );
    scenario
}

fn red_pool(amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

/// Rolling Thunder ({X}{R}{R}) in P0's hand, with `pool` red mana.
fn rolling_thunder(scenario: &mut GameScenario, pool: usize) -> ObjectId {
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rolling Thunder", false, ROLLING_THUNDER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, red_pool(pool));
    spell
}

/// Fireball ({X}{R}) in P0's hand, with `pool` red mana.
fn fireball(scenario: &mut GameScenario, pool: usize) -> ObjectId {
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fireball", false, FIREBALL_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, red_pool(pool));
    spell
}

fn cast(runner: &mut GameRunner, spell: ObjectId) -> Result<(), engine::game::engine::EngineError> {
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(spell.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|_| ())
}

fn assert_choose_x(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { player, .. } if player == P0
        ),
        "the cast must reach P0's X announcement, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 601.2c-d + CR 601.2h: target P1 alone with the whole X, pay, and resolve.
/// Returns P1's life after resolution.
fn target_p1_and_resolve(runner: &mut GameRunner, spell: ObjectId) -> i32 {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "after a legal X the cast must reach target selection, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("P1 is a legal target");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("finishing the optional target slots should succeed");
    }
    if let WaitingFor::DistributeAmong { total, targets, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::DistributeAmong {
                distribution: targets.into_iter().map(|t| (t, total)).collect(),
            })
            .expect("assigning the whole X to the single target should succeed");
    }
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == spell),
        "the legally announced spell must be cast onto the stack"
    );
    runner.resolve_top();
    assert!(runner.state().stack.is_empty(), "the spell must resolve");
    runner.life(P1)
}

/// CR 601.3a (its own example) + CR 202.3e: Rolling Thunder at X = 0 has mana
/// value 2, so a proposal read at X = 0 refuses it outright. The opponent may
/// begin the cast because an odd mana value is reachable; an even announced X
/// (X = 0 → 2, X = 2 → 4) is refused at the announcement, and X = 1 (mana
/// value 3, odd) is cast and resolves.
#[test]
fn rolling_thunder_odd_x_is_castable_under_void_winnower() {
    let mut scenario = scenario_with_winnower(P1);
    let spell = rolling_thunder(&mut scenario, 5);
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    cast(&mut runner, spell).expect(
        "CR 601.3a: the opponent may begin to cast Rolling Thunder — a chosen X can make its \
         mana value odd",
    );
    assert_choose_x(&runner);

    for even_x in [0, 2] {
        let err = runner
            .act(GameAction::ChooseX { value: even_x })
            .expect_err("an X that makes Rolling Thunder's mana value even must be refused");
        assert!(
            format!("{err:?}").contains("prevents you from casting"),
            "X={even_x} must be refused by the prohibition, not by payability: {err:?}"
        );
        assert_choose_x(&runner);
    }

    runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("X=1 gives mana value 3 (odd), a legal announcement");
    let life_after = target_p1_and_resolve(&mut runner, spell);
    assert_eq!(
        life_after,
        life_before - 1,
        "Rolling Thunder with X=1 deals 1 damage to its target"
    );
}

/// CR 601.6 + CR 202.3e: Fireball at X = 0 has mana value 1 (odd), so the
/// proposal check admits it. X = 1 makes its mana value 2 (even): the announced
/// spell is one Void Winnower forbids, so that announcement is refused. X = 2
/// (mana value 3) is the legal control and resolves for 2.
#[test]
fn fireball_even_resulting_mana_value_is_refused_under_void_winnower() {
    let mut scenario = scenario_with_winnower(P1);
    let spell = fireball(&mut scenario, 5);
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    cast(&mut runner, spell).expect("Fireball at X=0 has odd mana value 1; the cast may begin");
    assert_choose_x(&runner);

    let err = runner
        .act(GameAction::ChooseX { value: 1 })
        .expect_err("X=1 gives Fireball mana value 2 (even) — an illegal cast under Void Winnower");
    assert!(
        format!("{err:?}").contains("prevents you from casting"),
        "X=1 must be refused by the prohibition: {err:?}"
    );
    assert_choose_x(&runner);
    assert!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .is_some_and(|pending| pending.object_id == spell && pending.ability.chosen_x.is_none()),
        "the refused announcement must leave the cast awaiting a legal X, with no X locked in"
    );

    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("X=2 gives mana value 3 (odd), a legal announcement");
    let life_after = target_p1_and_resolve(&mut runner, spell);
    assert_eq!(
        life_after,
        life_before - 2,
        "Fireball with X=2 deals 2 damage to its single target"
    );
}

/// CR 109.5 control: Void Winnower restricts only its controller's opponents, so
/// its controller announces an even-mana-value X freely.
#[test]
fn void_winnower_controller_announces_even_x_freely() {
    let mut scenario = scenario_with_winnower(P0);
    let spell = fireball(&mut scenario, 5);
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    cast(&mut runner, spell).expect("the controller is never restricted");
    assert_choose_x(&runner);
    runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("the controller may announce X=1 (mana value 2)");
    let life_after = target_p1_and_resolve(&mut runner, spell);
    assert_eq!(life_after, life_before - 1);
}

/// CR 601.6 + CR 601.3a: the proposal admits Rolling Thunder because some X
/// could make it odd, but with only {R}{R} available the sole payable X is 0
/// (mana value 2). No announceable X lets the spell be cast, so the begun cast
/// is rewound instead of offering an X prompt with no legal answer.
#[test]
fn rolling_thunder_with_no_payable_odd_x_is_rewound() {
    let mut scenario = scenario_with_winnower(P1);
    let spell = rolling_thunder(&mut scenario, 2);
    let mut runner = scenario.build();

    let err = cast(&mut runner, spell).expect_err("no payable X makes Rolling Thunder odd");
    assert!(
        format!("{err:?}").contains("No value of X"),
        "the cast must be rewound for want of a legal X: {err:?}"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&spell)
            .expect("spell exists")
            .zone,
        Zone::Hand,
        "a rewound cast leaves Rolling Thunder in hand"
    );
    assert!(runner.state().pending_cast.is_none());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the caster keeps priority after the rewind, got {:?}",
        runner.state().waiting_for
    );

    // Reach guard: with one more red mana, X=1 is payable and the same cast
    // reaches the X announcement — the rewind above was the parity bound, not a
    // proposal refusal.
    let mut scenario = scenario_with_winnower(P1);
    let spell = rolling_thunder(&mut scenario, 3);
    let mut runner = scenario.build();
    cast(&mut runner, spell).expect("X=1 is payable with {R}{R}{R}");
    assert_choose_x(&runner);
}

/// Mind Spring ({X}{U}{U}, "Draw X cards.") in P0's exile, castable by P0
/// without paying its mana cost.
fn free_mind_spring(scenario: GameScenario) -> (GameRunner, ObjectId) {
    let mut scenario = scenario;
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mind Spring", false, "Draw X cards.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    zones::move_to_zone(runner.state_mut(), spell, Zone::Exile, &mut Vec::new());
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("spell exists")
        .casting_permissions = vec![CastingPermission::PlayFromExile {
        provenance: engine::types::ability::PlayFromExileProvenance::Impulse,
        duration: Duration::Permanent,
        granted_to: P0,
        mode: CardPlayMode::Cast,
        frequency: CastFrequency::Unlimited,
        source_id: None,
        invalidation: None,
        exiled_by_ability_controller: None,
        mana_spend_permission: None,
        card_filter: None,
        single_use_group: None,
        single_use: false,
        cast_cost_modifier: None,
        alt_ability_cost: None,
        without_paying_mana_cost: true,
        land_enter_tapped: EtbTapState::Unspecified,
    }];
    (runner, spell)
}

/// CR 107.3b + CR 601.6 + CR 202.3e: cast without paying its mana cost, Mind
/// Spring's only legal X is 0, so it is a mana-value-2 (even) spell. The
/// proposal admits it (it considers X choices, CR 601.3a), so the announced
/// spell must be re-checked before any cost is paid: the cast is cancelled and
/// Mind Spring never becomes cast. The same free cast with Void Winnower on the
/// caster's own side is the reach guard that the free route itself works.
#[test]
fn x_spell_cast_without_paying_its_mana_cost_is_refused_when_x_zero_is_even() {
    let (mut runner, spell) = free_mind_spring(scenario_with_winnower(P1));
    let err = cast(&mut runner, spell).expect_err(
        "a free Mind Spring (X = 0, mana value 2) is an illegal cast under Void Winnower",
    );
    assert!(
        format!("{err:?}").contains("with the announced X"),
        "the announced spell must be refused by the pre-payment recheck: {err:?}"
    );
    assert!(
        runner.state().stack.iter().all(|entry| entry.id != spell),
        "the refused spell must never be cast"
    );
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(
        runner
            .state()
            .objects
            .get(&spell)
            .expect("spell exists")
            .zone,
        Zone::Exile,
        "the rewound cast leaves Mind Spring in exile"
    );

    // Reach guard: Void Winnower's controller casts the same free spell.
    let (mut runner, spell) = free_mind_spring(scenario_with_winnower(P0));
    cast(&mut runner, spell).expect("the controller's free Mind Spring is legal");
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == spell),
        "the free cast route puts Mind Spring on the stack"
    );
}
