//! Integration tests for the active-voice self-reference damage prevention class:
//! "Prevent all damage that this creature would deal to <recipients>."
//!
//! Card class members tested:
//! - Goblin Furrier: "Prevent all damage that this creature would deal to snow creatures."
//! - Indentured Oaf: "Prevent all damage that this creature would deal to red creatures."
//!
//! CR 615.1 / CR 615.1a (damage prevention effects with specific source and recipient scoping),
//! CR 205.4a & CR 205.4g (snow supertype),
//! CR 105.1 (color).
//!
//! Before this fix, active-voice prevention clauses of the form "Prevent all damage
//! that ~ / this creature would deal to <recipients>" failed to isolate the damage source
//! (`damage_source_filter` was left None) and failed to parse "would deal to " (`valid_card`
//! was left None). Consequently, Goblin Furrier registered an unscoped, permanent prevention shield
//! that prevented ALL damage from any source to any target in the entire game.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, PreventionAmount, ShieldKind, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GOBLIN_FURRIER_ORACLE: &str =
    "Prevent all damage that this creature would deal to snow creatures.";

const INDENTURED_OAF_ORACLE: &str =
    "Prevent all damage that this creature would deal to red creatures.";

#[must_use = "combat must be asserted to have actually run"]
fn run_combat(
    runner: &mut GameRunner,
    attacker_player: PlayerId,
    attacks: &[(ObjectId, AttackTarget)],
    defend_player: PlayerId,
    blocks: &[(ObjectId, ObjectId)],
) -> bool {
    let mut attacked = false;
    let mut blocked = false;
    let mut reached_end_of_combat = false;

    for _ in 0..400 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => {
                reached_end_of_combat = true;
                break;
            }
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. }
                if player == attacker_player && !attacked =>
            {
                attacked = true;
                runner
                    .declare_attackers(attacks)
                    .expect("declaring attackers must succeed");
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { player, .. } if player == defend_player && !blocked => {
                blocked = true;
                runner
                    .declare_blockers(blocks)
                    .expect("declaring blockers must succeed");
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    attacked && (blocks.is_empty() || blocked) && reached_end_of_combat
}

// ===========================================================================
// Goblin Furrier (Exact Oracle text: "Prevent all damage that this creature would deal to snow creatures.")
// ===========================================================================

/// The user's exact reported bug:
/// Opponent controls Goblin Furrier. Player attacks with Ohran Yeti (Snow) and
/// two Korvikan Mists (non-snow), unblocked.
///
/// CR 615.1a: Goblin Furrier only prevents damage dealt BY Goblin Furrier
/// TO snow creatures. It must NOT prevent combat damage dealt by other attacking
/// creatures to the defending player.
#[test]
fn goblin_furrier_does_not_prevent_damage_from_attacking_snow_and_nonsnow_creatures_to_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent controls Goblin Furrier
    let goblin = scenario
        .add_creature_from_oracle(P1, "Goblin Furrier", 2, 2, GOBLIN_FURRIER_ORACLE)
        .id();

    // Attacking player controls a Snow creature (Ohran Yeti 3/3) and 2 non-snow creatures (3/3 each)
    let yeti = scenario.add_creature(P0, "Ohran Yeti", 3, 3).as_snow().id();
    let mist1 = scenario.add_creature(P0, "Korvikan Mist", 3, 3).id();
    let mist2 = scenario.add_creature(P0, "Korvikan Mist", 3, 3).id();

    let mut runner = scenario.build();

    // Reach-guard: Verify Goblin Furrier's ReplacementDefinition parsed with correct source and recipient filters
    let defs = &runner.state().objects[&goblin].replacement_definitions;
    assert_eq!(
        defs.len(),
        1,
        "Goblin Furrier must produce exactly 1 damage prevention ReplacementDefinition"
    );
    let repl = &defs[0];
    assert_eq!(
        repl.shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        },
        "prevention amount must be All"
    );
    assert_eq!(
        repl.damage_source_filter,
        Some(TargetFilter::SelfRef),
        "damage_source_filter must be scoped to Goblin Furrier (SelfRef)"
    );
    assert_eq!(
        repl.valid_card,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::HasSupertype {
                value: Supertype::Snow,
            }]
        ))),
        "valid_card filter must target snow creatures: {:?}",
        repl.valid_card
    );
    assert_eq!(
        repl.damage_target_filter, None,
        "damage_target_filter must be None (not player-directed)"
    );

    let p1_life_before = runner.life(P1);
    runner.advance_to_combat();

    let attacks = [
        (yeti, AttackTarget::Player(P1)),
        (mist1, AttackTarget::Player(P1)),
        (mist2, AttackTarget::Player(P1)),
    ];
    assert!(
        run_combat(&mut runner, P0, &attacks, P1, &[]),
        "reach-guard: combat must run successfully with 3 attackers unblocked"
    );
    runner.advance_until_stack_empty();

    // 3 + 3 + 3 = 9 combat damage dealt to P1
    assert_eq!(
        runner.life(P1),
        p1_life_before - 9,
        "Defending player must take full 9 combat damage from unblocked snow and non-snow attackers"
    );
}

