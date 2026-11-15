//! Molten Tide (FRA Instant {R}): "Until end of turn, whenever you tap a
//! Mountain for mana, add an additional {R}."
//!
//! Runtime proof that the "you tap … for mana" delayed-trigger actor only
//! fires when the trigger source's controller taps their own Mountain, pays
//! the bonus into the tapper's (= controller's) pool, fires repeatedly, and
//! expires at end of turn.

use engine::game::combat::{
    attack_tax_is_affordable, pending_combat_tax_is_affordable, AttackTarget,
};
use engine::game::mana_abilities::activate_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::{check_delayed_triggers, process_triggers};
use engine::game::turns::execute_cleanup;
use engine::parser::oracle_static::parse_static_line;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, ManaTapState};
use engine::types::game_state::{
    CastPaymentMode, CombatTaxContext, ManaAbilityResume, ManaChoice, ManaChoicePrompt,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const MOLTEN_TIDE_ORACLE: &str =
    "Until end of turn, whenever you tap a Mountain for mana, add an additional {R}.";
const SELESNYA_ORACLE: &str = "If an effect would create one or more tokens, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent, it puts twice that many of those counters on that permanent instead.\nWhenever chaos ensues, until end of turn, whenever you tap a land for mana, add one mana of any type that land produced.";
const CHAMBER_ORACLE: &str =
    "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {G}{U}.";
const MANABARBS_ORACLE: &str =
    "Whenever a player taps a land for mana, this enchantment deals 1 damage to that player.";

fn red(runner: &GameRunner, p: PlayerId) -> usize {
    runner.state().players[p.0 as usize]
        .mana_pool
        .count_color(ManaType::Red)
}

fn clear_pools(runner: &mut GameRunner) {
    for p in [P0, P1] {
        runner.state_mut().players[p.0 as usize].mana_pool.clear();
    }
}

/// Deposit one base {R} into `p`'s pool, simulating the land's own production
/// (CR 106.12a) ahead of a synthetic `TappedForMana` boundary input. The
/// delayed trigger under test only ever adds the BONUS, so each boundary row
/// asserts base + 0-or-1 and the base deposit keeps a no-fire row honest: a
/// pool that stays at exactly 1 proves the matcher declined, not that the
/// fixture never produced.
fn deposit_base_red(runner: &mut GameRunner, p: PlayerId, source: ObjectId) {
    let _ = runner
        .state_mut()
        .add_mana_to_pool(p, ManaUnit::new(ManaType::Red, source, false, vec![]));
}

#[test]
fn molten_tide_bonus_only_for_controller_taps_own_mountain_then_expires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tide = scenario
        .add_spell_to_hand_from_oracle(P0, "Molten Tide", true, MOLTEN_TIDE_ORACLE)
        .id();
    let mtn0 = scenario.add_basic_land(P0, ManaColor::Red);
    let mtn1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mtn2 = scenario.add_basic_land(P0, ManaColor::Red);
    let mtn_opp = scenario.add_basic_land(P1, ManaColor::Red);
    // Fund P0's {R} to cast Molten Tide through the real pipeline (registers
    // the delayed trigger authentically).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(9999), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.cast(tide).resolve();

    // Positive reach-guard FIRST: the instant registered exactly one
    // until-end-of-turn delayed trigger on resolution.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Molten Tide must register its delayed trigger on resolution"
    );
    clear_pools(&mut runner);

    // (1) P0 taps its own Mountain for mana. The activation resolves the
    // TapsForMana bonus inline (tap_state FromTapTriggersResolved), so the
    // land's base {R} plus the trigger's additional {R} land in P0's pool
    // (CR 106.4: the subjectless body deposits into the controller's pool;
    // CR 106.12a: the tap event). `process_triggers` over the same batch is
    // a no-op second pass (resolved marker), mirroring the
    // issue_6979_land_mana_amplification driving — it must NOT add again.
    let ability = runner.state().objects[&mtn0].abilities[0].clone();
    let mut events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        mtn0,
        P0,
        0,
        &ability,
        &mut events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("P0 taps its own Mountain for mana");
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        red(&runner, P0),
        2,
        "P0 taps its own Mountain: base {{R}} + additional {{R}} = 2 red"
    );
    assert_eq!(red(&runner, P1), 0, "P1 gets nothing when P0 taps");

    // (2) Multi-fire: P0 taps a second Mountain the same turn for another +1.
    let ability = runner.state().objects[&mtn1].abilities[0].clone();
    let mut events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        mtn1,
        P0,
        0,
        &ability,
        &mut events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("P0 taps a second Mountain for mana");
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        red(&runner, P0),
        4,
        "second Mountain the same turn fires again: 2 + base {{R}} + additional {{R}} = 4 red"
    );

    // (3) SAME-PATH POSITIVE via the delayed-matcher API: an otherwise
    // identical P0-taps-P0's-Mountain event through `check_delayed_triggers`
    // (the Any-scope delayed matcher) must yield base + bonus. This is the
    // reach guard for rows (4)-(5): it proves synthetic `TappedForMana`
    // input reaches Molten Tide's delayed matcher at all, so a hostile row
    // that stays at base is a true matcher decline, not a delivery bypass
    // (e.g. the trigger never registered, or the event never scanned).
    clear_pools(&mut runner);
    deposit_base_red(&mut runner, P0, mtn0);
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: P0,
            source_id: mtn0,
            produced: vec![ManaType::Red],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        red(&runner, P0),
        2,
        "same-path positive: P0 taps P0's Mountain via check_delayed_triggers = base {{R}} + bonus {{R}}"
    );
    assert_eq!(red(&runner, P1), 0, "P1 gets nothing when P0 taps");

    // (4) HOSTILE F1 (matcher boundary): P1 taps a Mountain CONTROLLED BY
    // P0. The tapper is not the trigger source's controller
    // (valid_target = Controller), so the matcher must decline and NEITHER
    // pool gains beyond the simulated base. Boundary label: CR 602.2 lets
    // only a land's controller activate its ability, so no legal activation
    // produces this event pair — it probes the matcher filter directly,
    // while CR 106.12a fixes the event shape a real tap would emit.
    // Revert check: deleting the `valid_target = Controller` gate flips the
    // P0 assert below from 0 to 1 (bonus misfires to the controller, which
    // holds no base here).
    clear_pools(&mut runner);
    deposit_base_red(&mut runner, P1, mtn0);
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: P1,
            source_id: mtn0,
            produced: vec![ManaType::Red],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        red(&runner, P0),
        0,
        "no bonus in P0's pool when P1 taps P0's Mountain"
    );
    assert_eq!(
        red(&runner, P1),
        1,
        "P1 keeps exactly the simulated base — no bonus misfires and none misroutes"
    );

    // (5) HOSTILE F2 (matcher boundary): P0 taps an OPPONENT-controlled
    // Mountain. The source is not You-controlled (valid_card controller
    // gate), so the matcher must decline. Same CR 602.2 / CR 106.12a
    // boundary label as F1.
    // Revert check: deleting `add_controller(filter, You)` flips the P0
    // assert below from 1 to 2.
    clear_pools(&mut runner);
    deposit_base_red(&mut runner, P0, mtn_opp);
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: P0,
            source_id: mtn_opp,
            produced: vec![ManaType::Red],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        red(&runner, P0),
        1,
        "no bonus when P0 taps an opponent-controlled Mountain — only the simulated base remains"
    );
    assert_eq!(red(&runner, P1), 0, "P1 gains nothing either");

    // (6) Expiry: end-of-turn cleanup purges the multi-fire delayed trigger
    // (CR 603.7b); a later Mountain tap yields only the base {R}.
    let mut cleanup_events = Vec::new();
    execute_cleanup(runner.state_mut(), &mut cleanup_events);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the until-end-of-turn delayed trigger must be purged at cleanup"
    );
    clear_pools(&mut runner);
    let ability = runner.state().objects[&mtn2].abilities[0].clone();
    let mut events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        mtn2,
        P0,
        0,
        &ability,
        &mut events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("P0 taps a Mountain after cleanup");
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        red(&runner, P0),
        1,
        "after cleanup: only the base {{R}}, no bonus — the delayed trigger expired"
    );
}

