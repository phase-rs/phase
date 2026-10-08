//! Harm's Way (#4793) and the "next N damage … is dealt to any target instead"
//! redirection class, driven through the production cast / activation pipeline.
//!
//! Timing (CR 115.1a + CR 601.2c): the "any target" recipient is chosen as the
//! spell is cast; the source is chosen as the effect is created, on resolution
//! (CR 609.7a). Zhalfirin Crusader chooses its recipient on activation
//! (CR 115.1c + CR 602.2b). The "any target" domain (CR 115.4) is enforced at
//! cast time and rechecked on resolution (CR 608.2b).

use engine::game::combat::AttackTarget;
use engine::game::effects::deal_damage;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityKind, ControllerRef, DamageRedirectTarget, Effect, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HARMS_WAY: &str = "The next 2 damage that a source of your choice would deal to you and/or permanents you control this turn is dealt to any target instead.";
const LIGHTNING_STRIKE: &str = "Lightning Strike deals 3 damage to any target.";
const ZHALFIRIN_CRUSADER: &str = "Flanking (Whenever a creature without flanking blocks this creature, the blocking creature gets -1/-1 until end of turn.)\n{1}{W}: The next 1 damage that would be dealt to this creature this turn is dealt to any target instead.";
const NOMADS_EN_KOR: &str = "{0}: The next 1 damage that would be dealt to this creature this turn is dealt to target creature you control instead.";

/// A real damage event through the engine's `DealDamage` resolver (secondary
/// seam, labelled): the replacement pipeline is reached from that resolver.
fn deal(
    runner: &mut GameRunner,
    source: ObjectId,
    controller: PlayerId,
    target: TargetRef,
    amount: i32,
) {
    let ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source,
        controller,
    );
    let mut events = Vec::<GameEvent>::new();
    deal_damage::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the damage event resolves");
}

fn damage_marked(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

/// Make an object an artifact creature in both its current and base types, so a
/// layer pass cannot restore or remove a type behind the test's back.
fn make_artifact_creature(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types.core_types.push(CoreType::Artifact);
}

/// Strip the Creature type from both the current and base types.
fn strip_creature(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types
        .core_types
        .retain(|t| *t != CoreType::Creature);
    obj.base_card_types
        .core_types
        .retain(|t| *t != CoreType::Creature);
}

/// Resolve Harm's Way (already cast with its target) to its CR 609.7a source
/// choice and pick `source`.
fn choose_source(runner: &mut GameRunner, outcome_wait: &WaitingFor, source: ObjectId) {
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = outcome_wait
    else {
        panic!("Harm's Way must stop at its CR 609.7a source choice, got {outcome_wait:?}");
    };
    assert_eq!(*player, P0);
    assert!(
        options.contains(&source),
        "the chosen source must be offered: {options:?}"
    );
    runner
        .act(GameAction::ChooseDamageSource { source })
        .expect("the selected source binds the pending redirection");
}

/// T1 — CR 614.9 + CR 609.7a + CR 115.1a: in real combat, the 2-damage budget
/// splits the chosen attacker's 3 (2 → the chosen player, 1 stays on P0), and the
/// unchosen attacker is untouched. Reverting the player-recipient latch, the
/// production, or the direct spell route leaves P0 at −5 and P1 at 0.
#[test]
fn harms_way_redirects_two_of_the_chosen_attackers_damage_to_the_chosen_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["P0 draw"]);
    scenario.with_library_top(P1, &["P1 draw"]);
    let harms_way = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen", 3, 3).id();
    let other = scenario.add_creature(P1, "Other", 2, 2).id();
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (chosen, AttackTarget::Player(P0)),
            (other, AttackTarget::Player(P0)),
        ])
        .expect("P1 declares both attackers");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .declare_blockers(&[])
            .expect("P0 declares no blockers");
    }
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to the defending caster");

    let commit = runner.cast(harms_way).target_player(P1).commit();
    // CR 115.1a + CR 601.2c: the "any target" recipient is fixed at cast.
    assert!(
        commit.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::Spell { ability: Some(ability), .. }
                if ability.targets.contains(&TargetRef::Player(P1))
        )),
        "Harm's Way's target must be chosen as it is cast"
    );
    let cast = commit.resolve();
    let WaitingFor::DamageSourceChoice { options, .. } = cast.final_waiting_for() else {
        panic!(
            "expected the source choice, got {:?}",
            cast.final_waiting_for()
        );
    };
    assert!(
        options.contains(&other),
        "every source is offered: {options:?}"
    );
    choose_source(&mut runner, cast.final_waiting_for(), chosen);

    let combat = runner.combat_damage();
    assert_eq!(
        combat.life_delta(P0),
        -3,
        "1 leftover from the chosen attacker + 2 from the unchosen attacker"
    );
    assert_eq!(
        combat.life_delta(P1),
        -2,
        "exactly 2 of the chosen attacker's damage is dealt to the chosen player"
    );
}