/// CR 615.1a: Goblin Furrier attacks and is blocked by a Snow creature (Ohran Yeti).
/// - Goblin Furrier's 2 damage to Ohran Yeti IS prevented.
/// - Ohran Yeti's 3 damage to Goblin Furrier is NOT prevented.
/// - Goblin Furrier takes lethal damage and dies. Ohran Yeti survives with 0 damage marked.
#[test]
fn goblin_furrier_damage_to_snow_creature_is_prevented_while_snow_creature_damages_furrier() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let goblin = scenario
        .add_creature_from_oracle(P0, "Goblin Furrier", 2, 2, GOBLIN_FURRIER_ORACLE)
        .id();
    let yeti = scenario.add_creature(P1, "Ohran Yeti", 3, 3).as_snow().id();

    let mut runner = scenario.build();
    runner.advance_to_combat();

    let attacks = [(goblin, AttackTarget::Player(P1))];
    let blocks = [(yeti, goblin)];
    assert!(
        run_combat(&mut runner, P0, &attacks, P1, &blocks),
        "reach-guard: combat must run successfully with Yeti blocking Furrier"
    );
    runner.advance_until_stack_empty();

    // Yeti takes 0 damage because Furrier's damage to snow creatures is prevented
    assert_eq!(
        runner.state().objects[&yeti].damage_marked,
        0,
        "Ohran Yeti must have taken 0 marked damage from Goblin Furrier"
    );
    assert_eq!(
        runner.state().objects[&yeti].zone,
        Zone::Battlefield,
        "Ohran Yeti must remain on the battlefield"
    );

    // Goblin Furrier dies from Yeti's 3 combat damage (not prevented)
    assert_eq!(
        runner.state().objects[&goblin].zone,
        Zone::Graveyard,
        "Goblin Furrier must die from Ohran Yeti's 3 combat damage"
    );
}

/// CR 615.1a: Goblin Furrier attacks and is blocked by a NON-SNOW creature (Grizzly Bears 2/2).
/// Both deal combat damage normally; both die to lethal combat damage.
#[test]
fn goblin_furrier_deals_combat_damage_normally_to_nonsnow_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let goblin = scenario
        .add_creature_from_oracle(P0, "Goblin Furrier", 2, 2, GOBLIN_FURRIER_ORACLE)
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();

    let attacks = [(goblin, AttackTarget::Player(P1))];
    let blocks = [(bears, goblin)];
    assert!(
        run_combat(&mut runner, P0, &attacks, P1, &blocks),
        "reach-guard: combat must run successfully with Bears blocking Furrier"
    );
    runner.advance_until_stack_empty();

    // Goblin Furrier deals 2 damage to Grizzly Bears (non-snow) -> Bears dies
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "Grizzly Bears must die from Goblin Furrier's combat damage"
    );

    // Bears deals 2 damage to Goblin Furrier -> Furrier dies
    assert_eq!(
        runner.state().objects[&goblin].zone,
        Zone::Graveyard,
        "Goblin Furrier must die from Grizzly Bears' combat damage"
    );
}