/// CR 605.4a + CR 608.2d: Molten Tide's fixed additional {R} owes no type
/// choice, so the affordability preview must count it before deciding whether
/// an auto-payment is possible. One Mountain's base {R} alone cannot pay
/// Goblin Piker's {1}{R}; base {R} + the delayed {R} can.
#[test]
fn molten_tide_fixed_bonus_funds_auto_payment_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tide = scenario
        .add_spell_to_hand_from_oracle(P0, "Molten Tide", true, MOLTEN_TIDE_ORACLE)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    // Fund Molten Tide itself without consuming the Mountain under test.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(9999), false, vec![])],
    );
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 1,
    };
    let piker = scenario
        .add_creature_to_hand(P0, "Goblin Piker", 2, 1)
        .with_mana_cost(cost.clone())
        .id();
    let mut runner = scenario.build();
    runner.cast(tide).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Molten Tide must install its delayed trigger"
    );
    clear_pools(&mut runner);

    // The regression: one Mountain's base {R} is insufficient for {1}{R}; the
    // preview must count the fixed delayed {R} without a type choice.
    assert!(
        engine::game::casting::can_pay_cost_after_auto_tap(runner.state(), P0, piker, &cost),
        "Molten Tide's fixed {{R}} bonus must make Goblin Piker castable from one Mountain"
    );

    let piker_card = runner.state().objects[&piker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: piker,
            card_id: piker_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the fixed delayed bonus must fund the auto-payment");
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::Spell { .. })),
        "the paid Goblin Piker must reach the stack"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the fixed delayed bonus owes no choice; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        red(&runner, P0),
        0,
        "base {{R}} + bonus {{R}} exactly pays {{1}}{{R}}"
    );
    assert!(
        runner.state().objects[&mountain].tapped,
        "the Mountain paid the cost"
    );
}

