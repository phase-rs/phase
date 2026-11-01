//! Grim Repriser (FRA): "{B}{R}: Return this card from your graveyard to the
//! battlefield with a finality counter on it. Activate only if an opponent has
//! been dealt noncombat damage this turn." Exercises the
//! `DamageDealtThisTurn { NoncombatOnly }` activation gate end to end through
//! `ActivationRestriction::RequiresCondition` (`can_activate_ability_now`), not
//! just that it parses.
//!
//! CR 602.5 governs the gate: the "activate only if" condition must hold for
//! the activation to be legal. CR 120.2b classifies Shock's damage as noncombat
//! (it opens the gate) and combat-step damage as combat (it must not).

use super::rules::run_combat;
use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall `oracle_text` for "Grim Repriser", FRA #136).
/// A paraphrase could take a different parser branch, so the printed text is
/// used exactly as printed.
const GRIM_ORACLE: &str = "Prowess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)\n{B}{R}: Return this card from your graveyard to the battlefield with a finality counter on it. Activate only if an opponent has been dealt noncombat damage this turn. (If a creature with a finality counter on it would die, exile it instead.)";

/// CR 120.2b: Shock deals noncombat damage — damage dealt by a spell effect,
/// not in the combat damage step.
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

fn floating(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

fn offers_activation(state: &engine::types::game_state::GameState, object: ObjectId) -> bool {
    legal_actions(state).iter().any(|action| {
        matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == object)
    })
}

fn grim_ability_index(state: &engine::types::game_state::GameState, grim: ObjectId) -> usize {
    state.objects[&grim]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                *ability.effect,
                engine::types::ability::Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    ..
                }
            )
        })
        .expect("Grim Repriser must have a Graveyard -> Battlefield return ability")
}

/// Give `player` priority in their own pre-combat main so offer checks read the
/// acting player's legal actions (mirrors the other activated-ability
/// integration tests).
fn hand_priority(runner: &mut GameRunner, player: PlayerId) {
    runner.state_mut().active_player = player;
    runner.state_mut().priority_player = player;
    runner.state_mut().waiting_for = WaitingFor::Priority { player };
}

/// Grim Repriser, built from its verbatim Oracle text. The Prowess keyword line
/// needs its explicit keyword-name hint (as in production MTGJSON) or it parses
/// to `Effect::Unimplemented`.
fn add_grim(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Grim Repriser", 2, 2)
        .from_oracle_text_with_keywords(&["Prowess"], GRIM_ORACLE)
        .id()
}

fn add_shock(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id()
}

/// CR 602.5: with no damage dealt this turn, the activation must be refused
/// and the card must stay in the graveyard. {B}{R} is funded, so the refusal is
/// attributable to the condition gate, not the cost.
#[test]
fn grim_activation_refused_with_no_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grim = add_grim(&mut scenario);
    scenario.with_mana_pool(P0, floating(&[ManaType::Black, ManaType::Red]));
    let mut runner = scenario.build();

    let ability_index = grim_ability_index(runner.state(), grim);
    assert!(
        !offers_activation(runner.state(), grim),
        "the return ability must not be offered with no damage dealt this turn"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: grim,
                ability_index,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .is_err(),
        "the submit path must reject the activation too"
    );
    assert_eq!(runner.state().objects[&grim].zone, Zone::Graveyard);
}

