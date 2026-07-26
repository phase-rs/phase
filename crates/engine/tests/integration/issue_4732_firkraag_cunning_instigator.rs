//! Firkraag, Cunning Instigator — "Whenever a creature deals combat damage to
//! one of your opponents, if that creature had to attack this combat, you put
//! a +1/+1 counter on Firkraag and you draw a card."
//!
//! Regression for issue #4732: the intervening-if "if that creature had to
//! attack this combat" (CR 603.4) was silently dropped during trigger
//! parsing (flagged by the card data's own `SwallowedClause` diagnostic), so
//! the ability put a counter on Firkraag and drew a card on ANY creature
//! dealing combat damage to an opponent — including creatures that attacked
//! purely by their controller's free choice, not under any must-attack
//! requirement (e.g. goad). The fix wires the clause to
//! `TriggerCondition::EventDamageSourceMatchesFilter` (the same "match the
//! event's damage source" mechanism already used by Mindblade Render's "if
//! any of that damage was dealt by a Warrior") over a new
//! `FilterProp::RequiredToAttack`, backed by a declaration-time snapshot
//! (`AttackerInfo::required_to_attack`) taken in
//! `declare_attackers_with_bands` BEFORE attackers are tapped — the
//! must-attack predicate exempts tapped creatures (CR 508.1a), so a live
//! recheck at combat-damage time would spuriously read `false` for every
//! non-vigilance attacker.
//!
//! Discriminating end-to-end: in one game a goaded attacker (CR 701.15b: must
//! attack if able) deals the triggering combat damage — the intervening-if
//! holds, Firkraag gets a counter and its controller draws. In the other, an
//! unencumbered attacker under no requirement deals the same combat damage —
//! the intervening-if fails, matching the pre-fix bug this test guards
//! against (before the fix, this case incorrectly fired too).

use super::rules::run_combat;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::{sba, zones};
use engine::types::ability::{ContinuousModification, Duration, StaticDefinition, TargetFilter};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const FIRKRAAG_TRIGGER_ORACLE: &str = "Whenever a creature deals combat damage to one of your \
    opponents, if that creature had to attack this combat, you put a +1/+1 counter on Firkraag \
    and you draw a card.";

/// +1/+1 counters on `id`.
fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player must exist")
}

/// Mark `creature` as goaded by `goader` — CR 701.15b: must attack each
/// combat if able. Mirrors the proven pattern in
/// `goaded_creature_under_pacifism_visible.rs`.
fn goad(runner: &mut GameRunner, creature: ObjectId, goader: PlayerId) {
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .goaded_by
        .insert(goader);
}

/// CR 603.4 + CR 701.15b + CR 508.1a: a goaded creature had to attack this
/// combat, and it dealt the triggering combat damage — the intervening-if
/// holds. Firkraag's controller gets a +1/+1 counter on Firkraag and draws a
/// card.
#[test]
fn firkraag_counter_and_draw_when_attacker_had_to_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let firkraag = scenario
        .add_creature_from_oracle(
            P0,
            "Firkraag, Cunning Instigator",
            4,
            4,
            FIRKRAAG_TRIGGER_ORACLE,
        )
        .id();
    // Goaded by P1 (the only opponent in a 2-player game, so attacking P1
    // stays legal per CR 701.15b even though P1 is also the goading player —
    // there is no other attackable player to redirect to).
    let goaded = scenario.add_creature(P0, "Goaded Dragon", 3, 3).id();

    let mut runner = scenario.build();
    goad(&mut runner, goaded, P1);

    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);
    let firkraag_counters_before = plus_one_counters(&runner, firkraag);

    run_combat(&mut runner, vec![goaded], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the goaded creature dealt 3 combat damage to P1"
    );
    assert_eq!(
        plus_one_counters(&runner, firkraag),
        firkraag_counters_before + 1,
        "the damage source had to attack this combat (goaded) — Firkraag gets a +1/+1 counter"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "the damage source had to attack this combat (goaded) — Firkraag's controller draws a card"
    );
}

