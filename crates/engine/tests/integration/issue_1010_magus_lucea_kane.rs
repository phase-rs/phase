//! Issue #1010 — Magus Lucea Kane Psychic Stimulus copies the next {X} spell or ability.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef,
    SacrificeRequirement, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, ConvokeMode, PayCostKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MAGUS_ORACLE: &str = "Psychic Stimulus — {T}: Add {C}{C}. When you next cast a spell with {X} in its mana cost or activate an ability with {X} in its activation cost this turn, copy that spell or ability. You may choose new targets for the copy.";

fn add_colorless(runner: &mut engine::game::scenario::GameRunner, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
}

#[test]
fn magus_lucea_kane_tap_ability_parses_delayed_copy_not_unimplemented() {
    let parsed = engine::parser::parse_oracle_text(
        MAGUS_ORACLE,
        "Magus Lucea Kane",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let ability = parsed.abilities.last().expect("tap ability");
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("delayed copy sub_ability");
    assert!(
        matches!(sub.effect.as_ref(), Effect::CreateDelayedTrigger { .. }),
        "expected CreateDelayedTrigger, got {:?}",
        sub.effect
    );
}

#[test]
fn magus_lucea_kane_tap_registers_delayed_copy_for_next_x_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
        ],
    );

    let magus = scenario
        .add_creature_from_oracle(P0, "Magus Lucea Kane", 2, 4, MAGUS_ORACLE)
        .id();

    let spell = scenario
        .add_spell_to_hand(P0, "Draw X", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .with_ability(Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            target: TargetFilter::Controller,
        })
        .id();

    let mut runner = scenario.build();
    let library_before = runner.state().players[0].library.len();

    runner.activate(magus, 0).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Psychic Stimulus must register a one-shot delayed trigger"
    );

    add_colorless(&mut runner, 3);
    runner.cast(spell).x(2).resolve();

    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        4,
        "Draw X=2 plus its copy should draw four cards total"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "one-shot delayed trigger must be consumed after the X spell is cast"
    );
}

#[test]
fn magus_delayed_copy_trigger_sits_above_activated_ability_on_stack() {
    use engine::types::actions::GameAction;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
        ],
    );

    let magus = scenario
        .add_creature_from_oracle(P0, "Magus Lucea Kane", 2, 4, MAGUS_ORACLE)
        .id();

    let draw_x_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        },
    });

    let source = scenario
        .add_creature(P0, "Draw X Source", 1, 1)
        .with_ability_definition(draw_x_ability)
        .id();

    let mut runner = scenario.build();
    runner.activate(magus, 0).resolve();
    add_colorless(&mut runner, 3);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announce activation");
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announce X");
    runner
        .act(GameAction::PassPriority)
        .expect("pay activation cost");

    assert_eq!(
        runner.state().stack.len(),
        2,
        "activated ability plus Magus delayed copy trigger must both be on stack"
    );
    assert!(
        matches!(
            runner.state().stack.back().map(|e| &e.kind),
            Some(engine::types::game_state::StackEntryKind::TriggeredAbility { .. })
        ),
        "Magus delayed copy must be on top of the activating ability"
    );

    let library_before = runner.state().players[0].library.len();
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass P0");
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass P1");
    }
    assert!(
        runner.state().stack.is_empty(),
        "both stack objects must resolve, stack = {:?}",
        runner.state().stack.len()
    );
    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        4,
        "copy trigger plus original and copy of Draw X=2 should draw four cards"
    );
}