/// Ghostly Prison's combat-tax static, carried by a 2/2 creature shell so the
/// fixture has a live permanent. Only its tax behavior is under test.
fn add_ghostly_prison(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let def = parse_static_line(
        "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
    )
    .expect("Ghostly Prison should parse");
    let mut builder = scenario.add_creature(player, "Ghostly Prison", 2, 2);
    builder.with_static_definition(def);
    builder.id()
}

/// Archangel of Tithes' verified block-tax static, carried by an attacking
/// creature shell. The shell stays a plain ground creature so the defending
/// player's blocker can legally block it; only the tax scoping (the defender
/// pays per blocking creature) is under test.
fn add_block_tax_raider(scenario: &mut GameScenario) -> ObjectId {
    let block_tax = parse_static_line(
        "As long as this creature is attacking, creatures can't block unless their controller pays {1} for each of those creatures.",
    )
    .expect("Archangel of Tithes' block-tax static should parse");
    let mut builder = scenario.add_creature(P0, "Taxing Raider", 3, 3);
    builder.with_static_definition(block_tax);
    builder.id()
}

struct CombatTaxFixture {
    runner: GameRunner,
    attacker: ObjectId,
    mountains: Vec<ObjectId>,
}

/// One or more untapped Mountains, one attacker, and Ghostly Prison's {2} tax.
/// The `with_tide` variant resolves Molten Tide through the real cast pipeline
/// (registering its delayed trigger) from a synthetic floating {R}, then empties
/// every pool, so the only mana left is the Mountains and their delayed bonuses.
fn combat_tax_fixture(with_tide: bool) -> CombatTaxFixture {
    combat_tax_fixture_with_mountains(with_tide, 1)
}