/// Build the CR 115.4 domain fixture and cast `spell` with no preselected
/// targets, returning the single target slot's legal targets and the fixture ids.
fn any_target_slot(name: &str, text: &str) -> (Vec<TargetRef>, [ObjectId; 4]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .id();
    let land = scenario.add_land_from_oracle(P1, "Probe Land", "").id();
    let artifact = scenario.add_artifact_from_oracle(P1, "Probe Rock", "").id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Probe Walker", "Jace", 3, "")
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::default(),
        })
        .expect("casting reaches target selection");
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "{name} must ask for its target, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "{name} declares exactly one target");
    (
        target_slots[0].legal_targets.clone(),
        [land, artifact, bear, walker],
    )
}

/// T1b — CR 115.4 + CR 601.2c: the redirect recipient's legal targets are the
/// "any target" domain — players, creatures and planeswalkers, never a land or a
/// noncreature artifact. Lightning Strike is the sibling guard for the existing
/// `DealDamage` domain. Reverting the role-aware predicate to `DealDamage`-only
/// offers the land and artifact for Harm's Way.
#[test]
fn harms_way_offers_only_the_any_target_domain() {
    for (name, text) in [
        ("Harm's Way", HARMS_WAY),
        ("Lightning Strike", LIGHTNING_STRIKE),
    ] {
        let (legal, [land, artifact, bear, walker]) = any_target_slot(name, text);
        for positive in [
            TargetRef::Player(P0),
            TargetRef::Player(P1),
            TargetRef::Object(bear),
            TargetRef::Object(walker),
        ] {
            assert!(
                legal.contains(&positive),
                "{name}: {positive:?} is a legal 'any target': {legal:?}"
            );
        }
        for negative in [TargetRef::Object(land), TargetRef::Object(artifact)] {
            assert!(
                !legal.contains(&negative),
                "{name}: CR 115.4 — {negative:?} is not a legal 'any target': {legal:?}"
            );
        }
    }
}

/// Scenario with an artifact-creature Golem controlled by P1 and `spell` in P0's
/// hand, at P0's main phase.
fn golem_fixture(name: &str, text: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .id();
    let golem = scenario.add_creature(P1, "Golem", 3, 3).id();
    let mut runner = scenario.build();
    make_artifact_creature(&mut runner, golem);
    (runner, spell, golem)
}

/// T1c — CR 608.2b + CR 115.4: Harm's Way's only target stops being a creature
/// while it is on the stack. It is no longer an "any target", so the spell doesn't
/// resolve: no source choice, no shield. The paired positive casts at the same
/// unmutated Golem and reaches the source choice. Reverting the resolution
/// domain recheck keeps the noncreature artifact legal and halts at the choice.
#[test]
fn harms_way_does_not_resolve_when_its_target_leaves_the_any_target_domain() {
    let (mut runner, harms_way, golem) = golem_fixture("Harm's Way", HARMS_WAY);
    let mut commit = runner.cast(harms_way).target_objects(&[golem]).commit();
    strip_creature(commit.state_mut(), golem);
    let golem_obj = &commit.state().objects[&golem];
    assert_eq!(
        golem_obj.zone,
        Zone::Battlefield,
        "positive guard: still on the battlefield"
    );
    assert!(
        !golem_obj
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "positive guard: no longer a creature"
    );
    let cast = commit.resolve();
    assert!(
        matches!(cast.final_waiting_for(), WaitingFor::Priority { .. }),
        "an illegal only target means no CR 609.7a choice: {:?}",
        cast.final_waiting_for()
    );
    assert!(cast.state().stack.is_empty());
    assert_eq!(cast.zone_of(harms_way), Zone::Graveyard);
    assert!(
        cast.state().pending_damage_replacements.is_empty(),
        "no Harm's Way shield is installed"
    );

    let (mut runner, harms_way, golem) = golem_fixture("Harm's Way", HARMS_WAY);
    let cast = runner.cast(harms_way).target_objects(&[golem]).resolve();
    assert!(
        matches!(
            cast.final_waiting_for(),
            WaitingFor::DamageSourceChoice { .. }
        ),
        "paired positive: a legal artifact-creature target resolves to the source choice, got {:?}",
        cast.final_waiting_for()
    );
}