/// CR 615.1a: Goblin Furrier attacks the defending player unblocked.
/// Goblin Furrier deals 2 combat damage normally (damage to players is not prevented).
#[test]
fn goblin_furrier_deals_combat_damage_normally_to_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let goblin = scenario
        .add_creature_from_oracle(P0, "Goblin Furrier", 2, 2, GOBLIN_FURRIER_ORACLE)
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);
    runner.advance_to_combat();

    let attacks = [(goblin, AttackTarget::Player(P1))];
    assert!(
        run_combat(&mut runner, P0, &attacks, P1, &[]),
        "reach-guard: combat must run successfully with Furrier unblocked"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "Defending player must take 2 combat damage from Goblin Furrier"
    );
}

/// CR 615.1a: Goblin Furrier blocks an attacking Snow creature (Ohran Yeti).
/// - Goblin Furrier's 2 damage to Ohran Yeti IS prevented.
/// - Ohran Yeti's 3 damage to Goblin Furrier is NOT prevented.
/// - Goblin Furrier dies; Yeti survives with 0 damage marked.
#[test]
fn goblin_furrier_blocking_snow_creature_prevents_furrier_damage_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let yeti = scenario.add_creature(P1, "Ohran Yeti", 3, 3).as_snow().id();
    let goblin = scenario
        .add_creature_from_oracle(P0, "Goblin Furrier", 2, 2, GOBLIN_FURRIER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();

    let attacks = [(yeti, AttackTarget::Player(P0))];
    let blocks = [(goblin, yeti)];
    assert!(
        run_combat(&mut runner, P1, &attacks, P0, &blocks),
        "reach-guard: combat must run successfully with Furrier blocking Yeti"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&yeti].damage_marked,
        0,
        "Ohran Yeti must have taken 0 marked damage from blocking Goblin Furrier"
    );
    assert_eq!(
        runner.state().objects[&yeti].zone,
        Zone::Battlefield,
        "Ohran Yeti must survive on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&goblin].zone,
        Zone::Graveyard,
        "Goblin Furrier must die from Ohran Yeti's 3 combat damage"
    );
}

// ===========================================================================
// Indentured Oaf (Exact Oracle text: "Prevent all damage that this creature would deal to red creatures.")
// Sibling card demonstrating the active-voice self-reference prevention class.
// ===========================================================================

