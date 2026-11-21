//! Burden of Proof through the real cast and block pipeline.
//!
//! Oracle text (MTGJSON): "Flash / Enchant creature / Enchanted creature gets
//! +2/+2 as long as it's a Detective you control. Otherwise, it has base power
//! and toughness 1/1 and can't block Detectives."
//!
//! Clause reading:
//!   * "a Detective you control": Detective is a creature type (CR 205.3m). For
//!     a static ability, "you" is the current controller of the object it's on
//!     (CR 109.5) — the Aura's controller, who is a separate player from the
//!     enchanted creature's controller (CR 303.4e).
//!   * "Otherwise, it has base power and toughness 1/1": layer 7b (CR 613.4b).
//!   * "and can't block Detectives": a blocking restriction (CR 509.1b). A
//!     declaration in which the creature blocks a Detective is illegal; it may
//!     block any attacker that is not a Detective. "Detectives" names no
//!     controller, so nothing here is controller-relative.
//!   * The restriction is an effect of the Aura's own static ability
//!     (CR 509.1b + CR 611.3a), not an ability of the enchanted creature, so a
//!     later "Creatures lose all abilities" (layer 6, CR 613.1f) does not end
//!     it. Dress Down (MTGJSON): "Flash / When this enchantment enters, draw a
//!     card. / Creatures lose all abilities. / At the beginning of the end
//!     step, sacrifice this enchantment."
//!
//! Ruling: "Once a Detective has been blocked, attaching Burden of Proof to a
//! creature blocking it won't cause that Detective to become unblocked." The
//! restriction is checked when blockers are declared (CR 509.1b), and blocked
//! status does not change afterwards (CR 509.1h). These rows assert declaration
//! legality only; they claim nothing about an attachment made after blocks.
//!
//! Two block-window facts shape every fixture:
//!   * after `declare_attackers` the state is a priority window (CR 508.2), so
//!     both players pass before `WaitingFor::DeclareBlockers` (CR 509.1);
//!   * the declare-blockers prompt is auto-submitted when the defending player
//!     has no legal block, so the defending player always controls a second
//!     creature that may legally block the attacker. Each declaration runs on a
//!     cloned state, because a successful one advances the step.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{TargetFilter, TypedFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const BURDEN_OF_PROOF: &str = "Flash\nEnchant creature\nEnchanted creature gets +2/+2 as long as it's a Detective you control. Otherwise, it has base power and toughness 1/1 and can't block Detectives.";
const VENGEFUL_TRACKER: &str =
    "Whenever an opponent sacrifices an artifact, this creature deals 2 damage to them.";
const DRESS_DOWN: &str = "Flash\nWhen this enchantment enters, draw a card.\nCreatures lose all abilities.\nAt the beginning of the end step, sacrifice this enchantment.";
const BLOCK_RESTRICTION_ERROR: &str = "can block only creatures matching the block restriction";

/// Vengeful Tracker: Human Detective, 2/2.
fn add_vengeful_tracker(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, "Vengeful Tracker", 2, 2, VENGEFUL_TRACKER)
        .with_subtypes(vec!["Human", "Detective"])
        .id()
}

/// Grizzly Bears: vanilla Bear, 2/2.
fn add_grizzly_bears(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id()
}

/// Two players in P0's precombat main phase, with libraries and two Islands for
/// P0 to cast Burden of Proof ({1}{U}).
fn base_scenario() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..2 {
        for _ in 0..8 {
            scenario.add_card_to_library_top(PlayerId(seat), "Filler");
        }
    }
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario
}

fn add_burden_of_proof(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_to_hand_from_oracle(P0, "Burden of Proof", BURDEN_OF_PROOF)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id()
}

/// Dress Down ({1}{U}) in P0's hand.
fn add_dress_down(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_to_hand_from_oracle(P0, "Dress Down", DRESS_DOWN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id()
}

/// CR 601.2 + CR 608.3c: cast the Aura on `host` and resolve it, then check the
/// cast completed: empty stack, a priority window, the Aura on the battlefield.
fn cast_burden_of_proof_on(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    runner.cast(aura).target_object(host).resolve();
    let state = runner.state();
    assert!(state.stack.is_empty(), "the Aura spell must have resolved");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a priority window, got {:?}",
        state.waiting_for.variant_name()
    );
    assert_eq!(state.objects[&aura].zone, Zone::Battlefield);
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &runner.state().objects[&id];
    (object.power, object.toughness)
}