#[test]
fn magus_lucea_kane_tap_registers_delayed_copy_for_next_x_activated_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
        ],
    );

    let magus = scenario
        .add_creature_from_oracle(P0, "Magus Lucea Kane", 2, 4, MAGUS_ORACLE)
        .id();

    let draw_x_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        },
    });

    let source = scenario
        .add_creature(P0, "Draw X Source", 1, 1)
        .with_ability_definition(draw_x_ability)
        .id();

    let mut runner = scenario.build();
    let library_before = runner.state().players[0].library.len();

    runner.activate(magus, 0).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    match &runner.state().delayed_triggers[0].condition {
        engine::types::ability::DelayedTriggerCondition::WhenNextEvent { or_trigger, .. } => {
            assert!(
                or_trigger.is_some(),
                "Magus delayed trigger must carry or_trigger for activated abilities"
            )
        }
        other => panic!("expected WhenNextEvent delayed trigger, got {other:?}"),
    }

    add_colorless(&mut runner, 3);
    runner.activate(source, 0).x(2).resolve();

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "one-shot delayed trigger must be consumed after the X activated ability"
    );
    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        4,
        "Draw X=2 activated ability plus its copy should draw four cards total"
    );
}

// ---------------------------------------------------------------------------
// "Activate an ability with {X} in its activation cost" is answered from the
// activation's own cost, bound before payment and carried on its
// `AbilityActivated` event (CR 602.1a + CR 603.2) — never re-read from the
// source after a cost moved it (CR 400.7), from a stale per-source row, or
// from a later activation of the same source (CR 107.3k). Mana-ability
// activations are activations too (CR 605.3).
// ---------------------------------------------------------------------------

/// Magus Lucea Kane, both printed lines with reminder text (MTGJSON).
const MAGUS_VERBATIM: &str = "Spiritual Leader — At the beginning of combat on your turn, put a +1/+1 counter on target creature.\nPsychic Stimulus — {T}: Add {C}{C}. When you next cast a spell with {X} in its mana cost or activate an ability with {X} in its activation cost this turn, copy that spell or ability. You may choose new targets for the copy. (A copy of a permanent spell becomes a token.)";
const PERNICIOUS_DEED: &str = "{X}, Sacrifice this enchantment: Destroy each artifact, creature, and enchantment with mana value X or less.";
const COPY_ENCHANTMENT: &str =
    "You may have this enchantment enter as a copy of any enchantment on the battlefield.";
const KATARA: &str = "Vigilance\nWhen Katara enters, create a 1/1 white Ally creature token.\nWaterbend {X}: Creatures you control have base power and toughness X/X until end of turn. X can't be 0. Activate only during your turn. (While paying a waterbend cost, you can tap your artifacts and creatures to help. Each one pays for {1}.)";
const WIZARDS_ROCKETS: &str = "This artifact enters tapped.\n{X}, {T}, Sacrifice this artifact: Add X mana in any combination of colors.\nWhen this artifact is put into a graveyard from the battlefield, draw a card.";
const LAZAV: &str = "When Lazav enters, surveil 1. (Look at the top card of your library. You may put it into your graveyard.)\n{X}: Lazav becomes a copy of target creature card in your graveyard with mana value X, except its name is Lazav, the Multifarious, it's legendary in addition to its other types, and it has this ability.";
const BOTTLE_GNOMES: &str = "Sacrifice this creature: You gain 3 life.";
/// Krav, the Unredeemed's activated line, verbatim.
const KRAV_ACTIVATED: &str =
    "{B}, Sacrifice X creatures: Target player draws X cards and gains X life. Put X +1/+1 counters on Krav.";
const SPHINXS_REVELATION: &str = "You gain X life and draw X cards.";

fn mana(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn add_mana(runner: &mut GameRunner, mana_type: ManaType, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            mana_type,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

fn copy_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
            "Card Seven",
            "Card Eight",
            "Card Nine",
            "Card Ten",
        ],
    );
    scenario
}

fn add_magus(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Magus Lucea Kane", 1, 1, MAGUS_VERBATIM)
        .with_mana_cost(mana(
            &[
                ManaCostShard::Green,
                ManaCostShard::Blue,
                ManaCostShard::Red,
            ],
            1,
        ))
        .id()
}

/// Tap Magus for {C}{C}, registering its one-shot delayed copy trigger.
fn stimulate(runner: &mut GameRunner, magus: ObjectId) {
    runner.activate(magus, 0).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: Psychic Stimulus registered its delayed copy trigger"
    );
}