/// T1e — CR 608.2b + CR 115.4 for the whole `DealDamage { target: Any }` class:
/// Lightning Strike's only target stops being a creature while it is on the
/// stack, so the spell doesn't resolve and deals no damage. The paired positive
/// deals 3 to the unmutated Golem. Reverting the resolution domain recheck lets
/// the Strike resolve and emit `DamageDealt` to the noncreature artifact.
#[test]
fn lightning_strike_does_not_resolve_when_its_target_leaves_the_any_target_domain() {
    let (mut runner, strike, golem) = golem_fixture("Lightning Strike", LIGHTNING_STRIKE);
    let mut commit = runner.cast(strike).target_objects(&[golem]).commit();
    strip_creature(commit.state_mut(), golem);
    let golem_obj = &commit.state().objects[&golem];
    assert_eq!(
        golem_obj.zone,
        Zone::Battlefield,
        "positive guard: still on the battlefield"
    );
    assert!(
        !golem_obj
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "positive guard: no longer a creature"
    );
    let cast = commit.resolve();
    assert_eq!(cast.zone_of(strike), Zone::Graveyard);
    assert!(
        !cast.events().iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt { target: TargetRef::Object(id), .. } if *id == golem
        )),
        "an illegal only target is dealt no damage: {:?}",
        cast.events()
    );

    let (mut runner, strike, golem) = golem_fixture("Lightning Strike", LIGHTNING_STRIKE);
    let cast = runner.cast(strike).target_objects(&[golem]).resolve();
    assert_eq!(
        cast.state().objects[&golem].damage_marked,
        3,
        "paired positive: a legal artifact-creature target takes 3"
    );
    assert!(cast.events().iter().any(|event| matches!(
        event,
        GameEvent::DamageDealt { target: TargetRef::Object(id), amount: 3, .. } if *id == golem
    )));
}

/// T2 — CR 609.7a: a spell on the stack is a source of your choice. P1 casts
/// Lightning Strike at P0; P0 responds with Harm's Way at P1's Wall and chooses
/// the Strike. The Wall takes 2, P0 takes 1. Reverting the production leaves P0
/// at −3 and the Wall undamaged.
#[test]
fn harms_way_redirects_a_lightning_strike_on_the_stack_to_a_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["P0 draw"]);
    scenario.with_library_top(P1, &["P1 draw"]);
    let strike = scenario
        .add_spell_to_hand_from_oracle(P1, "Lightning Strike", true, LIGHTNING_STRIKE)
        .id();
    let harms_way = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .id();
    let wall = scenario.add_creature(P1, "Wall", 0, 5).id();
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);

    let mut strike_commit = runner.cast(strike).target_player(P0).commit();
    strike_commit
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to P0");
    let response = strike_commit
        .cast(harms_way)
        .target_objects(&[wall])
        .resolve();
    let WaitingFor::DamageSourceChoice { options, .. } = response.final_waiting_for() else {
        panic!(
            "Harm's Way must stop at its source choice, got {:?}",
            response.final_waiting_for()
        );
    };
    assert!(
        options.contains(&strike),
        "CR 609.7a: the spell on the stack is offered as a source: {options:?}"
    );
    strike_commit
        .act(GameAction::ChooseDamageSource { source: strike })
        .expect("P0 chooses Lightning Strike as the source");

    let outcome = strike_commit.resolve();
    assert_eq!(
        outcome.life_delta(P0),
        -1,
        "1 of the Strike's 3 stays on P0"
    );
    assert_eq!(
        outcome.state().objects[&wall].damage_marked,
        2,
        "2 of the Strike's 3 is dealt to the chosen creature"
    );
    assert_eq!(
        outcome.life_delta(P1),
        0,
        "the Strike's controller takes nothing"
    );
}

/// Cast Harm's Way at `target` in P0's main phase and choose `source`.
fn install_harms_way(
    runner: &mut GameRunner,
    harms_way: ObjectId,
    target: TargetRef,
    source: ObjectId,
) {
    let cast = match target {
        TargetRef::Player(player) => runner.cast(harms_way).target_player(player).resolve(),
        TargetRef::Object(id) => runner.cast(harms_way).target_objects(&[id]).resolve(),
    };
    let wait = cast.final_waiting_for().clone();
    choose_source(runner, &wait, source);
}