fn combat_tax_fixture_with_mountains(with_tide: bool, mountain_count: usize) -> CombatTaxFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tide = with_tide.then(|| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Molten Tide", true, MOLTEN_TIDE_ORACLE)
            .id()
    });
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mountains: Vec<ObjectId> = (0..mountain_count)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    add_ghostly_prison(&mut scenario, P1);
    if with_tide {
        // Fund Molten Tide itself without consuming the Mountains under test.
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(9999), false, vec![])],
        );
    }
    let mut runner = scenario.build();
    if let Some(tide) = tide {
        runner.cast(tide).resolve();
        assert_eq!(
            runner.state().delayed_triggers.len(),
            1,
            "Molten Tide must install its delayed trigger"
        );
        clear_pools(&mut runner);
    }
    CombatTaxFixture {
        runner,
        attacker,
        mountains,
    }
}

/// CR 508.1d + CR 508.1i-j + CR 605.4a + CR 608.2d: Molten Tide's mandatory
/// fixed {R} owes no type choice, so the rootless combat-tax payment boundary
/// (`PayCombatTax` -> `pay_unless_cost`, which cannot suspend) must settle it
/// before the affordability check. One Mountain's base {R} plus the delayed {R}
/// pays Ghostly Prison's {2}; accepting the tax commits the attack and the
/// declaration continuation reaches its terminal priority state.
#[test]
fn molten_tide_fixed_bonus_funds_rootless_combat_tax_and_attack_commits() {
    let CombatTaxFixture {
        mut runner,
        attacker,
        mountains,
    } = combat_tax_fixture(true);
    let mountain = mountains[0];
    let attacks = vec![(attacker, AttackTarget::Player(P1))];

    // The preview the declaration solver consults must already see the bonus.
    assert!(
        attack_tax_is_affordable(runner.state(), &attacks),
        "one Mountain's base {{R}} plus Molten Tide's delayed {{R}} must cover the {{2}} tax"
    );

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attacks.clone(),
            bands: vec![],
        })
        .expect("the affordable taxed proposal must open CombatTaxPayment");

    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment {
            player,
            context,
            total_cost,
            per_creature,
            ..
        } => {
            assert_eq!(*player, P0, "the active player owes the tax");
            assert!(matches!(context, CombatTaxContext::Attacking));
            assert_eq!(total_cost.mana_value(), 2);
            assert_eq!(per_creature.len(), 1);
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }
    assert!(
        pending_combat_tax_is_affordable(runner.state()),
        "the live tax prompt's own affordability probe must count the delayed bonus"
    );

    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("the delayed {R} must fund the rootless combat-tax payment");

    let state = runner.state();
    let combat = state
        .combat
        .as_ref()
        .expect("combat state must exist once the tax is paid");
    assert_eq!(combat.attackers.len(), 1, "the attack must commit");
    assert!(
        state.objects[&mountain].tapped,
        "the Mountain (not a pre-floated pool) paid the tax"
    );
    assert_eq!(
        red(&runner, P0),
        0,
        "base {{R}} + bonus {{R}} exactly pays {{2}}"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "Molten Tide's multi-fire delayed trigger stays installed"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the declaration continuation must reach its terminal priority state; got {:?}",
        state.waiting_for
    );
}