/// CR 603.4 — negative control and the exact case the dropped intervening-if
/// got wrong: an unencumbered creature attacks purely by its controller's
/// free choice (no goad, no must-attack static) and deals the triggering
/// combat damage. The intervening-if fails: no counter, no draw. Before the
/// fix, the swallowed clause meant this case incorrectly fired anyway.
#[test]
fn firkraag_silent_when_attacker_was_not_required_to_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let firkraag = scenario
        .add_creature_from_oracle(
            P0,
            "Firkraag, Cunning Instigator",
            4,
            4,
            FIRKRAAG_TRIGGER_ORACLE,
        )
        .id();
    // Not goaded, no must-attack static — attacks purely by choice.
    let volunteer = scenario.add_creature(P0, "Unencumbered Dragon", 3, 3).id();

    let mut runner = scenario.build();

    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);
    let firkraag_counters_before = plus_one_counters(&runner, firkraag);

    run_combat(&mut runner, vec![volunteer], vec![]);
    // CR 603.3: with the intervening-if respected, the trigger must not even
    // go on the stack; draining confirms no counter/draw sneaks through.
    // Before the fix the dropped condition queued the effect here.
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the unencumbered creature dealt 3 combat damage to P1"
    );
    assert_eq!(
        plus_one_counters(&runner, firkraag),
        firkraag_counters_before,
        "the damage source was not required to attack — intervening-if fails, no counter placed"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "the damage source was not required to attack — intervening-if fails, no card drawn"
    );
}

/// CR 603.4 + CR 608.2i + CR 508.1a: the declaration-time "had to attack"
/// fact must survive the damage source LEAVING THE BATTLEFIELD between
/// dealing the triggering combat damage and the resolution-time
/// intervening-if re-check.
///
/// THE DEFECT this discriminates: the goaded attacker deals its combat
/// damage, Firkraag's trigger passes the fire-time CR 603.4 check (the
/// attacker is still in `state.combat.attackers`, whose `AttackerInfo`
/// carries `required_to_attack`), and goes on the stack. The attacker then
/// dies in response, through the REAL zone-change pipeline —
/// `apply_zone_exit_cleanup` (zones.rs) runs `remove_from_combat`, which
/// erases the `AttackerInfo`. A resolution-time re-check that consults live
/// combat state now reads FALSE for a creature that provably had to attack
/// when attackers were declared, and the trigger is silently removed from
/// the stack. The fix snapshots the fact into the `DamageRecord`
/// (`source_required_to_attack`) at damage time and makes the damage-record
/// matcher consume THAT snapshot, so the re-check stays answerable after the
/// source is gone (CR 608.2i: look-back criteria need not still hold).
#[test]
fn firkraag_fires_when_required_attacker_leaves_before_trigger_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let firkraag = scenario
        .add_creature_from_oracle(
            P0,
            "Firkraag, Cunning Instigator",
            4,
            4,
            FIRKRAAG_TRIGGER_ORACLE,
        )
        .id();
    let goaded = scenario.add_creature(P0, "Goaded Dragon", 3, 3).id();

    let mut runner = scenario.build();
    goad(&mut runner, goaded, P1);

    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);
    let firkraag_counters_before = plus_one_counters(&runner, firkraag);

    run_combat(&mut runner, vec![goaded], vec![]);

    // Precondition: the combat damage landed and Firkraag's trigger is on the
    // stack, still unresolved — the window in which the source can disappear.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the goaded creature dealt 3 combat damage to P1"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "precondition: Firkraag's trigger must be on the stack before the source dies"
    );

    // Kill the attacker in response, through the REAL zone-change pipeline
    // (the same `move_to_zone` + SBA route as removal resolving in response).
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), goaded, Zone::Graveyard, &mut events);
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    // Non-vacuity: the zone change must have removed the attacker from combat
    // (CR 506.4c) — live `state.combat.attackers` can no longer answer
    // "had to attack", so a pass below proves the snapshot path answered.
    assert!(
        runner.state().combat.as_ref().is_some_and(|combat| combat
            .attackers
            .iter()
            .all(|attacker| attacker.object_id != goaded)),
        "precondition: the dead attacker must be removed from combat, erasing its AttackerInfo"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        plus_one_counters(&runner, firkraag),
        firkraag_counters_before + 1,
        "CR 603.4 + CR 608.2i: the source had to attack at declaration; its death before \
         resolution must not flip the re-check — Firkraag still gets a +1/+1 counter"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "CR 603.4 + CR 608.2i: the re-check reads the damage-record snapshot — Firkraag's \
         controller still draws a card"
    );
}