fn magus_trigger_count(runner: &GameRunner, magus: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && entry.source_id == magus
        })
        .count()
}

fn activated_entries_of(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::ActivatedAbility { source_id, .. } if *source_id == source
            )
        })
        .count()
}

fn act(runner: &mut GameRunner, action: GameAction) {
    let label = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{label} must be accepted: {error:?}"));
}

/// Announce an `{X}` activation with the given X and pay it, ending at the
/// activator's priority with the ability on the stack.
fn activate_with_x(runner: &mut GameRunner, source: ObjectId, x: u32) {
    act(
        runner,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "the activation announces X: {:?}",
        runner.state().waiting_for
    );
    act(runner, GameAction::ChooseX { value: x });
    pay_until_priority(runner);
}

fn pay_until_priority(runner: &mut GameRunner) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        act(runner, GameAction::PassPriority);
    }
    panic!(
        "the activation must finish paying: {:?}",
        runner.state().waiting_for
    );
}

/// Both players pass once, resolving the top of the stack.
fn pass_both(runner: &mut GameRunner) {
    act(runner, GameAction::PassPriority);
    act(runner, GameAction::PassPriority);
}

fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..32 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the stack drains through priority passes: {:?}",
            runner.state().waiting_for
        );
        act(runner, GameAction::PassPriority);
    }
    assert!(runner.state().stack.is_empty(), "the stack drained");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// M-1/M-2: Pernicious Deed's "{X}, Sacrifice this enchantment" moves its
/// source as a cost. The activation's cost was bound before payment, so the
/// departed-source branch answers from it: Magus's trigger is used up and
/// copies the ability. Ruling: X = 0 still counts (the symbol, not its value).
fn deed_x_activation_consumes_magus_and_is_copied(x: u32) {
    let mut scenario = copy_scenario();
    let magus = add_magus(&mut scenario);
    let deed = scenario
        .add_enchantment_from_oracle(P0, "Pernicious Deed", PERNICIOUS_DEED)
        .with_mana_cost(mana(&[ManaCostShard::Black, ManaCostShard::Green], 1))
        .id();
    let mut runner = scenario.build();
    stimulate(&mut runner, magus);

    activate_with_x(&mut runner, deed, x);

    assert_eq!(
        runner.state().objects[&deed].zone,
        Zone::Graveyard,
        "reach: the cost sacrificed Deed, so the event takes the departed branch"
    );
    let Some(StackEntryKind::ActivatedAbility { ability, .. }) =
        runner.state().stack.front().map(|entry| &entry.kind)
    else {
        panic!("Deed's ability is on the stack: {:?}", runner.state().stack);
    };
    assert_eq!(ability.chosen_x, Some(x));
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 602.1a: an ability with {{X}} in its activation cost uses up Magus's trigger"
    );
    assert_eq!(activated_entries_of(&runner, deed), 1);
    assert_eq!(magus_trigger_count(&runner, magus), 1);

    pass_both(&mut runner);
    assert_eq!(
        activated_entries_of(&runner, deed),
        2,
        "CR 707.10: Magus copied Deed's ability (the copy above the original)"
    );
    drain_stack(&mut runner);
}

#[test]
fn magus_copies_a_pernicious_deed_activation_that_sacrifices_deed() {
    deed_x_activation_consumes_magus_and_is_copied(1);
}

#[test]
fn magus_copies_a_pernicious_deed_activation_with_x_zero() {
    deed_x_activation_consumes_magus_and_is_copied(0);
}

