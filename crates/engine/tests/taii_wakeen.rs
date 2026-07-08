//! Taii Wakeen, Perfect Shot — runtime coverage for ability 1.
//!
//! Ability 1 (CR 120.1 + CR 208.1 + CR 603.4): "Whenever a source you control
//! deals noncombat damage to a creature equal to that creature's toughness, draw
//! a card." The intervening-`if` compares the dealt amount
//! (`EventContextAmount`) against the damaged creature's toughness
//! (`ObjectScope::EventTarget`), re-checked at trigger detection and resolution.
//!
//! These tests drive the real cast pipeline and resolve through
//! `process_triggers`, so each would fail if the corresponding code were
//! reverted — notably the detection-time `EventContextAmount` fallback in
//! `game/quantity.rs` (without it the gate reads 0 at detection and never fires).
//!
//! Ability 2 (the `{X},{T}` damage-boost replacement) installs a global
//! `pending_damage_replacements` entry ("If a source you control would deal
//! noncombat damage to a permanent or player this turn, it deals that much damage
//! plus X instead."). Its *runtime* application is exercised in the `a2_runtime`
//! module below: the installing player is anchored onto the replacement
//! (`ReplacementDefinition::source_controller`, CR 109.4) so the
//! `ControllerRef::You` `damage_source_filter` resolves under the sentinel
//! `ObjectId(0)` it lives under. The announced X is frozen into the
//! `DamageModification::Plus` value at install time (CR 107.3a).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, TargetFilter, TypeFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const TAII: &str = "Whenever a source you control deals noncombat damage to a creature \
equal to that creature's toughness, draw a card.\n\
{X}, {T}: If a source you control would deal noncombat damage to a permanent or player \
this turn, it deals that much damage plus X instead.";

/// Add a noncombat "deal `amount` damage to target creature" instant to
/// `player`'s hand. The spell is the damage source (controlled by the caster),
/// so it is a "source you control" for Taii's trigger when cast by Taii's
/// controller.
fn add_damage_spell(scenario: &mut GameScenario, player: PlayerId, amount: i32) -> ObjectId {
    scenario
        .add_spell_to_hand(player, "Test Bolt", true)
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            damage_source: None,
            excess: None,
        })
        .id()
}

/// A1 fires when noncombat damage equals the recipient's (non-zero) toughness —
/// Taii's controller draws a card. Discriminating: the detection-time
/// `EventContextAmount` fallback (quantity.rs) must resolve the dealt amount when
/// `current_trigger_event` is still `None`, or the gate reads 0 and never fires.
#[test]
fn a1_fires_when_noncombat_damage_equals_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);
    scenario.add_creature_from_oracle(P0, "Taii Wakeen, Perfect Shot", 2, 3, TAII);
    // Recipient toughness 3, deal exactly 3 noncombat damage → amount == toughness.
    let target = scenario.add_creature(P1, "Big Bear", 0, 3).id();
    let spell = add_damage_spell(&mut scenario, P0, 3);

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "A1 must draw a card when 3 noncombat damage hits a 3-toughness creature"
    );
}

/// A1 does NOT fire when the dealt amount differs from toughness (the gate is
/// `EQ`, not `GE`).
#[test]
fn a1_no_fire_when_damage_differs_from_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);
    scenario.add_creature_from_oracle(P0, "Taii Wakeen, Perfect Shot", 2, 3, TAII);
    let target = scenario.add_creature(P1, "Big Bear", 0, 4).id();
    let spell = add_damage_spell(&mut scenario, P0, 3);

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "A1 must not fire when 3 damage hits a 4-toughness creature (EQ, not GE)"
    );
}

