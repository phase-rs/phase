//! CR 506.3 + CR 508.1d + CR 611.2c: Gideon Jura's "+2: During target
//! opponent's next turn, creatures that player controls attack Gideon Jura if
//! able." — a forced-attack requirement whose required defender is a
//! PLANESWALKER rather than a player.
//!
//! These drive the real pipeline end-to-end: verbatim Oracle text → parser
//! (`Effect::ForceAttack { required_defender: SelfRef }`) →
//! `force_attack::resolve` (which snapshots the defender as
//! `RequiredDefender::Permanent` and installs ONE continuous effect carrying the
//! live affected filter) → `must_attack_defender_directives_for_creature` →
//! `attacker_constraints_for_active_player` (the DeclareAttackers payload
//! authority) AND the `declare_attackers` legality validator via the production
//! `GameAction::DeclareAttackers` route.
//!
//! The rulings these pin, verbatim from Gatherer:
//!   * "Gideon Jura's first ability doesn't lock in what it applies to. … This
//!     includes creatures that come under that player's control after the
//!     ability has resolved."
//!   * "If a creature controlled by the affected player can't attack Gideon Jura
//!     (because he's no longer on the battlefield, for example), that player may
//!     have it attack you, another one of your planeswalkers, or nothing at all."

use engine::game::combat::{
    attacker_constraints_for_active_player, get_valid_attacker_ids, AttackTarget, CombatRequirement,
};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ContinuousModification;
use engine::types::ability::{Duration, PlayerScope};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::{RequiredDefender, StaticMode};
use engine::types::zones::Zone;

/// Gideon Jura — verbatim Oracle text (Scryfall). Only the "+2" matters here;
/// the other two abilities ride along so the fixture parses the real card rather
/// than an excerpt.
const GIDEON_JURA_ORACLE: &str = concat!(
    "+2: During target opponent's next turn, creatures that player controls ",
    "attack Gideon Jura if able.\n",
    "\u{2212}2: Destroy target tapped creature.\n",
    "0: Until end of turn, Gideon Jura becomes a 6/6 Human Soldier creature ",
    "that's still a planeswalker. Prevent all damage that would be dealt to him ",
    "this turn.",
);

/// P0 controls Gideon Jura; P1 controls `p1_creatures` vanilla bears. Returns
/// the runner (parked in P0's main phase with the statics live), Gideon's id,
/// and P1's creature ids.
fn setup(p1_creatures: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gideon = scenario
        .add_planeswalker_from_oracle(P0, "Gideon Jura", "Gideon", 6, GIDEON_JURA_ORACLE)
        .id();
    let bears: Vec<ObjectId> = (0..p1_creatures)
        .map(|i| scenario.add_creature(P1, &format!("Bear {i}"), 2, 2).id())
        .collect();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::PreCombatMain;
        state.turn_number = 2;
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    (runner, gideon, bears)
}

/// Activate the "+2" (loyalty ability index 0) targeting P1, then resolve it.
///
/// CR 601.2c: the companion player slot is the one `ControllerRef::TargetOpponent`
/// surfaces for "creatures that player controls" — if the parser bound that
/// anaphor to `You` instead, there would be no slot to fill and this panics.
fn activate_plus_two(runner: &mut GameRunner, gideon: ObjectId) {
    runner.activate(gideon, 0).target_player(P1).resolve();
}

/// Hand the turn to P1 and park at their declare-attackers step with the layer
/// pass fresh, so the requirement is evaluated exactly as it is in a real turn.
fn hand_turn_to_p1(runner: &mut GameRunner) {
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 3;
        // CR 302.6: the bears have been under P1's control since before this
        // turn began, so they are able to attack.
        for id in state.battlefield.clone() {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.summoning_sick = false;
            }
        }
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    let valid = get_valid_attacker_ids(runner.state());
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: valid,
        valid_attack_targets: vec![],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
}

