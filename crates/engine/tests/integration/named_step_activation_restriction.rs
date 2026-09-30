//! CR 602.1b + CR 602.5 + CR 506.1 + CR 511.1 + CR 511.3: "Activate only during
//! the <step> step" names one turn step with no turn owner. The restriction is a
//! live predicate over the current step, enforced when a player begins to activate
//! the ability (CR 602.5), whichever player's turn it is.
//!
//! Every Oracle string below is the card's verbatim text.

use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::restrictions::check_activation_restrictions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ActivationRestriction, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const DESERT: &str = "{T}: Add {C}.\n{T}: This land deals 1 damage to target attacking creature. Activate only during the end of combat step.";
const LESSER_WEREWOLF: &str = "{B}: If this creature's power is 1 or more, it gets -1/-0 until end of turn and put a -0/-1 counter on target creature blocking or blocked by this creature. Activate only during the declare blockers step.";
const KONGMINGS_CONTRAPTIONS: &str = "{T}: This creature deals 2 damage to target attacking creature. Activate only during the declare attackers step and only if you've been attacked this step.";
const BALDUVIAN_WARLORD: &str = "{T}: Remove target blocking creature from combat. Creatures it was blocking that hadn't become blocked by another creature this combat become unblocked, then it blocks an attacking creature of your choice. Activate only during the declare blockers step.";

/// CR 500.1: every step and phase of a turn, in order.
const ALL_PHASES: [Phase; 12] = [
    Phase::Untap,
    Phase::Upkeep,
    Phase::Draw,
    Phase::PreCombatMain,
    Phase::BeginCombat,
    Phase::DeclareAttackers,
    Phase::DeclareBlockers,
    Phase::CombatDamage,
    Phase::EndCombat,
    Phase::PostCombatMain,
    Phase::End,
    Phase::Cleanup,
];

/// Index of the source's only restricted activated ability.
fn restricted_ability(
    runner: &GameRunner,
    source: ObjectId,
) -> (usize, Vec<ActivationRestriction>) {
    runner.state().objects[&source]
        .abilities
        .iter()
        .enumerate()
        .find(|(_, ability)| !ability.activation_restrictions.is_empty())
        .map(|(index, ability)| (index, ability.activation_restrictions.clone()))
        .expect("the restricted ability must survive Oracle parsing")
}

/// CR 117.3d: once the active player (P0) passes, the defending player (P1)
/// receives priority in the same step.
fn give_p1_priority(runner: &mut GameRunner, phase: Phase) {
    assert_eq!(runner.state().phase, phase, "expected to be in {phase:?}");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0) {
        runner
            .act(GameAction::PassPriority)
            .expect("P0 passes priority");
    }
    assert_eq!(
        runner.state().phase,
        phase,
        "P0's pass must not end {phase:?}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 must hold priority in {phase:?}, got {:?}",
        runner.state().waiting_for
    );
}

/// P1 passes with an empty stack; the step ends (CR 117.4 / CR 500.2).
fn p1_passes_step(runner: &mut GameRunner) {
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");
}

/// P1 (holding priority) tries to activate `source`'s ability during a step the
/// instruction does not name. CR 602.5: the attempt is refused by the
/// activation restriction itself, not by an unrelated gate.
fn assert_restriction_refuses(
    runner: &mut GameRunner,
    source: ObjectId,
    ability_index: usize,
    card_name: &str,
) {
    let phase = runner.state().phase;
    match runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index,
    }) {
        Err(EngineError::ActionNotAllowed(message)) => {
            assert!(
                message.starts_with("Activation restriction not satisfied")
                    && message.contains("CurrentPhaseIs"),
                "{card_name} in {phase:?} must be refused by its step restriction, got {message:?}"
            );
        }
        other => panic!("{card_name} must not be activatable during {phase:?}, got {other:?}"),
    }
}

fn black_mana() -> ManaUnit {
    ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])
}

/// CR 500.5: mana empties as each step ends, so the {B} is re-seeded before each
/// attempt; the affordability gate ("Cannot pay activation cost") can then never
/// be the reason an attempt is refused.
fn seed_one_black(runner: &mut GameRunner) {
    let pool = &mut runner.state_mut().players[1].mana_pool;
    pool.mana.clear();
    pool.add(black_mana());
}