/// A1 does NOT fire when the matching damage comes from a source the Taii
/// controller does NOT control. The opponent (P1) is the active player here, so
/// P1 legally casts a damage spell that exactly matches the recipient's
/// toughness; Taii's `valid_source` is "a source you control" (P0), so no draw.
#[test]
fn a1_no_fire_on_opponent_source() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Deep libraries so neither player decks out while the turn rolls over.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8"]);
    }
    scenario.add_creature_from_oracle(P0, "Taii Wakeen, Perfect Shot", 2, 3, TAII);
    let target = scenario.add_creature(P0, "Friendly Bear", 0, 3).id();
    let spell = add_damage_spell(&mut scenario, P1, 3);

    let mut runner = scenario.build();
    // Roll forward until P1 (the opponent) is the active player and holds
    // priority, so P1 legally casts the matching damage spell.
    for _ in 0..200 {
        if runner.state().active_player == P1
            && matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1)
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                let _ = runner.act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                });
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                });
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().active_player,
        P1,
        "test precondition: P1 is the active player so P1's source deals the damage"
    );

    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();
    runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before,
        "A1 must not fire when an opponent's source deals the matching damage"
    );
}

/// A1 does NOT fire on COMBAT damage even when it equals the blocker's toughness:
/// the trigger is `NoncombatOnly`. A 3/3 attacker that deals exactly 3 combat
/// damage to a 3-toughness blocker must not draw for P0.
#[test]
fn a1_no_fire_on_combat_damage() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);
    scenario.add_creature_from_oracle(P0, "Taii Wakeen, Perfect Shot", 2, 3, TAII);
    // P0's 3/3 attacker; P1's 0/3 blocker. Combat damage 3 == blocker toughness.
    let attacker = scenario.add_creature(P0, "Striker", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Wall", 0, 3).id();

    let mut runner = scenario.build();
    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();

    // Advance to P0's declare-attackers, attack, block, and let combat damage
    // resolve.
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                let _ = runner.act(GameAction::DeclareAttackers {
                    attacks: vec![(attacker, AttackTarget::Player(P1))],
                    bands: vec![],
                });
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![(blocker, attacker)],
                });
            }
            WaitingFor::Priority { .. } => {
                // Stop once combat damage has been dealt (blocker marked).
                if runner.state().objects[&blocker].damage_marked >= 3 {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        3,
        "test precondition: 3 combat damage was dealt to the 3-toughness blocker"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before,
        "A1 is NoncombatOnly — combat damage equal to toughness must not draw"
    );
}