/// M-3: Copy Enchantment entered as a copy of Pernicious Deed. Its copied
/// "{X}, Sacrifice this enchantment" sacrifices it; afterwards the object in the
/// graveyard is Copy Enchantment with no abilities, so only the cost bound at
/// announcement can answer (CR 707.2 + CR 602.2a).
#[test]
fn magus_copies_a_copied_deed_ability_whose_source_lost_it() {
    let mut scenario = copy_scenario();
    let magus = add_magus(&mut scenario);
    let deed = scenario
        .add_enchantment_from_oracle(P0, "Pernicious Deed", PERNICIOUS_DEED)
        .with_mana_cost(mana(&[ManaCostShard::Black, ManaCostShard::Green], 1))
        .id();
    let copy_enchantment = scenario
        .add_spell_to_hand_from_oracle(P0, "Copy Enchantment", false, COPY_ENCHANTMENT)
        .as_enchantment()
        .with_mana_cost(mana(&[ManaCostShard::Blue], 2))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 2);
    add_mana(&mut runner, ManaType::Blue, 1);
    drop(runner.cast(copy_enchantment).commit());
    pass_both(&mut runner);
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "Copy Enchantment's entry replacement is offered: {:?}",
            runner.state().waiting_for
        );
    };
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == copy_enchantment)
        .expect("Copy Enchantment's own replacement is a candidate");
    act(&mut runner, GameAction::ChooseReplacement { index });
    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &runner.state().waiting_for else {
        panic!(
            "Copy Enchantment chooses what to copy: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(valid_targets.contains(&deed));
    act(
        &mut runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(deed)),
        },
    );
    let copy = &runner.state().objects[&copy_enchantment];
    assert_eq!(copy.name, "Pernicious Deed", "reach: it entered as Deed");
    assert_eq!(copy.abilities.len(), 1);

    stimulate(&mut runner, magus);
    activate_with_x(&mut runner, copy_enchantment, 1);

    let moved = &runner.state().objects[&copy_enchantment];
    assert_eq!(moved.zone, Zone::Graveyard);
    assert!(
        moved.abilities.is_empty(),
        "reach: after the sacrifice no ability is left at the activated index"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the copied ability's own {{X}} cost uses up Magus's trigger"
    );
    assert_eq!(activated_entries_of(&runner, copy_enchantment), 1);
    assert_eq!(magus_trigger_count(&runner, magus), 1);

    pass_both(&mut runner);
    assert_eq!(
        activated_entries_of(&runner, copy_enchantment),
        2,
        "CR 707.10: Magus copied the copied Deed ability"
    );
    drain_stack(&mut runner);
}

