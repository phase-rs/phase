//! Desperate Gambit — "Choose a source you control" must parse as
//! `ChooseDamageSource` with a You-controller candidate filter and thread that
//! filter into the coin-flip one-shot damage-replacement shields.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityKind, ControllerRef, DamageModification, Effect, PreventionAmount, QuantityExpr,
    ReplacementDefinition, ResolvedAbility, ShieldKind, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

fn desperate_gambit_mana() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

fn source_damage_to_player(source: ObjectId, amount: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Player,
            damage_source: None,
        },
        vec![TargetRef::Player(P1)],
        source,
        P0,
    )
}

const DESPERATE_GAMBIT: &str = "Choose a source you control. Flip a coin. If you win the flip, \
    the next time that source would deal damage this turn, it deals double that damage instead. \
    If you lose the flip, the next time it would deal damage this turn, prevent that damage.";

fn typed_you_control(filter: &TargetFilter) -> Option<&TypedFilter> {
    match filter {
        TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::You) => Some(tf),
        _ => None,
    }
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn drive_desperate_gambit_resolution(
    runner: &mut engine::game::scenario::GameRunner,
    chosen_source: ObjectId,
) {
    for _ in 0..96 {
        match &runner.state().waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(
                    options.contains(&chosen_source),
                    "chosen source must be offered, got options {options:?}"
                );
                runner
                    .act(GameAction::ChooseDamageSource {
                        source: chosen_source,
                    })
                    .expect("choose damage source");
            }
            WaitingFor::CoinFlipKeepChoice { .. } => {
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .expect("resolve coin flip keep choice");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve Desperate Gambit");
            }
            other => panic!("unexpected prompt resolving Desperate Gambit: {other:?}"),
        }
    }
    runner.advance_until_stack_empty();
}

fn resolve_desperate_gambit_through_source_choice(
    runner: &mut engine::game::scenario::GameRunner,
    gambit: ObjectId,
    chosen_source: ObjectId,
) {
    runner
        .act(GameAction::CastSpell {
            object_id: gambit,
            card_id: runner.state().objects[&gambit].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Desperate Gambit");

    drive_desperate_gambit_resolution(runner, chosen_source);
}

#[test]
fn desperate_gambit_oracle_parses_choose_damage_source_you_control() {
    let parsed = parse_oracle_text(
        DESPERATE_GAMBIT,
        "Desperate Gambit",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let spell = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.kind, AbilityKind::Spell))
        .expect("Desperate Gambit must have a spell ability");
    match spell.effect.as_ref() {
        Effect::ChooseDamageSource { source_filter } => {
            assert!(
                typed_you_control(source_filter).is_some(),
                "expected You-controller source filter, got {source_filter:?}"
            );
        }
        other => panic!("expected ChooseDamageSource head, got {other:?}"),
    }
}

#[test]
fn desperate_gambit_damage_source_choice_excludes_opponent_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
        .with_mana_cost(desperate_gambit_mana())
        .id();
    let p0_source = scenario.add_creature(P0, "Your Shaman", 2, 2).id();
    let p1_source = scenario.add_creature(P1, "Their Bear", 2, 2).id();

    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Red]);

    runner
        .act(GameAction::CastSpell {
            object_id: gambit,
            card_id: runner.state().objects[&gambit].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Desperate Gambit");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(options.contains(&p0_source), "P0's source must be offered");
                assert!(
                    !options.contains(&p1_source),
                    "P1's source must not be offered under 'you control'"
                );
                runner
                    .act(GameAction::ChooseDamageSource { source: p0_source })
                    .expect("choose P0 source");
                return;
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward source choice");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("Desperate Gambit never prompted for damage source choice");
}

fn shield_targets_chosen_source(shield: &ReplacementDefinition, source: ObjectId) -> bool {
    shield
        .damage_source_filter
        .as_ref()
        .is_some_and(|filter| match filter {
            TargetFilter::SpecificObject { id } => *id == source,
            TargetFilter::And { filters } => filters.iter().any(
                |filter| matches!(filter, TargetFilter::SpecificObject { id } if *id == source),
            ),
            _ => false,
        })
}

fn shields_for_chosen_source(
    runner: &engine::game::scenario::GameRunner,
    source: ObjectId,
) -> Vec<&ReplacementDefinition> {
    let mut shields = runner
        .state()
        .objects
        .get(&source)
        .map(|obj| {
            obj.replacement_definitions
                .iter_unchecked()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    shields.extend(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .filter(|shield| shield_targets_chosen_source(shield, source)),
    );
    shields
}

fn assert_one_shot_shield_on_source(
    runner: &engine::game::scenario::GameRunner,
    source: ObjectId,
    expect_double: bool,
) {
    let shields = shields_for_chosen_source(runner, source);
    let double_shield = shields.iter().find(|s| {
        matches!(s.shield_kind, ShieldKind::DamageReplacementOneShot)
            && s.damage_modification == Some(DamageModification::Double)
    });
    let prevent_shield = shields.iter().find(|s| {
        matches!(
            s.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        )
    });
    if expect_double {
        assert!(
            double_shield.is_some(),
            "win branch must install double shield on chosen source, got {:?}",
            shields,
        );
        assert!(
            prevent_shield.is_none(),
            "win branch must not install prevention shield"
        );
    } else {
        assert!(
            prevent_shield.is_some(),
            "lose branch must install prevention shield on chosen source, got {:?}",
            shields,
        );
        assert!(
            double_shield.is_none(),
            "lose branch must not install double shield"
        );
    }
}

fn assert_damage_outcome(
    runner: &mut engine::game::scenario::GameRunner,
    source: ObjectId,
    expect_double: bool,
) {
    let mut events = Vec::<GameEvent>::new();
    deal_damage::resolve(
        runner.state_mut(),
        &source_damage_to_player(source, 3),
        &mut events,
    )
    .expect("damage resolves");

    if expect_double {
        assert_eq!(
            runner.state().players[P1.0 as usize].life,
            14,
            "win branch must double 3 → 6",
        );
    } else {
        assert_eq!(
            runner.state().players[P1.0 as usize].life,
            20,
            "lose branch must prevent damage",
        );
    }
}

#[test]
fn desperate_gambit_win_branch_installs_double_shield_on_chosen_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
        .with_mana_cost(desperate_gambit_mana())
        .id();
    let source = scenario.add_creature(P0, "Damage Source", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(0);
    add_mana(&mut runner, &[ManaType::Red]);
    resolve_desperate_gambit_through_source_choice(&mut runner, gambit, source);

    assert_one_shot_shield_on_source(&runner, source, true);
    assert_damage_outcome(&mut runner, source, true);
}

#[test]
fn desperate_gambit_lose_branch_installs_prevention_shield_on_chosen_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
        .with_mana_cost(desperate_gambit_mana())
        .id();
    let source = scenario.add_creature(P0, "Damage Source", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(1);
    add_mana(&mut runner, &[ManaType::Red]);
    resolve_desperate_gambit_through_source_choice(&mut runner, gambit, source);

    assert_one_shot_shield_on_source(&runner, source, false);
    assert_damage_outcome(&mut runner, source, false);
}