/// CR 511.1 + CR 511.3: Desert's damage ability is activatable only during the
/// end of combat step — after combat damage (Scryfall ruling), while attacking
/// creatures are still attacking — and is refused in every earlier combat step,
/// even on the attacking player's own creature's attack.
#[test]
fn desert_activates_only_during_end_of_combat_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Test Bear", 2, 2).id();
    let desert = scenario.add_land_from_oracle(P1, "Desert", DESERT).id();
    let mut runner = scenario.build();
    let (index, restrictions) = restricted_ability(&runner, desert);
    assert!(
        format!("{restrictions:?}").contains("CurrentPhaseIs"),
        "reach-guard: Desert carries the typed step restriction, got {restrictions:?}"
    );
    let p1_life_before = runner.life(P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // CR 508.1: declare attackers step — refused.
    give_p1_priority(&mut runner, Phase::DeclareAttackers);
    assert_restriction_refuses(&mut runner, desert, index, "Desert");
    assert!(!runner.state().objects[&desert].tapped);
    p1_passes_step(&mut runner);

    // CR 509.1: declare blockers step — refused. P1 controls no creature, so no
    // blocker declaration is solicited and the step opens straight to priority.
    give_p1_priority(&mut runner, Phase::DeclareBlockers);
    assert_restriction_refuses(&mut runner, desert, index, "Desert");
    assert!(!runner.state().objects[&desert].tapped);
    p1_passes_step(&mut runner);

    // CR 510.1: combat damage step — damage is dealt, then refused.
    give_p1_priority(&mut runner, Phase::CombatDamage);
    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "the unblocked bear dealt its combat damage before end of combat"
    );
    assert_restriction_refuses(&mut runner, desert, index, "Desert");
    assert!(!runner.state().objects[&desert].tapped);
    p1_passes_step(&mut runner);

    // CR 511.1 + CR 511.3: end of combat step — the bear is still an attacking
    // creature, and Desert's ability is now legal.
    give_p1_priority(&mut runner, Phase::EndCombat);
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == bear)),
        "CR 511.3: the bear is still attacking during the end of combat step"
    );
    runner.activate(desert, index).target_object(bear).resolve();
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        1,
        "Desert deals 1 damage to the attacking bear in the end of combat step"
    );
    assert!(runner.state().objects[&desert].tapped, "{{T}} was paid");
}

/// Builds the Lesser Werewolf combat: P0's Test Bear attacks P1, who controls
/// Lesser Werewolf, and the game stops at P1's declare-blockers prompt.
fn werewolf_combat_at_blocker_prompt() -> (GameRunner, ObjectId, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Test Bear", 2, 2).id();
    let werewolf = scenario
        .add_creature_from_oracle(P1, "Lesser Werewolf", 2, 4, LESSER_WEREWOLF)
        .id();
    let mut runner = scenario.build();
    let (index, restrictions) = restricted_ability(&runner, werewolf);
    assert!(
        format!("{restrictions:?}").contains("CurrentPhaseIs"),
        "reach-guard: Lesser Werewolf carries the typed step restriction, got {restrictions:?}"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // CR 508.1: declare attackers step — refused, and nothing was paid.
    give_p1_priority(&mut runner, Phase::DeclareAttackers);
    seed_one_black(&mut runner);
    assert_restriction_refuses(&mut runner, werewolf, index, "Lesser Werewolf");
    assert_eq!(
        runner.state().players[1]
            .mana_pool
            .count_color(ManaType::Black),
        1,
        "the refused activation paid nothing"
    );
    p1_passes_step(&mut runner);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected the declare-blockers prompt, got {:?}",
        runner.state().waiting_for
    );
    (runner, bear, werewolf, index)
}

/// CR 509.1 + CR 602.5: Lesser Werewolf's ability is activatable during the
/// declare blockers step. P1 blocks the bear and activates targeting it (a legal
/// target under any reading of its target phrase): the activation is accepted,
/// its {B} is paid, and the ability goes on the stack.
#[test]
fn lesser_werewolf_activates_only_during_declare_blockers_step() {
    let (mut runner, bear, werewolf, index) = werewolf_combat_at_blocker_prompt();
    runner
        .declare_blockers(&[(werewolf, bear)])
        .expect("werewolf blocks the bear");
    give_p1_priority(&mut runner, Phase::DeclareBlockers);
    seed_one_black(&mut runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: werewolf,
            ability_index: index,
        })
        .expect("CR 602.5: the declare blockers step admits the activation");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "the activation proceeds to target selection, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("the blocked bear is a legal target");
    assert_eq!(runner.state().stack.len(), 1, "the ability is on the stack");
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.source_id),
        Some(werewolf),
        "the stacked ability is Lesser Werewolf's"
    );
    assert_eq!(
        runner.state().players[1]
            .mana_pool
            .count_color(ManaType::Black),
        0,
        "the {{B}} activation cost was paid"
    );
}