/// M-4: a source's `{X}` activation waits on the stack; its `{T}: Add {C}` is
/// then activated. That mana activation's own cost has no `{X}`, so Magus's
/// trigger stays — the next `{X}` spell is the one copied.
#[test]
fn a_mana_ability_of_a_source_with_a_waiting_x_activation_does_not_use_up_magus() {
    let mut scenario = copy_scenario();
    let magus = add_magus(&mut scenario);
    let source = scenario
        .add_artifact_from_oracle(P0, "X Draw Engine", "{X}: Draw X cards.\n{T}: Add {C}.")
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let revelation = scenario
        .add_spell_to_hand_from_oracle(P0, "Sphinx's Revelation", true, SPHINXS_REVELATION)
        .with_mana_cost(mana(
            &[
                ManaCostShard::X,
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            0,
        ))
        .id();
    let mut runner = scenario.build();
    let library_before = runner.state().players[0].library.len();
    let life_before = runner.state().players[0].life;

    add_mana(&mut runner, ManaType::Colorless, 1);
    activate_with_x(&mut runner, source, 1);
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: magus,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    assert_eq!(activated_entries_of(&runner, source), 1);

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(
        runner.state().objects[&source].tapped,
        "reach: the source's mana ability was activated"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "CR 602.1a: `{{T}}: Add {{C}}` has no {{X}} in its activation cost"
    );
    assert_eq!(magus_trigger_count(&runner, magus), 0);

    add_mana(&mut runner, ManaType::White, 1);
    add_mana(&mut runner, ManaType::Blue, 2);
    runner.cast(revelation).x(2).resolve();

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "positive reach: the delayed trigger was alive and fired on the X spell"
    );
    assert_eq!(
        runner.state().players[0].life - life_before,
        4,
        "Revelation and its copy each gain 2 life"
    );
    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        5,
        "Revelation 2 + its copy 2 + the X=1 activation 1"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Lazav, the Multifarious becomes a copy of `copied` (a creature card in the
/// graveyard with mana value `x`), keeping its own `{X}` ability at index 1.
fn lazav_copying(
    scenario_setup: impl FnOnce(&mut GameScenario) -> ObjectId,
    x: u32,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = copy_scenario();
    let magus = add_magus(&mut scenario);
    let lazav = scenario
        .add_creature_from_oracle(P0, "Lazav, the Multifarious", 1, 3, LAZAV)
        .with_mana_cost(mana(&[ManaCostShard::Blue, ManaCostShard::Black], 0))
        .id();
    let copied = scenario_setup(&mut scenario);
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, x as usize);
    runner
        .activate(lazav, 0)
        .x(x)
        .target_object(copied)
        .resolve();
    (runner, magus, lazav)
}

fn lazav_costs(runner: &GameRunner, lazav: ObjectId) -> Vec<Option<AbilityCost>> {
    runner.state().objects[&lazav]
        .abilities
        .iter()
        .map(|ability| ability.cost.clone())
        .collect()
}

fn is_x_mana(cost: &Option<AbilityCost>) -> bool {
    matches!(
        cost,
        Some(AbilityCost::Mana { cost: ManaCost::Cost { shards, .. } })
            if shards.contains(&ManaCostShard::X)
    )
}

/// M-5: Lazav as a copy of Bottle Gnomes sacrifices itself. Its activated
/// cost "Sacrifice this creature" has no `{X}`; after the move the index
/// names Lazav's own `{X}` ability, which must not answer for it.
#[test]
fn lazav_copying_bottle_gnomes_sacrificing_itself_does_not_use_up_magus() {
    let (mut runner, magus, lazav) = lazav_copying(
        |scenario| {
            scenario
                .add_creature_to_graveyard(P0, "Bottle Gnomes", 1, 3)
                .with_mana_cost(ManaCost::generic(3))
                .from_oracle_text(BOTTLE_GNOMES)
                .id()
        },
        3,
    );
    let costs = lazav_costs(&runner, lazav);
    assert!(
        matches!(
            costs.first(),
            Some(Some(AbilityCost::Sacrifice(sacrifice)))
                if sacrifice.target == TargetFilter::SelfRef
        ) && costs.get(1).is_some_and(is_x_mana),
        "fixture: Lazav is Bottle Gnomes with its own {{X}} ability second: {costs:?}"
    );
    stimulate(&mut runner, magus);
    let life_before = runner.state().players[0].life;

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: lazav,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    pay_until_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&lazav].zone,
        Zone::Graveyard,
        "reach: the cost moved Lazav, so the event takes the departed branch"
    );
    assert_eq!(activated_entries_of(&runner, lazav), 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(magus_trigger_count(&runner, magus), 0);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "CR 602.1a: \"Sacrifice this creature\" has no {{X}} in its activation cost"
    );
    pass_both(&mut runner);
    assert_eq!(runner.state().players[0].life - life_before, 3);
}