fn declare(runner: &mut GameRunner, attacks: Vec<(ObjectId, AttackTarget)>) -> Result<(), String> {
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// CR 611.2: the resolved "+2" installs ONE continuous effect whose modification
/// is a `MustAttackDefender` bound to a `RequiredDefender::Permanent` — the
/// planeswalker itself — and whose expiry is the TARGETED player's next turn,
/// lowered to a concrete snapshot.
///
/// REVERT-FAIL on three separate seams: a player-only `RequiredDefender` cannot
/// express the defender; an un-lowered `PlayerScope::Target` expiry would never
/// arm (`prune_until_next_turn_effects` compares against a concrete player); and
/// a frozen `SpecificObject` affected set would replace the live filter.
#[test]
fn plus_two_installs_permanent_defender_requirement_scoped_to_target() {
    let (mut runner, gideon, _bears) = setup(1);
    activate_plus_two(&mut runner, gideon);

    let effect = runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|ce| {
            ce.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustAttackDefender { .. }
                    }
                )
            })
        })
        .expect("the +2 installs a must-attack requirement");

    let ContinuousModification::AddStaticMode {
        mode: StaticMode::MustAttackDefender { defender },
    } = &effect.modifications[0]
    else {
        panic!(
            "expected a MustAttackDefender grant, got {:?}",
            effect.modifications
        );
    };
    let RequiredDefender::Permanent { permanent } = defender else {
        panic!("CR 506.3: the required defender is the PLANESWALKER, got {defender:?}");
    };
    assert_eq!(
        permanent.object_id, gideon,
        "the snapshotted defender is Gideon Jura itself"
    );

    // CR 508.1d (final sentence): the window is that player's whole next turn.
    assert_eq!(
        effect.duration,
        Duration::UntilEndOfNextTurnOf {
            player: PlayerScope::SpecificPlayer { id: P1 }
        },
        "the expiry is lowered to the TARGETED player, not the controller"
    );

    // CR 611.2c: the affected set stays a live FILTER, never a frozen id list —
    // the ruling requires creatures that arrive later to be caught too.
    assert!(
        !matches!(
            effect.affected,
            engine::types::ability::TargetFilter::SpecificObject { .. }
        ),
        "the affected population must stay dynamic, got {:?}",
        effect.affected
    );
}

/// CR 115.1: the "+2" targets exactly ONE thing — the opponent. "creatures that
/// player controls" is a population, not a target, so no creature slot may be
/// declared.
///
/// This is what `EffectScope::All` buys: before it, the broadcast subject filter
/// was read as a selectable target and the ability went on the stack with TWO
/// targets (the player AND an arbitrary creature). That over-targets the ability
/// — it would wrongly fizzle when that creature became an illegal target, and it
/// would illegally "target" a creature with hexproof.
#[test]
fn plus_two_targets_only_the_opponent() {
    let (mut runner, gideon, bears) = setup(2);
    runner
        .act(GameAction::ActivateAbility {
            source_id: gideon,
            ability_index: 0,
        })
        .expect("activation is legal");
    let stacked = runner
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .expect("the ability is on the stack awaiting resolution");
    assert_eq!(
        stacked.targets.len(),
        1,
        "CR 115.1: exactly one target — the opponent, got {:?}",
        stacked.targets
    );
    assert!(
        matches!(
            stacked.targets[0],
            engine::types::ability::TargetRef::Player(P1)
        ),
        "the sole target is the opponent, got {:?}",
        stacked.targets
    );
    for bear in &bears {
        assert!(
            !stacked
                .targets
                .iter()
                .any(|t| matches!(t, engine::types::ability::TargetRef::Object(id) if id == bear)),
            "no creature is targeted: {:?}",
            stacked.targets
        );
    }
}