/// CR 510.1 + CR 511.1: after the declare blockers step has passed, Lesser
/// Werewolf's ability is refused again in the combat damage and end of combat
/// steps (the declare attackers step is refused while building the combat).
#[test]
fn lesser_werewolf_is_refused_after_the_declare_blockers_step() {
    let (mut runner, bear, werewolf, index) = werewolf_combat_at_blocker_prompt();
    runner
        .declare_blockers(&[(werewolf, bear)])
        .expect("werewolf blocks the bear");
    give_p1_priority(&mut runner, Phase::DeclareBlockers);
    p1_passes_step(&mut runner);

    for phase in [Phase::CombatDamage, Phase::EndCombat] {
        give_p1_priority(&mut runner, phase);
        seed_one_black(&mut runner);
        assert_restriction_refuses(&mut runner, werewolf, index, "Lesser Werewolf");
        assert_eq!(
            runner.state().players[1]
                .mana_pool
                .count_color(ManaType::Black),
            1,
            "the refused activation paid nothing in {phase:?}"
        );
        p1_passes_step(&mut runner);
    }
}

/// CR 500.1 + CR 602.5: the class matrix. For every step of the turn and both
/// active players, the production restriction predicate admits activation iff the
/// current step is the one the instruction names — no turn owner is implied.
/// Kongming's Contraptions composes the step leg with its independent
/// "only if you've been attacked this step" leg.
#[test]
fn named_step_restriction_matrix() {
    let mut scenario = GameScenario::new();
    let desert = scenario.add_land_from_oracle(P0, "Desert", DESERT).id();
    let werewolf = scenario
        .add_creature_from_oracle(P0, "Lesser Werewolf", 2, 4, LESSER_WEREWOLF)
        .id();
    let warlord = scenario
        .add_creature_from_oracle(P0, "Balduvian Warlord", 3, 2, BALDUVIAN_WARLORD)
        .id();
    let kongming = scenario
        .add_creature_from_oracle(P0, "Kongming's Contraptions", 2, 4, KONGMINGS_CONTRAPTIONS)
        .id();
    let mut runner = scenario.build();

    for (source, named, card_name) in [
        (desert, Phase::EndCombat, "Desert"),
        (werewolf, Phase::DeclareBlockers, "Lesser Werewolf"),
        (warlord, Phase::DeclareBlockers, "Balduvian Warlord"),
    ] {
        let (index, restrictions) = restricted_ability(&runner, source);
        assert!(
            format!("{restrictions:?}").contains("CurrentPhaseIs"),
            "reach-guard: {card_name} carries the typed step restriction, got {restrictions:?}"
        );
        for active_player in [P0, P1] {
            for phase in ALL_PHASES {
                let state = runner.state_mut();
                state.active_player = active_player;
                state.phase = phase;
                assert_eq!(
                    check_activation_restrictions(state, P0, source, index, &restrictions).is_ok(),
                    phase == named,
                    "{card_name}: activation during {phase:?} of {active_player:?}'s turn"
                );
            }
        }
    }

    let (index, restrictions) = restricted_ability(&runner, kongming);
    for active_player in [P0, P1] {
        let state = runner.state_mut();
        state.active_player = active_player;
        state.phase = Phase::DeclareAttackers;
        state.players_attacked_this_step.insert(P0);
        assert!(
            check_activation_restrictions(state, P0, kongming, index, &restrictions).is_ok(),
            "Kongming's Contraptions: legal in the declare attackers step once attacked"
        );
        state.players_attacked_this_step.clear();
        assert!(
            check_activation_restrictions(state, P0, kongming, index, &restrictions).is_err(),
            "Kongming's Contraptions: the attacked-this-step leg still gates it"
        );
        state.phase = Phase::DeclareBlockers;
        state.players_attacked_this_step.insert(P0);
        assert!(
            check_activation_restrictions(state, P0, kongming, index, &restrictions).is_err(),
            "Kongming's Contraptions: the step leg refuses the declare blockers step"
        );
        state.players_attacked_this_step.clear();
    }
}