/// M-6: Lazav as a copy of Krav, the Unredeemed, sacrifices itself as X = 1.
/// "{B}, Sacrifice X creatures" includes X but not the mana symbol `{X}`
/// (ruling), even though the activation chose an X.
#[test]
fn lazav_copying_krav_sacrificing_itself_as_x_does_not_use_up_magus() {
    let (mut runner, magus, lazav) = lazav_copying(
        |scenario| {
            scenario
                .add_creature_to_graveyard(P0, "Krav, the Unredeemed", 3, 3)
                .with_mana_cost(mana(&[ManaCostShard::Black], 4))
                .from_oracle_text(KRAV_ACTIVATED)
                .id()
        },
        5,
    );
    let costs = lazav_costs(&runner, lazav);
    assert!(
        matches!(
            costs.first(),
            Some(Some(AbilityCost::Composite { costs: krav }))
                if krav.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::Sacrifice(sacrifice)
                        if sacrifice.requirement == SacrificeRequirement::Count { count: u32::MAX }
                ))
        ) && costs.get(1).is_some_and(is_x_mana),
        "fixture: Lazav is Krav with its own {{X}} ability second: {costs:?}"
    );
    stimulate(&mut runner, magus);
    add_mana(&mut runner, ManaType::Black, 1);

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: lazav,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(
        &mut runner,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
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
        "{:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::SelectCards { cards: vec![lazav] });
    pay_until_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&lazav].zone,
        Zone::Graveyard,
        "reach: Lazav sacrificed itself as X = 1"
    );
    assert_eq!(activated_entries_of(&runner, lazav), 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(magus_trigger_count(&runner, magus), 0);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "ruling: a cost that includes X but not the mana symbol {{X}} is not copied"
    );
}

/// M-7: Katara's "Waterbend {X}" means "Pay {X}" (CR 701.67a), so the
/// activation has `{X}` in its activation cost.
#[test]
fn magus_copies_katara_waterbend_x_activation() {
    let mut scenario = copy_scenario();
    let magus = add_magus(&mut scenario);
    let katara = scenario
        .add_creature_from_oracle(P0, "Katara, Water Tribe's Hope", 3, 3, KATARA)
        .with_mana_cost(mana(
            &[
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            2,
        ))
        .id();
    let mut runner = scenario.build();
    stimulate(&mut runner, magus);
    add_mana(&mut runner, ManaType::Colorless, 2);

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: katara,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { min: 1, .. }
        ),
        "X can't be 0: {:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::ChooseX { value: 2 });
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Waterbend),
                ..
            }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::PassPriority);

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 701.67a: Waterbend {{X}} has {{X}} in its activation cost"
    );
    assert_eq!(activated_entries_of(&runner, katara), 1);
    assert_eq!(magus_trigger_count(&runner, magus), 1);

    pass_both(&mut runner);
    assert_eq!(
        activated_entries_of(&runner, katara),
        2,
        "CR 707.10: Magus copied the waterbend ability"
    );
    drain_stack(&mut runner);
    let katara_object = &runner.state().objects[&katara];
    assert_eq!(
        (katara_object.power, katara_object.toughness),
        (Some(2), Some(2))
    );
}

/// M-8: Wizard's Rockets' "{X}, {T}, Sacrifice this artifact: Add X mana …" is
/// a mana ability with `{X}` in its activation cost. Its activation uses up
/// Magus's trigger (CR 605.3: activating a mana ability follows CR 602.2), and
/// the trigger copies nothing — a mana ability never uses the stack
/// (CR 605.3b) — so the next `{X}` spell is not copied.
#[test]
fn wizards_rockets_x_mana_ability_uses_up_magus_and_copies_nothing() {
    let mut scenario = copy_scenario();
    let magus = add_magus(&mut scenario);
    let rockets = scenario
        .add_artifact_from_oracle(P0, "Wizard's Rockets", WIZARDS_ROCKETS)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let revelation = scenario
        .add_spell_to_hand_from_oracle(P0, "Sphinx's Revelation", true, SPHINXS_REVELATION)
        .with_mana_cost(mana(
            &[
                ManaCostShard::X,
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            0,
        ))
        .id();
    let mut runner = scenario.build();
    stimulate(&mut runner, magus);

    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: rockets,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
        panic!(
            "CR 603.3b: Rockets' dies trigger and Magus's trigger wait together: {:?}",
            runner.state().waiting_for
        );
    };
    let mut sources: Vec<ObjectId> = triggers.iter().map(|trigger| trigger.source_id).collect();
    sources.sort();
    let mut expected = vec![magus, rockets];
    expected.sort();
    assert_eq!(sources, expected);
    assert_eq!(runner.state().objects[&rockets].zone, Zone::Graveyard);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 605.3: the {{X}} mana ability's activation uses up Magus's trigger"
    );

    act(&mut runner, GameAction::OrderTriggers { order: vec![0, 1] });
    assert_eq!(magus_trigger_count(&runner, magus), 1);
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == rockets
            && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })));

    for _ in 0..32 {
        assert_eq!(
            activated_entries_of(&runner, rockets),
            0,
            "CR 605.3b: there is no Rockets ability on the stack to copy"
        );
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        act(&mut runner, GameAction::PassPriority);
    }
    assert!(runner.state().stack.is_empty());

    let library_before = runner.state().players[0].library.len();
    let life_before = runner.state().players[0].life;
    add_mana(&mut runner, ManaType::White, 1);
    add_mana(&mut runner, ManaType::Blue, 2);
    runner.cast(revelation).x(2).resolve();
    assert_eq!(
        runner.state().players[0].life - life_before,
        2,
        "the used-up trigger does not copy the next {{X}} spell"
    );
    assert_eq!(library_before - runner.state().players[0].library.len(), 2);
}