fn block_restrictions(runner: &GameRunner, id: ObjectId) -> Vec<StaticMode> {
    runner.state().objects[&id]
        .static_definitions
        .iter_all()
        .filter(|def| matches!(def.mode, StaticMode::BlockRestriction { .. }))
        .map(|def| def.mode.clone())
        .collect()
}

/// The block restrictions the Aura itself hosts (its own static abilities).
fn aura_block_restrictions(runner: &GameRunner, aura: ObjectId) -> Vec<StaticMode> {
    block_restrictions(runner, aura)
}

/// CR 509.1b: "can't block Detectives" is the whitelist "can block only
/// non-Detectives".
fn cant_block_detectives() -> StaticMode {
    StaticMode::BlockRestriction {
        filter: TargetFilter::Not {
            filter: Box::new(TargetFilter::Typed(
                TypedFilter::default().subtype("Detective".to_string()),
            )),
        },
    }
}

/// On a clone of `runner`, declare `attacker` attacking `defender` and pass
/// priority (CR 508.2) to the defender's declare-blockers prompt (CR 509.1).
fn to_declare_blockers(runner: &GameRunner, attacker: ObjectId, defender: PlayerId) -> GameRunner {
    let mut combat = GameRunner::from_state(runner.state().clone());
    combat.advance_to_combat();
    combat
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("the attack must be legal");
    combat.pass_both_players();
    match &combat.state().waiting_for {
        WaitingFor::DeclareBlockers { player, .. } => assert_eq!(*player, defender),
        other => panic!(
            "expected the declare-blockers prompt, got {:?}",
            other.variant_name()
        ),
    }
    combat
}

/// Like [`to_declare_blockers`], but P0 casts `flash` (CR 702.8a) in the
/// declare-attackers priority window (CR 508.2) and resolves it before both
/// players pass to the defender's declare-blockers prompt (CR 509.1).
fn to_declare_blockers_after_flash(
    runner: &GameRunner,
    attacker: ObjectId,
    defender: PlayerId,
    flash: ObjectId,
) -> GameRunner {
    let mut combat = GameRunner::from_state(runner.state().clone());
    combat.advance_to_combat();
    combat
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("the attack must be legal");
    assert!(
        matches!(
            combat.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "the declare-attackers step must open a priority window, got {:?}",
        combat.state().waiting_for.variant_name()
    );
    combat.cast(flash).resolve();
    combat.advance_until_stack_empty();
    let state = combat.state();
    assert!(state.stack.is_empty(), "the flash spell must have resolved");
    assert_eq!(state.phase, Phase::DeclareAttackers);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a priority window, got {:?}",
        state.waiting_for.variant_name()
    );
    assert_eq!(state.objects[&flash].zone, Zone::Battlefield);
    combat.pass_both_players();
    match &combat.state().waiting_for {
        WaitingFor::DeclareBlockers { player, .. } => assert_eq!(*player, defender),
        other => panic!(
            "expected the declare-blockers prompt, got {:?}",
            other.variant_name()
        ),
    }
    combat
}

fn valid_blockers(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            valid_blocker_ids, ..
        } => valid_blocker_ids.clone(),
        other => panic!(
            "expected the declare-blockers prompt, got {:?}",
            other.variant_name()
        ),
    }
}

