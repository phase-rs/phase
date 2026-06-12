//! Issue #2925: Mystic Confluence — choosing the "counter target spell unless
//! its controller pays {3}" mode TWICE against the same spell must enforce TWO
//! independent {3} payments. CR 700.2d makes duplicate modal choices repeat in
//! sequence, and CR 608.2c resolves those instructions in written order, so the
//! controller must pay {3} for EACH (i.e. {6} total) or the spell is countered.
//! Pre-fix, a single {3} payment let the spell resolve: the first mode's
//! unless-payment success silently dropped the second mode's `SequentialSibling`
//! counter instead of resolving it.
//!
//! https://github.com/phase-rs/phase/issues/2925

use engine::game::ability_utils::build_chained_resolved;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityCost, AbilityKind, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::zones::Zone;

const COUNTER_MODE_ORACLE: &str = "Counter target spell unless its controller pays {3}.";

/// The counter mode parses to a `Counter` effect carrying an `unless_pay` of
/// {3} payable by the spell's controller. (Sanity guard for the test premise.)
#[test]
fn counter_mode_parses_with_unless_pay_three() {
    let def = parse_effect_chain(COUNTER_MODE_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(*def.effect, Effect::Counter { .. }),
        "counter mode must parse to Effect::Counter, got {:?}",
        def.effect
    );
    let unless = def
        .unless_pay
        .as_ref()
        .expect("counter mode must carry an unless_pay modifier");
    assert!(
        matches!(&unless.cost, AbilityCost::Mana { cost } if cost.mana_value() == 3),
        "the unless cost must be {{3}}, got {:?}",
        unless.cost
    );
}

fn give_generic_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    amount: usize,
) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

fn put_spell_on_stack(runner: &mut engine::game::scenario::GameRunner) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(77),
        P1,
        "Opponent Spell".to_string(),
        Zone::Stack,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// THE discriminating test. The counter mode chosen twice produces a chained
/// `Counter -> Counter(SequentialSibling)`, each with its own {3} unless cost.
/// Paying {3} for the FIRST mode must NOT save the spell — the SECOND mode's
/// {3} unless prompt must still surface, and declining it must counter the
/// spell. Pre-fix, the first payment dropped the second instruction and the
/// spell survived.
#[test]
fn paying_three_once_does_not_save_spell_from_double_counter() {
    let mut runner = GameScenario::new().build();
    let spell = put_spell_on_stack(&mut runner);
    // The controller of the targeted spell (P1) can afford a single {3}.
    give_generic_mana(&mut runner, P1, 3);

    let def = parse_effect_chain(COUNTER_MODE_ORACLE, AbilityKind::Spell);
    // "Choose counter twice" — both indices select the same (counter) mode.
    let mut chained =
        build_chained_resolved(&[def], &[0, 0], ObjectId(9000), P0).expect("chain builds");
    chained.targets = vec![TargetRef::Object(spell)];

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &chained, &mut events, 0)
        .expect("resolve double-counter chain");

    // First unless prompt: P1 may pay {3}.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "first counter mode must prompt P1 for its {{3}} unless cost, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays the first {3}");

    // SECOND unless prompt MUST surface (the second mode is an independent
    // instruction). This is the core regression assertion — pre-fix the second
    // mode was dropped and the state went straight to Priority with the spell
    // surviving on the stack.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "second counter mode must independently prompt P1 for a SECOND {{3}} \
         unless cost (CR 700.2d + CR 608.2c), got {:?}",
        runner.state().waiting_for
    );

    // P1 spent its only {3} on the first mode and cannot pay again — decline.
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines the second {3}");

    // The spell is countered: off the stack and in its owner's graveyard.
    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered when only one of the two {{3}} costs is paid"
    );
    assert!(
        runner.state().players[1].graveyard.contains(&spell),
        "the countered spell must be in its owner's graveyard"
    );
}

/// Paying BOTH {3} costs ({6} total) saves the spell — neither counter fires.
#[test]
fn paying_three_twice_saves_spell_from_double_counter() {
    let mut runner = GameScenario::new().build();
    let spell = put_spell_on_stack(&mut runner);
    // P1 can afford {6} — both unless costs.
    give_generic_mana(&mut runner, P1, 6);

    let def = parse_effect_chain(COUNTER_MODE_ORACLE, AbilityKind::Spell);
    let mut chained =
        build_chained_resolved(&[def], &[0, 0], ObjectId(9000), P0).expect("chain builds");
    chained.targets = vec![TargetRef::Object(spell)];

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &chained, &mut events, 0)
        .expect("resolve double-counter chain");

    // Pay both {3} prompts.
    for which in ["first", "second"] {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::UnlessPayment { player: P1, .. }
            ),
            "{which} counter mode must prompt P1 for {{3}}, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PayUnlessCost { pay: true })
            .unwrap_or_else(|e| panic!("P1 pays the {which} {{3}}: {e:?}"));
    }

    // Both {3} paid → the spell survives.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "paying both {{3}} costs must leave the spell on the stack"
    );
    assert!(
        !runner.state().players[1].graveyard.contains(&spell),
        "a spell that paid both unless costs must not be countered"
    );
}