/// T4 — the "next 2 damage" budget depletes across events: 1 of 1, then 1 of 3,
/// then the shield is spent.
#[test]
fn harms_way_budget_depletes_across_events() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let harms_way = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen", 3, 3).id();
    let mut runner = scenario.build();
    install_harms_way(&mut runner, harms_way, TargetRef::Player(P1), chosen);

    let (p0, p1) = (runner.life(P0), runner.life(P1));
    deal(&mut runner, chosen, P1, TargetRef::Player(P0), 1);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (p0, p1 - 1),
        "1 of 1 redirected"
    );
    deal(&mut runner, chosen, P1, TargetRef::Player(P0), 3);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (p0 - 2, p1 - 2),
        "the last 1 of the budget redirects; 2 stay"
    );
    deal(&mut runner, chosen, P1, TargetRef::Player(P0), 1);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (p0 - 3, p1 - 2),
        "the spent shield redirects nothing"
    );
}

/// T5 — "you and/or permanents you control": damage to P1's creature is not
/// redirected; damage to P0 and to P0's creature is.
#[test]
fn harms_way_covers_you_and_permanents_you_control_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let harms_way = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen", 3, 3).id();
    let foe = scenario.add_creature(P1, "Foe", 1, 5).id();
    let ward = scenario.add_creature(P0, "Ward", 1, 5).id();
    let mut runner = scenario.build();
    install_harms_way(&mut runner, harms_way, TargetRef::Player(P1), chosen);

    let (p0, p1) = (runner.life(P0), runner.life(P1));
    deal(&mut runner, chosen, P1, TargetRef::Object(foe), 2);
    assert_eq!(
        damage_marked(&runner, foe),
        2,
        "P1's creature is not protected"
    );
    assert_eq!(runner.life(P1), p1, "nothing redirected from P1's creature");

    deal(&mut runner, chosen, P1, TargetRef::Player(P0), 1);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (p0, p1 - 1),
        "reach guard: the shield is still live and redirects damage to P0"
    );

    deal(&mut runner, chosen, P1, TargetRef::Object(ward), 1);
    assert_eq!(
        damage_marked(&runner, ward),
        0,
        "P0's creature is protected"
    );
    assert_eq!(
        runner.life(P1),
        p1 - 2,
        "damage to P0's creature is redirected"
    );
}

/// T6 — CR 614.9: a creature recipient that left the battlefield after
/// resolution makes the redirection do nothing; the damage stays on P0.
#[test]
fn harms_way_does_nothing_when_its_creature_recipient_is_gone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let harms_way = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen", 3, 3).id();
    let dest = scenario.add_creature(P1, "Destination", 2, 5).id();
    let mut runner = scenario.build();
    install_harms_way(&mut runner, harms_way, TargetRef::Object(dest), chosen);

    engine::game::zones::move_to_zone(runner.state_mut(), dest, Zone::Graveyard, &mut Vec::new());
    let p0 = runner.life(P0);
    deal(&mut runner, chosen, P1, TargetRef::Player(P0), 3);
    assert_eq!(runner.life(P0), p0 - 3, "the redirection does nothing");
}

/// T7 — CR 514.2: an unused shield ends at cleanup; next turn the chosen source's
/// damage is not redirected and no shield remains.
#[test]
fn harms_way_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 draw"]);
    scenario.with_library_top(P1, &["P1 draw"]);
    let harms_way = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen", 3, 3).id();
    let mut runner = scenario.build();
    install_harms_way(&mut runner, harms_way, TargetRef::Player(P1), chosen);
    assert!(
        !runner.state().pending_damage_replacements.is_empty(),
        "positive guard: the shield is installed this turn"
    );

    for _ in 0..40 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passing advances into the next turn");
    }
    assert_eq!(runner.state().active_player, P1);
    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "the shield is pruned at cleanup"
    );
    let p0 = runner.life(P0);
    deal(&mut runner, chosen, P1, TargetRef::Player(P0), 3);
    assert_eq!(runner.life(P0), p0 - 3, "no redirection on a later turn");
}

fn add_white_mana(runner: &mut GameRunner) {
    for mana in [ManaType::White, ManaType::White] {
        let added = runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(mana, ObjectId(0), false, vec![]));
        assert!(
            added.is_some(),
            "the priority window accepts activation mana"
        );
    }
}