/// CR 605.4a + CR 508.1i-j: when the base pool already covers the cost, the
/// mandatory delayed bonus still resolves during the payment. Two Mountains
/// produce {R}{R} for the {2} tax and each tap triggers Tide, so the pool must
/// carry both surplus {R} after the tax is paid. On the `PayCombatTax` path the
/// post-action scan retains only `SpellCast` events, so a skipped settlement
/// would drop the bonus entirely rather than deliver it at priority.
#[test]
fn molten_tide_bonus_settles_when_two_mountains_already_cover_the_combat_tax() {
    let CombatTaxFixture {
        mut runner,
        attacker,
        mountains,
    } = combat_tax_fixture_with_mountains(true, 2);
    let attacks = vec![(attacker, AttackTarget::Player(P1))];

    assert!(
        attack_tax_is_affordable(runner.state(), &attacks),
        "two Mountains cover the tax before any bonus is considered"
    );

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("the affordable taxed proposal must open CombatTaxPayment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CombatTaxPayment { .. }
    ));
    assert!(pending_combat_tax_is_affordable(runner.state()));

    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("the covered tax must still settle each mandatory delayed {R}");

    let state = runner.state();
    assert_eq!(
        state
            .combat
            .as_ref()
            .expect("combat state must exist once the tax is paid")
            .attackers
            .len(),
        1,
        "the attack must commit"
    );
    assert!(
        mountains
            .iter()
            .all(|mountain| state.objects[mountain].tapped),
        "both Mountains produced the base {{R}}{{R}}"
    );
    assert_eq!(
        red(&runner, P0),
        2,
        "base {{R}}{{R}} + two delayed {{R}} minus the {{2}} tax"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "Molten Tide's multi-fire delayed trigger stays installed"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the declaration continuation must reach its terminal priority state; got {:?}",
        state.waiting_for
    );
}

/// CR 508.1d: the same-state control. Without Molten Tide the Mountain's base
/// {R} cannot cover Ghostly Prison's {2}: the declaration preview reports the
/// tax unaffordable, and accepting the prompt fails before anything is tapped or
/// committed.
#[test]
fn mountain_alone_cannot_pay_the_same_combat_tax_without_molten_tide() {
    let CombatTaxFixture {
        mut runner,
        attacker,
        mountains,
    } = combat_tax_fixture(false);
    let mountain = mountains[0];
    let attacks = vec![(attacker, AttackTarget::Player(P1))];

    assert!(
        !attack_tax_is_affordable(runner.state(), &attacks),
        "one Mountain's base {{R}} alone cannot cover the {{2}} tax"
    );

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("a human taxed declaration still opens the tax prompt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CombatTaxPayment { .. }
    ));
    assert!(
        !pending_combat_tax_is_affordable(runner.state()),
        "the control prompt must stay unaffordable"
    );

    let error = runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect_err("accepting an unaffordable tax must fail");
    assert!(
        format!("{error:?}").contains("Cannot pay mana cost"),
        "unexpected rejection: {error:?}"
    );
    let state = runner.state();
    assert!(
        state
            .combat
            .as_ref()
            .is_none_or(|combat| combat.attackers.is_empty()),
        "a failed tax payment must not commit an attack"
    );
    assert!(
        !state.objects[&mountain].tapped,
        "a failed tax payment must not tap the Mountain"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "the failed payment must leave the tax prompt intact; got {:?}",
        state.waiting_for
    );
}