/// Submit one block on a clone, so the prompt stays available to later rows.
fn try_block(runner: &GameRunner, blocker: ObjectId, attacker: ObjectId) -> Result<(), String> {
    let mut clone = GameRunner::from_state(runner.state().clone());
    clone
        .declare_blockers(&[(blocker, attacker)])
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// P1-V1: P0's Aura on P1's non-Detective. The Otherwise branch applies: the
/// creature is 1/1, can't block a Detective, and can still block a
/// non-Detective (a whitelist, not a blanket "can't block").
#[test]
fn otherwise_the_enchanted_creature_cannot_block_detectives() {
    let mut scenario = base_scenario();
    let tracker = add_vengeful_tracker(&mut scenario, P0);
    let p0_bears = add_grizzly_bears(&mut scenario, P0);
    let bears_a = add_grizzly_bears(&mut scenario, P1);
    let bears_b = add_grizzly_bears(&mut scenario, P1);
    let aura = add_burden_of_proof(&mut scenario);
    let mut runner = scenario.build();
    cast_burden_of_proof_on(&mut runner, aura, bears_a);

    // Reach guard: the Otherwise def is live. CR 613.4b: base P/T 1/1.
    assert_eq!(power_toughness(&runner, bears_a), (Some(1), Some(1)));
    assert_eq!(
        aura_block_restrictions(&runner, aura),
        vec![cant_block_detectives()]
    );
    assert!(
        block_restrictions(&runner, bears_a).is_empty(),
        "the restriction is the Aura's effect, not an ability grafted onto the creature"
    );

    // Window 1: the Detective attacks.
    let detective_attack = to_declare_blockers(&runner, tracker, P1);
    let valid = valid_blockers(&detective_attack);
    // CR 509.1b: the enchanted creature can't block a Detective.
    assert!(
        !valid.contains(&bears_a),
        "the enchanted creature must not be offered against a Detective: {valid:?}"
    );
    let refused = try_block(&detective_attack, bears_a, tracker)
        .expect_err("blocking a Detective must be illegal");
    assert!(
        refused.contains(BLOCK_RESTRICTION_ERROR),
        "the refusal must come from the block restriction: {refused}"
    );
    // Paired positive: the prompt is real and the defender has a legal block.
    assert!(valid.contains(&bears_b), "{valid:?}");
    assert_eq!(try_block(&detective_attack, bears_b, tracker), Ok(()));

    // Window 2: a non-Detective attacks. CR 509.1b: the whitelist admits it.
    let bears_attack = to_declare_blockers(&runner, p0_bears, P1);
    let valid = valid_blockers(&bears_attack);
    assert!(
        valid.contains(&bears_a),
        "the enchanted creature may block a non-Detective: {valid:?}"
    );
    assert_eq!(try_block(&bears_attack, bears_a, p0_bears), Ok(()));
}

/// P1-V2: P0's Aura on P0's own Detective. CR 109.5: "you" is the Aura's
/// controller, who controls this Detective, so the +2/+2 branch applies and the
/// creature is not restricted. Not revert-failing by design: a hostile sibling
/// against an over-restricting change.
#[test]
fn a_detective_its_controller_controls_can_block_detectives() {
    let mut scenario = base_scenario();
    let p0_tracker = add_vengeful_tracker(&mut scenario, P0);
    let p0_bears = add_grizzly_bears(&mut scenario, P0);
    let p1_tracker = add_vengeful_tracker(&mut scenario, P1);
    let aura = add_burden_of_proof(&mut scenario);
    let mut runner = scenario.build();
    cast_burden_of_proof_on(&mut runner, aura, p0_tracker);

    // Reach guard: the "as long as" def applies (2/2 + 2/2); no restriction.
    assert_eq!(power_toughness(&runner, p0_tracker), (Some(4), Some(4)));
    assert!(block_restrictions(&runner, p0_tracker).is_empty());

    // Production priority passing to P1's turn.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("declaring no attackers is legal");
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ),
        "P1 must hold priority in its main phase, got {:?}",
        runner.state().waiting_for.variant_name()
    );

    let detective_attack = to_declare_blockers(&runner, p1_tracker, P0);
    let valid = valid_blockers(&detective_attack);
    assert!(
        valid.contains(&p0_bears),
        "second legal blocker keeps the prompt real: {valid:?}"
    );
    assert!(
        valid.contains(&p0_tracker),
        "the enchanted Detective may block a Detective: {valid:?}"
    );
    assert_eq!(try_block(&detective_attack, p0_tracker, p1_tracker), Ok(()));
}

/// P1-V3: P0's Aura on P1's Detective. CR 109.5 + CR 303.4e: the Aura's
/// controller does not control that Detective, so the Otherwise branch applies:
/// 1/1 and can't block Detectives (CR 509.1b). Under the enchanted creature's
/// controller reading it would be 4/4 and unrestricted.
#[test]
fn an_opponents_detective_it_enchants_cannot_block_detectives() {
    let mut scenario = base_scenario();
    let p0_tracker = add_vengeful_tracker(&mut scenario, P0);
    let _p0_bears = add_grizzly_bears(&mut scenario, P0);
    let p1_tracker = add_vengeful_tracker(&mut scenario, P1);
    let p1_bears = add_grizzly_bears(&mut scenario, P1);
    let aura = add_burden_of_proof(&mut scenario);
    let mut runner = scenario.build();
    cast_burden_of_proof_on(&mut runner, aura, p1_tracker);

    // Reach guard: the Otherwise def is live on the opponent's Detective.
    assert_eq!(power_toughness(&runner, p1_tracker), (Some(1), Some(1)));
    assert_eq!(
        aura_block_restrictions(&runner, aura),
        vec![cant_block_detectives()]
    );
    assert!(
        block_restrictions(&runner, p1_tracker).is_empty(),
        "the restriction is the Aura's effect, not an ability grafted onto the creature"
    );

    let detective_attack = to_declare_blockers(&runner, p0_tracker, P1);
    let valid = valid_blockers(&detective_attack);
    // CR 509.1b: the enchanted Detective can't block a Detective.
    assert!(
        !valid.contains(&p1_tracker),
        "the enchanted Detective must not be offered against a Detective: {valid:?}"
    );
    let refused = try_block(&detective_attack, p1_tracker, p0_tracker)
        .expect_err("blocking a Detective must be illegal");
    assert!(
        refused.contains(BLOCK_RESTRICTION_ERROR),
        "the refusal must come from the block restriction: {refused}"
    );
    // Paired positive: the prompt is real.
    assert!(valid.contains(&p1_bears), "{valid:?}");
}

