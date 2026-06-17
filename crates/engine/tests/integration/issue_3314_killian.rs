//! Regression for issue #3314: Killian, Decisive Mentor.
//!
//! Killian reads "Whenever one or more creatures that are enchanted by an Aura
//! you control attack, draw a card." Per CR 303.4e an Aura's controller is
//! separate from the enchanted object's controller — so the trigger must fire
//! even when an *opponent's* creature, enchanted by an Aura *you* control,
//! attacks. The live parser already produced the attachment-relation filter,
//! but the runtime matcher left the trigger controller-scoped (it only fired
//! when the trigger's controller was the attacking player), so an opponent's
//! enchanted creature attacking did not draw the card.
//!
//! The fix decouples two channels that the matcher previously conflated through
//! `valid_target = Some(TargetFilter::Player)`:
//!   - attacking-player scope (CR 506.2) → stays in `valid_target`; for the
//!     attachment-relation class it is set to the permissive pass-through so any
//!     attacking player qualifies (CR 303.4e).
//!   - attacked-target narrowing ("attack a player", CR 508.3a) → moves to the
//!     purpose-built `attack_target_filter`.
//!
//! Decoupling them is what lets Killian fire when its enchanted creature attacks
//! a planeswalker or battle, not only a player.
//!
//! https://github.com/phase-rs/phase/issues/3314

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const KILLIAN_TRIGGER: &str =
    "Whenever one or more creatures that are enchanted by an Aura you control attack, draw a card.";
const CONTROL_SCOPED_TRIGGER: &str =
    "Whenever one or more creatures you control attack, draw a card.";

/// Number of cards in `player`'s hand right now.
fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

/// Attach a P0-controlled Aura to `host` (which may be controlled by anyone).
/// CR 303.4e: the Aura's controller (P0) is independent of the host's
/// controller. Returns the Aura's id.
fn attach_p0_aura(runner: &mut GameRunner, host: ObjectId) -> ObjectId {
    let state = runner.state_mut();
    let aura = engine::game::zones::create_object(
        state,
        engine::types::identifiers::CardId(9000 + host.0),
        P0,
        "Cartouche".to_string(),
        Zone::Battlefield,
    );
    {
        let aura_obj = state.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types.push(CoreType::Enchantment);
        aura_obj.card_types.subtypes.push("Aura".to_string());
        aura_obj.attached_to = Some(AttachTarget::Object(host));
    }
    {
        let host_obj = state.objects.get_mut(&host).unwrap();
        host_obj.attachments.push(aura);
    }
    aura
}

/// Turn an existing battlefield object into a planeswalker with loyalty so it is
/// a legal attack target (CR 306, CR 508.1b).
fn make_planeswalker(runner: &mut GameRunner, id: ObjectId, loyalty: u32) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    // Replace the builder's Creature type with Planeswalker (a 0/0 creature would
    // be destroyed by SBAs, CR 704.5f); a planeswalker has no power/toughness.
    obj.card_types.core_types = vec![CoreType::Planeswalker];
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    // Mirror onto base_card_types so layer re-evaluation (which rebuilds
    // card_types from base_card_types) preserves the Planeswalker type.
    obj.base_card_types = obj.card_types.clone();
    // CR 306.5b: loyalty field and counter map mirror each other.
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
}

/// Drive the (already-declared) combat trigger to resolution by passing
/// priority until the stack and any triggered abilities settle. Declares empty
/// blocks if asked.
fn settle(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
}