/// Helix Pinnacle, verbatim (MTGJSON), reminder text included.
const HELIX_PINNACLE: &str = "Shroud (This enchantment can't be the target of spells or abilities.)\n{X}: Put X tower counters on this enchantment.\nAt the beginning of your upkeep, if there are 100 or more tower counters on this enchantment, you win the game.";
/// Stifle, verbatim (MTGJSON).
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";

fn tower_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&engine::types::counter::CounterType::Generic(
            "tower".to_string(),
        ))
        .copied()
        .unwrap_or(0)
}

/// M-9 (ruling 2022-10-07: "A copy is created even if the spell or ability
/// that caused the delayed triggered ability to trigger has been countered by
/// the time that delayed triggered ability resolves"; CR 701.6a + CR 113.7a +
/// CR 608.2h + CR 707.10): Stifle counters Helix Pinnacle's `{X}` activation
/// (X = 2) while Magus's delayed trigger waits above it. The trigger is used up
/// by the activation and still copies it with the same X, so only the copy puts
/// counters.
#[test]
fn magus_copies_an_x_activation_countered_before_its_delayed_trigger_resolves() {
    let mut scenario = copy_scenario();
    let magus = add_magus(&mut scenario);
    let pinnacle = scenario
        .add_enchantment_from_oracle(P0, "Helix Pinnacle", HELIX_PINNACLE)
        .from_oracle_text_with_keywords(&["Shroud"], HELIX_PINNACLE)
        .with_mana_cost(mana(&[ManaCostShard::Green], 0))
        .id();
    let stifle = scenario
        .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    assert!(
        is_x_mana(&runner.state().objects[&pinnacle].abilities[0].cost),
        "reach: Helix Pinnacle's ability 0 is its {{X}} ability"
    );
    stimulate(&mut runner, magus);

    // Magus's {C}{C} pays X = 2.
    activate_with_x(&mut runner, pinnacle, 2);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 602.1a: the {{X}} activation uses up Magus's trigger"
    );
    assert_eq!(activated_entries_of(&runner, pinnacle), 1);
    assert_eq!(magus_trigger_count(&runner, magus), 1);
    let activation = runner.state().stack[0].id;

    drop(runner.cast(stifle).target_objects(&[activation]).commit());
    pass_both(&mut runner);
    assert_eq!(
        activated_entries_of(&runner, pinnacle),
        0,
        "reach: Stifle countered the activation"
    );
    assert_eq!(
        magus_trigger_count(&runner, magus),
        1,
        "reach: Magus's delayed trigger still waits"
    );

    pass_both(&mut runner);
    let copies: Vec<Option<u32>> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::ActivatedAbility { source_id, ability } if *source_id == pinnacle => {
                Some(ability.chosen_x)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        copies,
        vec![Some(2)],
        "CR 707.10: the countered activation is copied with its X"
    );
    drain_stack(&mut runner);
    assert_eq!(
        tower_counters(&runner, pinnacle),
        2,
        "only the copy resolves"
    );
}