/// CR 615.1 + CR 615.1a: Indentured Oaf (4/3) attacks and is blocked by a red creature (Goblin Raider 2/2).
/// - Indentured Oaf's 4 combat damage to the red creature IS prevented.
/// - The red creature's 2 combat damage to Indentured Oaf is NOT prevented.
/// - Goblin Raider survives with 0 marked damage; Indentured Oaf survives with 2 marked damage.
#[test]
fn indentured_oaf_damage_to_red_creature_is_prevented_while_red_creature_damages_oaf() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let oaf = scenario
        .add_creature_from_oracle(P0, "Indentured Oaf", 4, 3, INDENTURED_OAF_ORACLE)
        .id();
    let raider = scenario
        .add_creature(P1, "Goblin Raider", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();

    let mut runner = scenario.build();

    // Reach-guard: Verify Indentured Oaf's ReplacementDefinition parsed with correct source and recipient filters
    let defs = &runner.state().objects[&oaf].replacement_definitions;
    assert_eq!(
        defs.len(),
        1,
        "Indentured Oaf must produce exactly 1 damage prevention ReplacementDefinition"
    );
    let repl = &defs[0];
    assert_eq!(
        repl.shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        },
        "prevention amount must be All"
    );
    assert_eq!(
        repl.damage_source_filter,
        Some(TargetFilter::SelfRef),
        "damage_source_filter must be scoped to Indentured Oaf (SelfRef)"
    );
    assert_eq!(
        repl.valid_card,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::HasColor {
                color: ManaColor::Red,
            }]
        ))),
        "valid_card filter must target red creatures: {:?}",
        repl.valid_card
    );

    runner.advance_to_combat();

    let attacks = [(oaf, AttackTarget::Player(P1))];
    let blocks = [(raider, oaf)];
    assert!(
        run_combat(&mut runner, P0, &attacks, P1, &blocks),
        "reach-guard: combat must run successfully with Raider blocking Oaf"
    );
    runner.advance_until_stack_empty();

    // Raider takes 0 damage because Oaf's damage to red creatures is prevented
    assert_eq!(
        runner.state().objects[&raider].damage_marked,
        0,
        "Goblin Raider must have taken 0 marked damage from Indentured Oaf"
    );
    assert_eq!(
        runner.state().objects[&raider].zone,
        Zone::Battlefield,
        "Goblin Raider must survive on the battlefield"
    );

    // Indentured Oaf takes 2 damage from Raider (not prevented)
    assert_eq!(
        runner.state().objects[&oaf].damage_marked,
        2,
        "Indentured Oaf must have taken 2 marked damage from Goblin Raider"
    );
    assert_eq!(
        runner.state().objects[&oaf].zone,
        Zone::Battlefield,
        "Indentured Oaf (toughness 3) must survive on the battlefield with 2 marked damage"
    );
}

/// CR 615.1a: Indentured Oaf attacks and is blocked by a non-red creature (Grizzly Bears 2/2, Green).
/// Indentured Oaf deals 4 combat damage to Grizzly Bears normally; Grizzly Bears dies.
#[test]
fn indentured_oaf_deals_combat_damage_normally_to_nonred_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let oaf = scenario
        .add_creature_from_oracle(P0, "Indentured Oaf", 4, 3, INDENTURED_OAF_ORACLE)
        .id();
    let bears = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();

    let attacks = [(oaf, AttackTarget::Player(P1))];
    let blocks = [(bears, oaf)];
    assert!(
        run_combat(&mut runner, P0, &attacks, P1, &blocks),
        "reach-guard: combat must run successfully with Bears blocking Oaf"
    );
    runner.advance_until_stack_empty();

    // Indentured Oaf deals 4 damage to Grizzly Bears (non-red) -> Bears dies
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "Grizzly Bears must die from Indentured Oaf's combat damage"
    );

    // Bears deals 2 damage to Indentured Oaf -> Oaf survives with 2 damage marked
    assert_eq!(
        runner.state().objects[&oaf].damage_marked,
        2,
        "Indentured Oaf must take 2 damage from Grizzly Bears"
    );
    assert_eq!(
        runner.state().objects[&oaf].zone,
        Zone::Battlefield,
        "Indentured Oaf must survive on the battlefield"
    );
}

/// CR 615.1a: Indentured Oaf attacks the defending player unblocked.
/// Deals 4 combat damage normally (damage to players is not prevented).
#[test]
fn indentured_oaf_deals_combat_damage_normally_to_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let oaf = scenario
        .add_creature_from_oracle(P0, "Indentured Oaf", 4, 3, INDENTURED_OAF_ORACLE)
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);
    runner.advance_to_combat();

    let attacks = [(oaf, AttackTarget::Player(P1))];
    assert!(
        run_combat(&mut runner, P0, &attacks, P1, &[]),
        "reach-guard: combat must run successfully with Oaf unblocked"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 4,
        "Defending player must take 4 combat damage from Indentured Oaf"
    );
}