/// CR 605.4a + CR 608.2d + CR 508.1i-j: one payment frame can mix a
/// choice-free delayed bonus (Molten Tide on each Mountain tap) with a
/// choice-owing echo (Selesnya Loft Gardens on the Chamber's {G}{U} tap). The
/// choice-free firings settle before spending — without them the four base
/// units cannot pay Ghostly Prison's {6} — and the choice-owing firing must
/// pause the declaration's own tax payment for its colour **before** any mana
/// of the cost is spent and before the attack commits (CR 605.4a).
#[test]
fn mixed_choice_free_and_choice_owing_frame_settles_bonus_and_carries_the_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tide = scenario
        .add_spell_to_hand_from_oracle(P0, "Molten Tide", true, MOLTEN_TIDE_ORACLE)
        .id();
    let selesnya = scenario
        .add_land_from_oracle(P0, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    let chamber = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let mountains: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let attackers: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id())
        .collect();
    add_ghostly_prison(&mut scenario, P1);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(9999), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.cast(tide).resolve();
    clear_pools(&mut runner);

    // CR 311.7 / CR 901.9b: fire Selesnya's chaos trigger through the real
    // pipeline so its delayed echo is live alongside Molten Tide's.
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    while runner.state().delayed_triggers.len() < 2 && !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the chaos trigger must succeed");
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        2,
        "Molten Tide and Selesnya Loft Gardens must both be live"
    );
    clear_pools(&mut runner);

    let attacks: Vec<(ObjectId, AttackTarget)> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();

    // Three attackers owe Ghostly Prison {6}. The four base mana units are
    // short: only the choice-free delayed settlement (two Selesnya Mountain
    // echoes + two Molten Tide bonuses) makes the tax payable.
    assert!(
        attack_tax_is_affordable(runner.state(), &attacks),
        "the preview must count the settled choice-free bonuses even though the \
         chamber's echo still owes a colour"
    );

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("the affordable taxed proposal must open CombatTaxPayment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CombatTaxPayment { .. }
    ));
    assert!(pending_combat_tax_is_affordable(runner.state()));

    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("the mixed frame must settle its choice-free firings and pause for the echo");

    // CR 605.4a + CR 508.1i-j: the choice-owing echo resolves BEFORE the spend
    // and BEFORE the declaration commits. At the pause the sources are already
    // tapped, the pool still holds the unspent base mana plus whichever
    // choice-free firings preceded the echo in collection order, and combat
    // state has not been committed.
    {
        let state = runner.state();
        assert!(
            state
                .combat
                .as_ref()
                .is_none_or(|combat| combat.attackers.is_empty()),
            "the declaration must not commit before the echo choice resolves"
        );
        let pool = state.players[P0.0 as usize].mana_pool.total();
        assert!(
            (4..9).contains(&pool),
            "the {{6}} tax must not be spent before the echo resolves; pool was {pool}"
        );
        assert!(
            mountains
                .iter()
                .chain(std::iter::once(&chamber))
                .all(|land| state.objects[land].tapped),
            "every base source must have been tapped for the frame"
        );
    }

    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "the echo choice belongs to the tapper");
            assert_eq!(
                options.len(),
                2,
                "the chamber echo must offer exactly Green and Blue; got {options:?}"
            );
        }
        other => panic!("expected the echo's colour choice before the commit, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering the echo must succeed");

    {
        let state = runner.state();
        assert_eq!(
            state
                .combat
                .as_ref()
                .expect("combat state must exist once the tax is paid")
                .attackers
                .len(),
            3,
            "the attack must commit after the echo resolves"
        );
        assert_eq!(
            state.players[P0.0 as usize].mana_pool.total(),
            3,
            "base 4 + four choice-free bonuses + the chosen echo (9) minus the {{6}} tax"
        );
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::Priority { player } if player == P0
            ),
            "the resumed declaration must reach the active player's priority; got {:?}",
            state.waiting_for
        );
    }
}