/// P1-V4 (maintainer review F1): P0's Aura on P1's non-Detective, then P0
/// flashes in Dress Down before blockers. CR 613.1f: "Creatures lose all
/// abilities" removes the creatures' abilities, but the restriction is the
/// Aura's effect (CR 509.1b + CR 611.3a), so the enchanted creature still
/// can't block a Detective and may still block a non-Detective. The Aura's
/// controller (P0) is not the enchanted creature's controller (P1, CR 303.4e).
#[test]
fn a_later_lose_all_abilities_effect_does_not_end_the_otherwise_restriction() {
    let mut scenario = base_scenario();
    // Two more Islands: {1}{U} for Burden of Proof and {1}{U} for Dress Down.
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let tracker = add_vengeful_tracker(&mut scenario, P0);
    let p0_bears = add_grizzly_bears(&mut scenario, P0);
    let bears_a = add_grizzly_bears(&mut scenario, P1);
    let bears_b = add_grizzly_bears(&mut scenario, P1);
    let aura = add_burden_of_proof(&mut scenario);
    let dress_down = add_dress_down(&mut scenario);
    let mut runner = scenario.build();
    cast_burden_of_proof_on(&mut runner, aura, bears_a);

    // Pre-guard: the Tracker has its printed trigger before Dress Down.
    assert!(
        !runner.state().objects[&tracker]
            .trigger_definitions
            .is_empty(),
        "the Tracker's printed trigger must be present before Dress Down"
    );

    // Window 1: the Detective attacks; Dress Down resolves before blockers.
    let detective_attack = to_declare_blockers_after_flash(&runner, tracker, P1, dress_down);
    // Reach guards. CR 613.1f: Dress Down is in force.
    assert!(
        detective_attack.state().objects[&tracker]
            .trigger_definitions
            .is_empty(),
        "Dress Down must have removed the Tracker's trigger"
    );
    // CR 613.4b: the Otherwise def is the Aura's effect and survives.
    assert_eq!(
        power_toughness(&detective_attack, bears_a),
        (Some(1), Some(1))
    );
    let valid = valid_blockers(&detective_attack);
    // CR 509.1b: the enchanted creature still can't block a Detective.
    assert!(
        !valid.contains(&bears_a),
        "the enchanted creature must not be offered against a Detective after Dress Down: {valid:?}"
    );
    let refused = try_block(&detective_attack, bears_a, tracker)
        .expect_err("blocking a Detective must be illegal after Dress Down");
    assert!(
        refused.contains(BLOCK_RESTRICTION_ERROR),
        "the refusal must come from the block restriction: {refused}"
    );
    // Paired positive: the prompt is real and the defender has a legal block.
    assert!(valid.contains(&bears_b), "{valid:?}");
    assert_eq!(try_block(&detective_attack, bears_b, tracker), Ok(()));

    // Window 2: a non-Detective attacks. CR 509.1b: the whitelist admits it.
    let bears_attack = to_declare_blockers_after_flash(&runner, p0_bears, P1, dress_down);
    assert!(
        bears_attack.state().objects[&tracker]
            .trigger_definitions
            .is_empty(),
        "Dress Down must have removed the Tracker's trigger"
    );
    assert_eq!(power_toughness(&bears_attack, bears_a), (Some(1), Some(1)));
    let valid = valid_blockers(&bears_attack);
    assert!(
        valid.contains(&bears_a),
        "the enchanted creature may block a non-Detective after Dress Down: {valid:?}"
    );
    assert_eq!(try_block(&bears_attack, bears_a, p0_bears), Ok(()));
}