/// CR 508.1d + CR 603.4: sibling requirement source #1 — an intrinsic
/// `StaticMode::MustAttack` static ("attacks each combat if able", the
/// Curse of the Nightly Hunt / Juggernaut class) also makes the creature one
/// that "had to attack this combat". The declaration-time snapshot in
/// `declare_attackers_with_bands` derives from
/// `creature_must_attack_with_attackable_players_gated`, which reads goad AND
/// both must-attack static shapes — this arm proves the static path end to
/// end, not just goad.
#[test]
fn firkraag_counts_must_attack_static_as_had_to_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let firkraag = scenario
        .add_creature_from_oracle(
            P0,
            "Firkraag, Cunning Instigator",
            4,
            4,
            FIRKRAAG_TRIGGER_ORACLE,
        )
        .id();
    // CR 508.1d: intrinsic "attacks each combat if able" — the SelfRef-scoped
    // MustAttack static (same install shape as must_attack_player_attribution).
    let berserker = scenario
        .add_creature(P0, "Compelled Berserker", 3, 3)
        .with_static_definition(
            StaticDefinition::new(StaticMode::MustAttack).affected(TargetFilter::SelfRef),
        )
        .id();

    let mut runner = scenario.build();

    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);
    let firkraag_counters_before = plus_one_counters(&runner, firkraag);

    run_combat(&mut runner, vec![berserker], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the must-attack creature dealt 3 combat damage to P1"
    );
    assert_eq!(
        plus_one_counters(&runner, firkraag),
        firkraag_counters_before + 1,
        "a MustAttack static is a must-attack requirement (CR 508.1d) — the creature had to \
         attack this combat, so Firkraag gets a +1/+1 counter"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "a MustAttack static counts as \"had to attack\" — Firkraag's controller draws"
    );
}

/// CR 508.1d + CR 611.2c + CR 603.4: sibling requirement source #2 — a
/// grafted `StaticMode::MustAttackPlayer` requirement ("attacks that player
/// each combat if able", the ForceAttack / coerce class) also makes the
/// creature one that "had to attack this combat". Grafted exactly as
/// `Effect::ForceAttack` resolves it (transient continuous effect from a
/// directing object), mirroring must_attack_player_attribution.rs.
#[test]
fn firkraag_counts_must_attack_player_requirement_as_had_to_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let firkraag = scenario
        .add_creature_from_oracle(
            P0,
            "Firkraag, Cunning Instigator",
            4,
            4,
            FIRKRAAG_TRIGGER_ORACLE,
        )
        .id();
    let coerced = scenario.add_creature(P0, "Coerced Dragon", 3, 3).id();
    // The directing object (the coercer) — a P1 permanent, as with a resolved
    // ForceAttack-class effect.
    let coercer = scenario.add_vanilla(P1, 1, 1);

    let mut runner = scenario.build();
    // CR 508.1d + CR 611.2c: graft "attacks P1 each combat if able" onto the
    // creature from the directing object, exactly as `Effect::ForceAttack`
    // resolves it.
    runner.state_mut().add_transient_continuous_effect(
        coercer,
        P1,
        Duration::UntilEndOfCombat,
        TargetFilter::SpecificObject { id: coerced },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::MustAttackPlayer { player: P1 },
        }],
        None,
    );

    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);
    let firkraag_counters_before = plus_one_counters(&runner, firkraag);

    run_combat(&mut runner, vec![coerced], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the coerced creature dealt 3 combat damage to P1"
    );
    assert_eq!(
        plus_one_counters(&runner, firkraag),
        firkraag_counters_before + 1,
        "a MustAttackPlayer requirement is a must-attack requirement (CR 508.1d) — the \
         creature had to attack this combat, so Firkraag gets a +1/+1 counter"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "a MustAttackPlayer requirement counts as \"had to attack\" — Firkraag's controller draws"
    );
}
