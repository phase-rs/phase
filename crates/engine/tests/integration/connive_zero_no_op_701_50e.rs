//! CR 701.50e — "If a permanent would connive 0, no connive event occurs.
//! Abilities that trigger whenever a permanent connives won't trigger."
//!
//! A connive whose dynamic count resolves to 0 (Spymaster's Vault X = creatures
//! that died this turn, with zero deaths recorded) must be a COMPLETE no-op: no
//! draw, no discard, no +1/+1 counter, no `ConniveDiscard` pause, and crucially
//! no `EffectResolved{Connive}` — so a "whenever a creature you control connives"
//! watcher does NOT fire. The positive sibling proves count >= 1 is unregressed
//! and the guard is count == 0-exact.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::GameScenario;
use engine::game::triggers::process_triggers;
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Spymaster's Vault connive line — `connives X, where X is the number of
/// creatures that died this turn` (a dynamic `ZoneChangeCountThisTurn`
/// battlefield→graveyard count). With zero deaths recorded X resolves to 0.
const VAULT_ORACLE: &str = "{B}, {T}: Target creature you control connives X, \
where X is the number of creatures that died this turn.";

/// Parse the Spymaster's Vault connive activated ability and return its
/// definition for re-targeting at an arbitrary conviver from an arbitrary source.
fn vault_connive_def() -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(
        VAULT_ORACLE,
        "Spymaster's Vault",
        &[],
        &["Land".to_string()],
        &[],
    );
    parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::Connive { .. }))
        .expect("must parse a Connive activated ability")
        .clone()
}

/// Drive priority until the stack empties (resolving any connive payoff trigger).
/// Stops at the first non-priority wait so the turn does not advance.
fn drain_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 60, "stack did not drain");
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// CR 701.50e (the revert-failing test): a connive whose dynamic count resolves
/// to 0 is a COMPLETE no-op — no draw, no counter, no `ConniveDiscard` pause, and
/// crucially no `EffectResolved{Connive}`, so a "whenever a creature you control
/// connives" watcher does NOT fire.
///
/// The conviver's hand is EMPTY on purpose: without the guard a count-0 connive
/// draws 0, finds an empty hand (so the discard step is skipped, NOT parked), and
/// falls through to the tail that emits `EffectResolved{Connive}` — which fires
/// the watcher for +2 life. With the guard, `resolve_connive_effect` returns
/// before the tail, no event is emitted, and life is unchanged. (A nonempty hand
/// would instead park a `ConniveDiscard` on revert and never reach the tail,
/// making the life assertion non-discriminating — the inline
/// `connive_count_zero_no_ops` test covers that nonempty-hand discard-park arm.)
///
/// REVERT-FAILING assertion: `life == life_before`. Without the `count == 0`
/// guard in `resolve_connive_effect`, `EffectResolved{Connive}` is emitted, the
/// watcher fires, and `life == life_before + 2`.
#[test]
fn connive_zero_fires_no_trigger_no_draw_no_counter() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // A distinguishable card on library top — must NOT be drawn.
    scenario.with_library_top(P0, &["Top Card"]);

    // Watcher: "whenever a creature you control connives, you gain 2 life."
    scenario.add_creature_from_oracle(
        P0,
        "Glorious Watcher",
        2,
        2,
        "Whenever a creature you control connives, you gain 2 life.",
    );
    // The conviver — a plain creature you control.
    let conniver = scenario.add_creature(P0, "Conniver", 2, 2).id();
    // The Spymaster's Vault source carrying the dynamic-count connive ability.
    let vault = scenario
        .add_creature_from_oracle(P0, "Spymaster's Vault", 0, 0, VAULT_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    // Seed Priority so the "not ConniveDiscard" assertion below is meaningful.
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let life_before = runner.life(P0);
    let top_card = runner.state().players[0].library[0];

    // Empty hand (see the rationale in the doc comment) — required for the
    // life/trigger assertion to be revert-failing.
    assert!(
        runner.state().players[0].hand.is_empty(),
        "precondition: empty hand so a count-0 revert reaches the EffectResolved tail"
    );
    // ZERO creature deaths recorded → connive X resolves to 0.
    assert_eq!(
        runner.state().zone_changes_this_turn.len(),
        0,
        "precondition: no creature has died this turn, so connive X = 0"
    );

    // Resolve the connive on `conniver` from the Vault source, controlled by P0.
    let def = vault_connive_def();
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(conniver)],
        ..build_resolved_from_def(&def, vault, P0)
    };
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("connive must resolve");

    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    // CR 701.50e: no draw — the library-top card is still in the library.
    assert!(
        runner.state().players[0].library.contains(&top_card),
        "connive 0 must not draw: library top must stay in library"
    );
    // CR 701.50e: no +1/+1 counter on the conviver.
    assert_eq!(
        runner.state().objects[&conniver]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "connive 0 must place no +1/+1 counter"
    );
    // CR 701.50e: no ConniveDiscard pause.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ConniveDiscard { .. }
        ),
        "connive 0 must not park a ConniveDiscard, got {:?}",
        runner.state().waiting_for
    );
    // CR 701.50e (REVERT-FAILING): no connive event occurs → the watcher never
    // fires → life is unchanged. Pre-fix EffectResolved{Connive} fires the
    // watcher and life += 2.
    assert_eq!(
        runner.life(P0),
        life_before,
        "connive 0 must not fire 'whenever a creature you control connives' — \
         pre-fix the watcher fires and gains 2 life"
    );
}

/// CR 701.50a + CR 701.50e (positive sibling): the same dynamic-count connive
/// with TWO creature deaths recorded resolves X = 2 and connives NORMALLY — draws
/// 2, discards 2, places 2 +1/+1 counters, and fires the watcher once. Proves the
/// count >= 1 flow is unregressed and the guard is count == 0-exact.
#[test]
fn connive_count_two_connives_normally() {
    let mut scenario = GameScenario::new_n_player(2, 9);
    scenario.at_phase(Phase::PreCombatMain);
    // Two nonland cards on top so connive X=2 draws and auto-discards them
    // (empty hand → draw 2 → discard all 2).
    scenario.with_library_top(P0, &["Top A", "Top B"]);

    scenario.add_creature_from_oracle(
        P0,
        "Glorious Watcher",
        2,
        2,
        "Whenever a creature you control connives, you gain 2 life.",
    );
    let conniver = scenario.add_creature(P0, "Conniver", 2, 2).id();
    let vault = scenario
        .add_creature_from_oracle(P0, "Spymaster's Vault", 0, 0, VAULT_ORACLE)
        .id();
    // Two creatures that will die this turn → X = 2.
    let dead_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let dead_b = scenario.add_creature(P0, "Bear B", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Populate two deaths through the production zone-change recorder.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), dead_a, Zone::Graveyard, &mut events);
    move_to_zone(runner.state_mut(), dead_b, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().zone_changes_this_turn.len(),
        2,
        "precondition: two deaths recorded so connive X = 2"
    );
    events.clear();

    let life_before = runner.life(P0);

    let def = vault_connive_def();
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(conniver)],
        ..build_resolved_from_def(&def, vault, P0)
    };
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("connive must resolve");

    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    // CR 701.50a: drew 2, discarded both (auto), placed 2 +1/+1 counters.
    assert_eq!(
        runner.state().objects[&conniver]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "connive X=2 with two nonland discards must place two +1/+1 counters"
    );
    // The watcher fires exactly once for the single connive event → +2 life.
    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "connive X=2 fires 'whenever a creature you control connives' once → +2 life"
    );
}