/// CR 508.1d enforcement, through the production `GameAction::DeclareAttackers`
/// route: P1's creature must attack Gideon Jura. Attacking P0 instead is
/// rejected, attacking Gideon commits, and declining entirely is rejected.
#[test]
fn plus_two_forces_the_targeted_opponents_creature_onto_gideon() {
    // Attacking the PLAYER leaves the requirement unmet.
    let (mut wrong, gideon, bears) = setup(1);
    activate_plus_two(&mut wrong, gideon);
    hand_turn_to_p1(&mut wrong);
    assert!(
        declare(&mut wrong, vec![(bears[0], AttackTarget::Player(P0))]).is_err(),
        "attacking Gideon's controller does not obey a requirement naming the planeswalker"
    );

    // Attacking the PLANESWALKER satisfies it.
    let (mut right, gideon, bears) = setup(1);
    activate_plus_two(&mut right, gideon);
    hand_turn_to_p1(&mut right);
    declare(
        &mut right,
        vec![(bears[0], AttackTarget::Planeswalker(gideon))],
    )
    .expect("attacking Gideon Jura satisfies CR 508.1d");
    assert!(
        right.state().combat.is_some(),
        "the satisfying declaration commits"
    );

    // Declining entirely leaves it unmet — the requirement genuinely binds.
    let (mut none, gideon, _bears) = setup(1);
    activate_plus_two(&mut none, gideon);
    hand_turn_to_p1(&mut none);
    assert!(
        declare(&mut none, vec![]).is_err(),
        "declaring no attacker leaves the requirement unmet"
    );
}

/// CR 611.2c + the card's own ruling — "doesn't lock in what it applies to …
/// includes creatures that come under that player's control after the ability
/// has resolved."
///
/// Resolve the "+2" while P1 controls NOTHING, then give them a creature, then
/// declare. The late arrival is still forced onto Gideon. A resolution-time
/// snapshot of the affected set would leave it free, so this fails on any
/// freeze-the-population regression.
#[test]
fn plus_two_affected_set_is_not_locked_in_at_resolution() {
    let (mut runner, gideon, _bears) = setup(0);
    activate_plus_two(&mut runner, gideon);

    // The creature arrives AFTER the ability resolved.
    let latecomer = {
        let state = runner.state_mut();
        let id = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(9001),
            P1,
            "Latecomer Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        id
    };

    hand_turn_to_p1(&mut runner);
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&latecomer),
        "reach-guard: the late arrival is an eligible attacker"
    );
    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    let Some(CombatRequirement::MustAttack { defenders, .. }) = constraints.get(&latecomer) else {
        panic!(
            "the late arrival must carry the requirement, got {:?}",
            constraints.get(&latecomer)
        );
    };
    assert_eq!(
        defenders,
        &vec![AttackTarget::Planeswalker(gideon)],
        "CR 611.2c: the population is re-derived at declare-attackers"
    );
}

/// The card's ruling: "If a creature controlled by the affected player can't
/// attack Gideon Jura (because he's no longer on the battlefield, for example),
/// that player may have it attack you … or nothing at all."
///
/// CR 508.1d drops a requirement that cannot be obeyed, so with Gideon gone the
/// creature is free — both to attack the player and to decline. This is the
/// vacuity guard for the enforcement test above: without it, an implementation
/// that simply never enforced the requirement would also pass "declining works".
#[test]
fn requirement_lapses_when_gideon_leaves_the_battlefield() {
    let (mut runner, gideon, bears) = setup(1);
    activate_plus_two(&mut runner, gideon);

    // CR 400.7: Gideon leaves; the snapshotted incarnation pin no longer names a
    // live defender.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), gideon, Zone::Graveyard, &mut events);
    hand_turn_to_p1(&mut runner);

    let valid = get_valid_attacker_ids(runner.state());
    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    assert!(
        !matches!(
            constraints.get(&bears[0]),
            Some(CombatRequirement::MustAttack { .. })
        ),
        "with Gideon gone the requirement is unobeyable and must not surface: {:?}",
        constraints.get(&bears[0])
    );
    declare(&mut runner, vec![]).expect("the creature may attack nothing at all");
}