/// CR 603.3 + CR 508.1d + CR 508.1i-j + CR 605.4a: with **no** delayed mana
/// bonus in the payment frame, the tax payment's land taps still belong to
/// ordinary observers. `PayCombatTax` marks its triggers processed inline, so
/// the declaration batch starts after the payment events and the post-action
/// scan retains only `SpellCast`; the payment boundary is therefore the only
/// owner of those events and must claim and defer the ordinary contexts rather
/// than assume the prefix is settled. Two Mountains pay Ghostly Prison's `{2}`
/// with no Molten Tide: Manabarbs must put both triggers on the stack and deal
/// 2 damage.
#[test]
fn combat_tax_payment_reports_ordinary_observers_without_a_delayed_bonus() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let manabarbs = scenario
        .add_enchantment_from_oracle(P0, "Manabarbs", MANABARBS_ORACLE)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mountains: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    add_ghostly_prison(&mut scenario, P1);
    let mut runner = scenario.build();

    let attacks = vec![(attacker, AttackTarget::Player(P1))];
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("the untaxed-bonus proposal must still open CombatTaxPayment");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "the declaration must ask for Ghostly Prison's {{2}}"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("two Mountains pay the {{2}} tax");

    // CR 603.3b: two same-source, indistinguishable triggers are auto-ordered;
    // answer the ordering prompt defensively if this shape ever prompts.
    for _ in 0..4 {
        let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
            break;
        };
        let order: Vec<usize> = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering the two Manabarbs triggers must succeed");
    }

    {
        let state = runner.state();
        assert_eq!(
            state
                .combat
                .as_ref()
                .expect("combat state must exist once the tax is paid")
                .attackers
                .len(),
            1,
            "the attack must commit"
        );
        assert!(
            mountains.iter().all(|land| state.objects[land].tapped),
            "both Mountains paid the {{2}} tax"
        );
        assert_eq!(
            state.stack.len(),
            2,
            "both Manabarbs triggers must be on the stack before priority returns; got {:?}",
            state.stack
        );
        assert!(
            state.stack.iter().all(|entry| matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == manabarbs
            )),
            "both stack entries must be Manabarbs triggers; got {:?}",
            state.stack
        );
    }

    // CR 603.3: let both triggers resolve; each deals 1 to the tapper.
    for _ in 0..8 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.pass_both_players();
    }
    let state = runner.state();
    assert!(
        state.stack.is_empty(),
        "both Manabarbs triggers must resolve; got {:?}",
        state.stack
    );
    assert_eq!(
        state.players[P0.0 as usize].life,
        life_before - 2,
        "two land taps must deal exactly 2 damage to the tapping player"
    );
    assert!(
        mountains.iter().all(|land| state.objects[land].tapped),
        "the observer resolution must not refund or untap the payment sources"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the observer batch must converge back to Priority; got {:?}",
        state.waiting_for
    );
}

/// CR 605.4a + CR 603.3b: a triggered mana ability resolves immediately and
/// never becomes a stack object, so it must never appear in an `OrderTriggers`
/// prompt. The tax payment taps a Chamber that owes a choice-owing echo under
/// Selesnya while an ordinary observer (Manabarbs) watches the same tap: the
/// echo must pause for its colour inside the payment (before the declaration
/// commits), and the observer must reach the stack afterwards without the echo
/// ever being offered for ordering.
#[test]
fn combat_tax_echo_is_answered_without_a_stack_ordering_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selesnya = scenario
        .add_land_from_oracle(P0, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    let chamber = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Manabarbs", MANABARBS_ORACLE);
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    add_ghostly_prison(&mut scenario, P1);
    let mut runner = scenario.build();

    // CR 311.7 / CR 901.9b: fire Selesnya's chaos trigger through the real
    // pipeline so its delayed echo is live.
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the chaos trigger must succeed");
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Selesnya's delayed echo must be live"
    );

    let attacks = vec![(attacker, AttackTarget::Player(P1))];
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("the affordable taxed proposal must open CombatTaxPayment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CombatTaxPayment { .. }
    ));

    let life_before = runner.state().players[P0.0 as usize].life;
    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("the Chamber's {{G}}{{U}} pays the {{2}} tax");

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "a triggered mana ability must never be offered in a stack-ordering prompt; got {:?}",
        runner.state().waiting_for
    );

    // The echo must pause for its colour inside the payment and still reach the
    // pool, and the ordinary observer must resolve afterwards.
    let mut answered = false;
    let mut pool_after_echo = None;
    for _ in 0..12 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } => {
                assert!(
                    options.contains(&ManaType::Green) && options.contains(&ManaType::Blue),
                    "the echo must offer exactly its produced types; got {options:?}"
                );
                runner
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Green),
                        count: 1,
                    })
                    .expect("answering the echo must succeed");
                answered = true;
                pool_after_echo = Some(runner.state().players[P0.0 as usize].mana_pool.total());
            }
            WaitingFor::OrderTriggers { .. } => {
                panic!(
                    "a triggered mana ability must not appear in an ordering prompt; got {:?}",
                    runner.state().waiting_for
                );
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected wait while settling the tax batch: {other:?}"),
        }
    }

    let state = runner.state();
    assert!(answered, "the echo must have owed and received a colour");
    assert_eq!(
        pool_after_echo,
        Some(1),
        "base 2 + the echo (3) minus the {{2}} tax leaves exactly one"
    );
    assert_eq!(
        state.players[P0.0 as usize].life,
        life_before - 1,
        "the Chamber tap's ordinary observer must resolve exactly once"
    );
    assert!(
        state.stack.is_empty(),
        "the observer batch must drain completely; got {:?}",
        state.stack
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the payment and observer batch must converge back to Priority; got {:?}",
        state.waiting_for
    );
    assert!(
        state.objects[&chamber].tapped,
        "the Chamber paid the tax and must be tapped"
    );
}