/// Ability 2 runtime: "{X},{T}: If a source you control would deal noncombat
/// damage to a permanent or player this turn, it deals that much damage plus X
/// instead." These tests drive the install path
/// (`add_target_replacement::resolve`) and the apply path (`replace_event`)
/// with a faithfully-constructed `ReplacementDefinition` matching what the
/// parser emits for A2: a global (`TargetFilter::None`) `DamageDone` replacement
/// carrying `damage_modification: Plus { value: 0 }` (the parser placeholder for
/// the symbolic X), `combat_scope: NoncombatOnly`, and a `ControllerRef::You`
/// `damage_source_filter`. The announced X rides on the installing ability's
/// `chosen_x`; `freeze_damage_modification_x` locks it into the `Plus` value at
/// install time. End-to-end activation through the cast pipeline (choosing X,
/// paying `{X},{T}`) is not modeled here — the install/apply seam is the unit of
/// behavior the fix touches.
mod a2_runtime {
    use engine::game::effects::add_target_replacement;
    use engine::game::replacement::{replace_event, ReplacementResult};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        CombatDamageScope, ControllerRef, DamageModification, Duration, Effect, QuantityExpr,
        ReplacementDefinition, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
    };
    use engine::types::game_state::GameState;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::proposed_event::ProposedEvent;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::Zone;

    /// The replacement the parser emits for Taii A2 before X is frozen: the
    /// `Plus { value: 0 }` placeholder is replaced by the announced X at install
    /// time via `chosen_x`.
    fn a2_replacement() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Plus {
                value: QuantityExpr::Fixed { value: 0 },
            })
            .combat_scope(CombatDamageScope::NoncombatOnly)
            .damage_source_filter(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
    }

    /// Build the A2 install ability with `chosen_x = Some(x)`, controlled by P0.
    fn install_a2(state: &mut GameState, x: u32) {
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(a2_replacement()),
                target: TargetFilter::None,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilEndOfTurn);
        ability.chosen_x = Some(x);
        let mut events = Vec::new();
        add_target_replacement::resolve(state, &ability, &mut events).unwrap();
    }

    fn noncombat_damage(source: ObjectId, victim: ObjectId, amount: u32) -> ProposedEvent {
        ProposedEvent::Damage {
            source_id: source,
            target: TargetRef::Object(victim),
            amount,
            is_combat: false,
            applied: Default::default(),
        }
    }

    /// X=2: a source P0 controls deals noncombat damage boosted by +2, and the
    /// boost fires on multiple damage events (continuous until end of turn).
    /// Discriminating: the `+2` assertion flips if the controller anchor read at
    /// `replacement.rs` is reverted (the replacement would never match the
    /// `ControllerRef::You` source filter and damage would stay unmodified).
    #[test]
    fn a2_boosts_own_source_by_x_on_every_event() {
        let mut state = GameState::new_two_player(42);
        let my_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Source".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );

        install_a2(&mut state, 2);
        let pending = &state.pending_damage_replacements[0];
        assert_eq!(
            pending.source_controller,
            Some(PlayerId(0)),
            "install must anchor the activating controller (CR 109.4)"
        );
        assert_eq!(
            pending.damage_modification,
            Some(DamageModification::Plus {
                value: QuantityExpr::Fixed { value: 2 }
            }),
            "announced X=2 must be frozen into the Plus value (CR 107.3a)"
        );

        let mut events = Vec::new();
        for _ in 0..2 {
            let result = replace_event(
                &mut state,
                noncombat_damage(my_source, victim, 3),
                &mut events,
            );
            let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
                panic!("expected modified damage event, got {result:?}");
            };
            assert_eq!(amount, 5, "3 + X(2) boost must apply on every damage event");
        }
    }

    /// X=0 freeze: the boost is +0 (a genuine literal "plus 0"), so a source we
    /// control deals unmodified damage but the replacement still matches.
    #[test]
    fn a2_x_zero_applies_plus_zero() {
        let mut state = GameState::new_two_player(42);
        let my_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Source".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );

        install_a2(&mut state, 0);
        assert_eq!(
            state.pending_damage_replacements[0].damage_modification,
            Some(DamageModification::Plus {
                value: QuantityExpr::Fixed { value: 0 }
            }),
            "X=0 freezes to Plus 0"
        );

        let mut events = Vec::new();
        let result = replace_event(
            &mut state,
            noncombat_damage(my_source, victim, 4),
            &mut events,
        );
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected damage event, got {result:?}");
        };
        assert_eq!(amount, 4, "X=0 boost is +0");
    }

    /// Negative: an opponent's source is NOT boosted by "a source you control".
    #[test]
    fn a2_does_not_boost_opponent_source() {
        let mut state = GameState::new_two_player(42);
        let their_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Their Source".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Victim".to_string(),
            Zone::Battlefield,
        );

        install_a2(&mut state, 2);

        let mut events = Vec::new();
        let result = replace_event(
            &mut state,
            noncombat_damage(their_source, victim, 3),
            &mut events,
        );
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected damage event, got {result:?}");
        };
        assert_eq!(amount, 3, "an opponent's source must not be boosted");
    }

    /// The replacement is turn-bound: it expires at the cleanup step (CR 514.2).
    /// After `execute_cleanup` the pending replacement is gone and a
    /// previously-boosted source deals unmodified damage.
    #[test]
    fn a2_expires_at_end_of_turn() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let my_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Source".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );

        install_a2(&mut state, 2);
        assert_eq!(state.pending_damage_replacements.len(), 1);

        let mut events = Vec::new();
        engine::game::turns::execute_cleanup(&mut state, &mut events);
        assert!(
            state.pending_damage_replacements.is_empty(),
            "EOT-expiring A2 replacement must be cleared at the cleanup step"
        );

        let result = replace_event(
            &mut state,
            noncombat_damage(my_source, victim, 3),
            &mut events,
        );
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected damage event, got {result:?}");
        };
        assert_eq!(amount, 3, "after expiry the boost no longer applies");
    }
}