/// (1) CR 303.4e: P1 (active) attacks a player with a creature enchanted by P0's
/// Aura; P0 controls Killian → P0 draws a card.
///
/// Discriminator for EDIT A: if `try_parse_n_or_more_attacks` does not set the
/// attachment-relation gate to the permissive pass-through, the matcher stays
/// controller-scoped, the attacking player (P1) ≠ Killian's controller (P0),
/// and P0's hand does not grow.
#[test]
fn killian_fires_on_opponent_attack_with_enchanted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);

    let _killian = scenario
        .add_creature_from_oracle(P0, "Killian, Decisive Mentor", 2, 4, KILLIAN_TRIGGER)
        .id();
    // Opponent's attacker, enchanted by P0's Aura.
    let opp_attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    attach_p0_aura(&mut runner, opp_attacker);

    runner.state_mut().active_player = P1;
    let before = hand_size(&runner, P0);

    runner.pass_both_players();
    runner
        .declare_attackers(&[(opp_attacker, AttackTarget::Player(P0))])
        .expect("P1 declares its enchanted creature as an attacker");
    settle(&mut runner);

    assert_eq!(
        hand_size(&runner, P0),
        before + 1,
        "Killian must draw when an opponent's creature enchanted by P0's Aura attacks (CR 303.4e)"
    );
}

/// (2) CR 508.3a decoupling discriminator: P1's creature enchanted by P0's Aura
/// attacks a PLANESWALKER (controlled by P0). Killian must still draw — the
/// trigger has no "attack a player" restriction.
///
/// If the old conflation block in `matching_you_attack_pairs` is still present,
/// `valid_target == Player` would suppress every non-player attack target, so
/// this attack at a planeswalker would NOT draw. The fix removes that block; the
/// attacked-target narrowing lives solely in `attack_target_filter`, which
/// Killian leaves unset.
#[test]
fn killian_fires_when_enchanted_creature_attacks_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);

    let _killian = scenario
        .add_creature_from_oracle(P0, "Killian, Decisive Mentor", 2, 4, KILLIAN_TRIGGER)
        .id();
    // P0 controls the planeswalker (it is the defending, non-active player).
    let pw = scenario.add_creature(P0, "Test Planeswalker", 0, 0).id();
    let opp_attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    make_planeswalker(&mut runner, pw, 5);
    attach_p0_aura(&mut runner, opp_attacker);

    runner.state_mut().active_player = P1;
    let before = hand_size(&runner, P0);

    runner.pass_both_players();
    runner
        .declare_attackers(&[(opp_attacker, AttackTarget::Planeswalker(pw))])
        .expect("P1 declares its enchanted creature attacking P0's planeswalker");
    settle(&mut runner);

    assert_eq!(
        hand_size(&runner, P0),
        before + 1,
        "Killian must draw even when the enchanted attacker targets a planeswalker (CR 508.3a)"
    );
}

/// (3) The trigger must stay silent when the attacker is NOT enchanted by a
/// P0-controlled Aura.
#[test]
fn killian_silent_on_unenchanted_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);

    let _killian = scenario
        .add_creature_from_oracle(P0, "Killian, Decisive Mentor", 2, 4, KILLIAN_TRIGGER)
        .id();
    let opp_attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    // No Aura attached.
    runner.state_mut().active_player = P1;
    let before = hand_size(&runner, P0);

    runner.pass_both_players();
    runner
        .declare_attackers(&[(opp_attacker, AttackTarget::Player(P0))])
        .expect("P1 declares an unenchanted attacker");
    settle(&mut runner);

    assert_eq!(
        hand_size(&runner, P0),
        before,
        "Killian must NOT draw when the attacker is not enchanted by a P0 Aura"
    );
}

/// (4) Regression guard for the control-scoped class (the maintainer's explicit
/// concern): a plain "Whenever one or more creatures you control attack" trigger
/// must remain controller-scoped — only its controller's attacks fire it, not an
/// opponent's. Proves the decoupling did not loosen the default path.
#[test]
fn control_scoped_attack_trigger_silent_on_opponent_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);

    // P0-controlled source with the control-scoped trigger.
    let _source = scenario
        .add_creature_from_oracle(P0, "Control Scoped Source", 2, 2, CONTROL_SCOPED_TRIGGER)
        .id();
    let opp_attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let before = hand_size(&runner, P0);

    runner.pass_both_players();
    runner
        .declare_attackers(&[(opp_attacker, AttackTarget::Player(P0))])
        .expect("only P1 attacks");
    settle(&mut runner);

    assert_eq!(
        hand_size(&runner, P0),
        before,
        "a control-scoped 'creatures you control attack' trigger must not fire on an \
         opponent-only attack (CR 506.2)"
    );
}