/// CR 508.1d + CR 509.1c + CR 509.1f + CR 605.4a + CR 509.2a: the **defending**
/// player owes a block tax, and the land that pays it owes a choice-owing echo
/// (Selesnya on the Chamber's `{G}{U}`). The echo must resolve before any of the
/// tax is spent and before the block declaration commits, and the resumed
/// declaration must hand priority to the ACTIVE player — not to the defending
/// payer whose choice just resolved.
#[test]
fn defending_player_block_tax_echo_resolves_before_commit_and_returns_priority_to_active_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selesnya = scenario
        .add_land_from_oracle(P1, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    let chamber = scenario
        .add_land_from_oracle(P1, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let attacker = add_block_tax_raider(&mut scenario);
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    // CR 311.7 / CR 901.9b: fire Selesnya's chaos trigger through the real
    // pipeline so the defending payer's delayed echo is live.
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    while runner.state().delayed_triggers.is_empty() && !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Selesnya's delayed echo must be live"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        0,
        "the defending player starts with an empty pool"
    );

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the untaxed attack must commit");
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("the taxed block must open CombatTaxPayment");

    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment {
            player,
            context,
            total_cost,
            ..
        } => {
            assert_eq!(*player, P1, "the defending player owes the block tax");
            assert!(
                matches!(context, CombatTaxContext::Blocking),
                "the quote must belong to the block step; got {context:?}"
            );
            assert_eq!(total_cost.mana_value(), 1, "{{1}} per blocking creature");
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }

    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("the Chamber must be tapped to pay the block tax");

    // CR 605.4a: the echo owns the prompt. The Chamber paid its base {G}{U}
    // into the pool, but none of the {1} tax is spent and no blocker is
    // committed while the choice is owed.
    {
        let state = runner.state();
        assert!(
            state
                .combat
                .as_ref()
                .is_none_or(|combat| combat.blocker_to_attacker.is_empty()),
            "the block declaration must not commit before the echo resolves"
        );
        assert_eq!(
            state.players[P1.0 as usize].mana_pool.total(),
            2,
            "the {{1}} tax must not be spent before the echo resolves"
        );
        assert!(
            state.objects[&chamber].tapped,
            "the Chamber must have been tapped for the frame"
        );
    }
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the echo choice belongs to the defending tapper"
            );
            assert!(
                options.contains(&ManaType::Green) && options.contains(&ManaType::Blue),
                "the chamber echo must offer exactly Green and Blue; got {options:?}"
            );
        }
        other => panic!("expected the echo's colour choice before the commit, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering the echo must succeed");

    // CR 509.2a: once the last defending player has declared blockers, the
    // active player receives priority — not the payer whose choice resolved.
    let state = runner.state();
    let combat = state
        .combat
        .as_ref()
        .expect("combat state must exist once the tax is paid");
    assert_eq!(
        combat.blocker_to_attacker.get(&blocker),
        Some(&vec![attacker]),
        "the taxed block must commit after the echo resolves"
    );
    assert_eq!(
        combat.blockers_declared_by,
        vec![P1],
        "the defending player's declaration must be recorded"
    );
    assert_eq!(
        state.players[P1.0 as usize].mana_pool.total(),
        2,
        "the Chamber's {{G}}{{U}} plus the echo's chosen mana minus the {{1}} tax leaves two"
    );
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "the resumed block declaration must return priority to the active player; got {:?}",
        state.waiting_for
    );
}