fn crusader_fixture() -> (GameRunner, ObjectId, [ObjectId; 4]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crusader = {
        let mut builder = scenario.add_creature(P0, "Zhalfirin Crusader", 2, 2);
        builder.from_oracle_text_with_keywords(&["Flanking"], ZHALFIRIN_CRUSADER);
        builder.id()
    };
    let land = scenario.add_land_from_oracle(P1, "Probe Land", "").id();
    let artifact = scenario.add_artifact_from_oracle(P1, "Probe Rock", "").id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Probe Walker", "Jace", 3, "")
        .id();
    let mut runner = scenario.build();
    add_white_mana(&mut runner);
    (runner, crusader, [land, artifact, bear, walker])
}

/// T8 — Zhalfirin Crusader (U4) through activation: its "any target" recipient
/// slot offers the CR 115.4 domain only, and choosing a player moves 1 of the
/// next damage dealt to the Crusader to that player.
#[test]
fn zhalfirin_crusader_redirects_one_damage_to_any_target() {
    let (mut runner, crusader, [land, artifact, bear, walker]) = crusader_fixture();
    runner
        .act(GameAction::ActivateAbility {
            source_id: crusader,
            ability_index: 0,
        })
        .expect("activation reaches target selection");
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "Zhalfirin Crusader must ask for its target, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "exactly one redirect-recipient slot");
    let legal = &target_slots[0].legal_targets;
    for positive in [
        TargetRef::Player(P0),
        TargetRef::Player(P1),
        TargetRef::Object(bear),
        TargetRef::Object(walker),
    ] {
        assert!(
            legal.contains(&positive),
            "{positive:?} is a legal 'any target': {legal:?}"
        );
    }
    for negative in [TargetRef::Object(land), TargetRef::Object(artifact)] {
        assert!(
            !legal.contains(&negative),
            "CR 115.4 — {negative:?} is not a legal 'any target': {legal:?}"
        );
    }

    let (mut runner, crusader, [_, _, bear, _]) = crusader_fixture();
    runner.activate(crusader, 0).target_player(P1).resolve();
    let p1 = runner.life(P1);
    deal(&mut runner, bear, P1, TargetRef::Object(crusader), 2);
    assert_eq!(
        runner.state().objects[&crusader].zone,
        Zone::Battlefield,
        "the Crusader survives"
    );
    assert_eq!(
        damage_marked(&runner, crusader),
        1,
        "1 of 2 stays on the Crusader"
    );
    assert_eq!(runner.life(P1), p1 - 1, "1 is dealt to the chosen player");

    // Negative sibling: the en-Kor text keeps its typed "target creature you
    // control" recipient; positive guard is the same `ChosenTarget` recipient.
    let nomads = parse_oracle_text(
        NOMADS_EN_KOR,
        "Nomads en-Kor",
        &[],
        &["Creature".into()],
        &[],
    );
    let effect = nomads
        .abilities
        .iter()
        .find(|ability| matches!(ability.kind, AbilityKind::Activated))
        .map(|ability| ability.effect.as_ref())
        .expect("Nomads en-Kor has an activated ability");
    assert!(
        matches!(
            effect,
            Effect::CreateDamageReplacement {
                redirect_to: Some(DamageRedirectTarget::ChosenTarget),
                redirect_object_filter: Some(TargetFilter::Typed(typed)),
                ..
            } if typed.controller == Some(ControllerRef::You)
        ),
        "Nomads en-Kor keeps its typed creature-you-control recipient: {effect:?}"
    );
}

/// T13 — route SHAPE (labelled): Harm's Way is one spell ability whose effect is
/// the redirection, and no static replacement stub claims the line. Reverting
/// the direct spell route leaves the detail-less `DamageDone` replacement.
#[test]
fn harms_way_routes_to_a_spell_redirection_not_a_static_stub() {
    let parsed = parse_oracle_text(HARMS_WAY, "Harm's Way", &[], &["Instant".into()], &[]);
    let spells: Vec<_> = parsed
        .abilities
        .iter()
        .filter(|ability| matches!(ability.kind, AbilityKind::Spell))
        .collect();
    assert_eq!(
        spells.len(),
        1,
        "exactly one spell ability: {:?}",
        parsed.abilities
    );
    assert!(
        matches!(
            spells[0].effect.as_ref(),
            Effect::CreateDamageReplacement {
                redirect_to: Some(DamageRedirectTarget::ChosenTarget),
                redirect_object_filter: Some(TargetFilter::Any),
                ..
            }
        ),
        "Harm's Way must be a chosen-target redirection: {:?}",
        spells[0].effect
    );
    assert!(
        parsed.replacements.is_empty(),
        "no static replacement stub claims the line: {:?}",
        parsed.replacements
    );
}