/// CR 602.5 + CR 120.2b: after Shock deals noncombat damage to an opponent
/// through the real cast pipeline, the gate opens and the activation resolves,
/// returning Grim Repriser with a finality counter.
#[test]
fn grim_activation_resolves_after_noncombat_damage_to_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grim = add_grim(&mut scenario);
    let shock = add_shock(&mut scenario);
    scenario.with_mana_pool(
        P0,
        floating(&[ManaType::Black, ManaType::Red, ManaType::Red]),
    );
    let mut runner = scenario.build();

    // CR 120.2b: Shock's 2 damage to the opponent is noncombat damage through
    // the production cast pipeline.
    let shock_outcome = runner.cast(shock).target_player(P1).resolve();
    shock_outcome.assert_life_delta(P1, -2);

    let ability_index = grim_ability_index(runner.state(), grim);
    assert!(
        offers_activation(runner.state(), grim),
        "the return ability must be offered after an opponent was dealt noncombat damage"
    );
    let outcome = runner.activate(grim, ability_index).resolve();
    outcome.assert_zone(&[grim], Zone::Battlefield);
    assert_eq!(
        *outcome.state().objects[&grim]
            .counters
            .get(&CounterType::Finality)
            .unwrap_or(&0),
        1,
        "the returned card must carry a finality counter"
    );
}

/// CR 120.2a: combat damage alone must NOT open a noncombat-only gate. The life
/// assertion is the reach-guard — the opponent really was dealt combat damage —
/// so the absent offer pins the damage class, not a missing event.
#[test]
fn grim_activation_refused_after_combat_damage_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grim = add_grim(&mut scenario);
    let attacker = scenario.add_creature(P0, "Vanilla Attacker", 2, 2).id();
    scenario.with_mana_pool(P0, floating(&[ManaType::Black, ManaType::Red]));
    let mut runner = scenario.build();

    // CR 510 + CR 120.2a: an unblocked 2/2 deals 2 COMBAT damage to the
    // opponent in the combat damage step.
    run_combat(&mut runner, vec![attacker], vec![]);
    hand_priority(&mut runner, P0);
    // CR 106.4: phase and step boundaries empty ordinary floating mana.
    // Refill after combat so cost availability cannot mask a restriction bug.
    for mana in floating(&[ManaType::Black, ManaType::Red]) {
        runner
            .state_mut()
            .add_mana_to_pool(P0, mana)
            .expect("P0 must exist for the combat-control mana refill");
    }
    for color in [ManaType::Black, ManaType::Red] {
        assert_eq!(
            runner.state().players[0].mana_pool.count_color(color),
            1,
            "the return cost must be funded after combat"
        );
    }
    assert_eq!(
        runner.life(P1),
        18,
        "reach-guard: the opponent was really dealt 2 combat damage"
    );

    let ability_index = grim_ability_index(runner.state(), grim);
    assert!(
        !offers_activation(runner.state(), grim),
        "combat damage must not open a noncombat-only activation gate"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: grim,
                ability_index,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .is_err(),
        "the submit path must reject the activation too"
    );
    assert_eq!(runner.state().objects[&grim].zone, Zone::Graveyard);
}

/// CR 120.1 + CR 120.3: damage dealt to an opponent's CREATURE is a different
/// event from damage dealt to that opponent, so it must not open the gate. The
/// marked-damage assertion is the reach-guard — the Shock really hit the
/// creature — and the unchanged life total pins the recipient.
#[test]
fn grim_activation_refused_after_damage_to_creature_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grim = add_grim(&mut scenario);
    let bearer = scenario.add_creature(P1, "Shield Bearer", 2, 4).id();
    let shock = add_shock(&mut scenario);
    scenario.with_mana_pool(
        P0,
        floating(&[ManaType::Black, ManaType::Red, ManaType::Red]),
    );
    let mut runner = scenario.build();

    let _ = runner.cast(shock).target_object(bearer).resolve();
    assert!(
        runner.state().objects[&bearer].damage_marked > 0,
        "reach-guard: the Shock really dealt damage to the creature"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "the opponent themself was dealt no damage"
    );

    let ability_index = grim_ability_index(runner.state(), grim);
    assert!(
        !offers_activation(runner.state(), grim),
        "damage to an opponent's creature must not open the opponent-damage gate"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: grim,
                ability_index,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .is_err(),
        "the submit path must reject the activation too"
    );
    assert_eq!(runner.state().objects[&grim].zone, Zone::Graveyard);
}
