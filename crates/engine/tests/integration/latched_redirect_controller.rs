//! Resolution-created "to you" redirection binds the creating ability's controller.
//! Full-Oracle activations, response spells, and damage run through real actions.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityKind, DamageRedirectTarget, Effect, PreventionAmount, RedirectionLifetime,
    ReplacementDefinition, ShieldKind, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

// Verbatim Oracle, independently retrieved from Scryfall's named-card API on 2026-10-05.
const JADE: &str = "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.";
const VASSALS_DUTY: &str = "{1}: The next 1 damage that would be dealt to target legendary creature you control this turn is dealt to you instead.";
const HELLKITE: &str = "Flying (This creature can't be blocked except by creatures with flying or reach.)\n{1}{R}: This creature deals 1 damage to any target.";
const RAY: &str = "Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.";
const MEMNARCH: &str = "{1}{U}{U}: Target permanent becomes an artifact in addition to its other types. (This effect lasts indefinitely.)\n{3}{U}: Gain control of target artifact. (This effect lasts indefinitely.)";
const DISPERSE: &str = "Return target nonland permanent to its owner's hand.";

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    for _ in 0..count {
        runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn mana(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool
        .total()
}

fn priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..runner.state().players.len() {
        match runner.state().waiting_for {
            WaitingFor::Priority { player: current } if current == player => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            ref other => panic!("expected priority, got {other:?}"),
        }
    }
    panic!("priority did not reach {player:?}");
}

fn announce_targeted(
    runner: &mut GameRunner,
    player: PlayerId,
    source: ObjectId,
    ability_index: usize,
    target: ObjectId,
) {
    priority(runner, player);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("announce the printed activation");
    let WaitingFor::TargetSelection {
        player: chooser,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected one target prompt: {:?}",
            runner.state().waiting_for
        );
    };
    // CR 115.1c + CR 602.2b: only the printed creature/artifact target is declared.
    assert_eq!(*chooser, player);
    assert_eq!(target_slots.len(), 1, "no implicit player target is added");
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(target)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("submit the legal original target");
    let entry = runner
        .state()
        .stack
        .back()
        .expect("activation on the stack");
    // CR 113.8: the stacked ability retains the activator's authority.
    assert_eq!(entry.controller, player);
    assert!(
        matches!(&entry.kind, StackEntryKind::ActivatedAbility { source_id, ability }
        if *source_id == source && ability.controller == player
            && ability.ability_index == Some(ability_index)
            && ability.targets == vec![TargetRef::Object(target)])
    );
}

struct Board {
    runner: GameRunner,
    jade: ObjectId,
    protected: ObjectId,
    other: ObjectId,
    sources: [ObjectId; 2],
    rock: ObjectId,
    ray: ObjectId,
    disperse: ObjectId,
    memnarch: ObjectId,
}

fn board(protected_controller: PlayerId) -> Board {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let jade = scenario
        .add_artifact_from_oracle(P2, "Jade Monolith", JADE)
        .controlled_by(P0)
        .id();
    let protected = scenario
        .add_creature(P2, "Protected creature", 0, 9)
        .controlled_by(protected_controller)
        .id();
    let other = scenario.add_creature(P1, "Other creature", 0, 9).id();
    let sources = [
        scenario
            .add_creature_from_oracle(P1, "Shivan Hellkite", 5, 5, HELLKITE)
            .id(),
        scenario
            .add_creature_from_oracle(P1, "Shivan Hellkite", 5, 5, HELLKITE)
            .id(),
    ];
    let rock = scenario
        .add_artifact_from_oracle(P1, "Nondamaging artifact", "")
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P1, "Ray of Command", true, RAY)
        .id();
    let disperse = scenario
        .add_spell_to_hand_from_oracle(P1, "Disperse", true, DISPERSE)
        .id();
    let memnarch = scenario
        .add_creature(P1, "Memnarch", 4, 5)
        .as_artifact_creature()
        .as_legendary()
        .from_oracle_text(MEMNARCH)
        .id();
    Board {
        runner: scenario.build(),
        jade,
        protected,
        other,
        sources,
        rock,
        ray,
        disperse,
        memnarch,
    }
}

fn announce_jade(b: &mut Board) {
    let abilities = &b.runner.state().objects[&b.jade].abilities;
    // SHAPE reach guard: the full Oracle must reach the intended CDR instruction.
    assert_eq!(abilities.len(), 1);
    assert_eq!(abilities[0].kind, AbilityKind::Activated);
    assert!(abilities[0].sub_ability.is_none());
    assert!(matches!(
        abilities[0].effect.as_ref(),
        Effect::CreateDamageReplacement {
            redirect_to: Some(DamageRedirectTarget::Controller),
            recipient_object_filter: Some(_),
            redirect_object_filter: None,
            source_filter: Some(TargetFilter::ChosenDamageSource { .. }),
            ..
        }
    ));
    add_mana(&mut b.runner, P0, ManaType::Colorless, 1);
    let before = mana(&b.runner, P0);
    announce_targeted(&mut b.runner, P0, b.jade, 0, b.protected);
    assert_eq!(mana(&b.runner, P0), before - 1, "printed {{1}} paid once");
}

fn source_prompt(b: &mut Board) {
    for _ in 0..16 {
        if matches!(
            b.runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        assert!(
            !b.runner.state().stack.is_empty(),
            "Jade must reach its source choice"
        );
        b.runner
            .act(GameAction::PassPriority)
            .expect("resolve Jade through priority passes");
    }
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = &b.runner.state().waiting_for
    else {
        panic!(
            "expected Jade's source choice: {:?}",
            b.runner.state().waiting_for
        );
    };
    // CR 109.5 + CR 609.7a: the activator chooses a source, including nondamaging permanents.
    assert_eq!(*player, P0);
    for source in [b.sources[0], b.sources[1], b.rock] {
        assert!(options.contains(&source));
    }
    assert_ne!(
        options.first(),
        Some(&b.sources[1]),
        "select a non-first source"
    );
    let continuation = b
        .runner
        .state()
        .active_ability_continuation()
        .expect("parked Jade instruction");
    assert_eq!(continuation.chain.controller, P0);
    assert_eq!(continuation.chain.source_id, b.jade);
    assert_eq!(
        continuation.chain.targets,
        vec![TargetRef::Object(b.protected)]
    );
}

fn installed_shield(runner: &GameRunner, protected: ObjectId) -> &ReplacementDefinition {
    let shields = &runner.state().objects[&protected].replacement_definitions;
    assert_eq!(
        shields.len(),
        1,
        "one shield is stored on the protected creature"
    );
    let shield = &shields[0];
    // CR 611.2c: resolution-created shields survive characteristic reseeding.
    assert!(shield.is_resolution_installed());
    assert_eq!(shield.valid_card, Some(TargetFilter::SelfRef));
    assert!(!shield.is_consumed);
    shield
}

fn choose_source(b: &mut Board) {
    source_prompt(b);
    b.runner
        .act(GameAction::ChooseDamageSource {
            source: b.sources[1],
        })
        .expect("resume Jade with the selected source");
    assert!(b.runner.state().stack.is_empty());
    assert!(
        b.runner.state().last_chosen_damage_source.is_none(),
        "transient choice drained"
    );
    let shield = installed_shield(&b.runner, b.protected);
    assert_eq!(
        shield.damage_source_filter,
        Some(TargetFilter::SpecificObject { id: b.sources[1] })
    );
    assert!(matches!(
        shield.shield_kind,
        ShieldKind::Redirection {
            amount: PreventionAmount::All,
            lifetime: RedirectionLifetime::OneOpportunity,
            ..
        }
    ));
}

fn install(b: &mut Board) {
    announce_jade(b);
    choose_source(b);
}

fn hellkite_damage(runner: &mut GameRunner, source: ObjectId, target: ObjectId) -> Vec<GameEvent> {
    // The full Oracle includes Flying. Select the unique damage activation rather than slot zero.
    let ability_index = {
        let mut matches = runner.state().objects[&source]
            .abilities
            .iter()
            .enumerate()
            .filter(|(_, ability)| {
                ability.kind == AbilityKind::Activated
                    && matches!(ability.effect.as_ref(), Effect::DealDamage { .. })
            });
        let (index, ability) = matches
            .next()
            .expect("full Hellkite has an activated DealDamage ability");
        assert!(
            matches.next().is_none(),
            "the printed activation is unambiguous"
        );
        assert!(ability.sub_ability.is_none());
        index
    };
    priority(runner, P1);
    add_mana(runner, P1, ManaType::Red, 2);
    let before = mana(runner, P1);
    let outcome = runner
        .activate(source, ability_index)
        .target_object(target)
        .resolve();
    assert_eq!(mana(runner, P1), before - 2, "printed {{1}}{{R}} paid");
    outcome.events().to_vec()
}

fn assert_damage(events: &[GameEvent], source: ObjectId, target: TargetRef) {
    // CR 120.2b: prove a nonzero event from the real activated damage source reaches its recipient.
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::DamageDealt {
        source_id, target: actual, amount: 1, ..
    } if *source_id == source && *actual == target)),
        "expected one damage from {source:?} to {target:?}: {events:?}"
    );
}

fn assert_redirected(b: &mut Board) {
    let lives = [b.runner.life(P0), b.runner.life(P1), b.runner.life(P2)];
    let events = hellkite_damage(&mut b.runner, b.sources[1], b.protected);
    assert!(
        events.iter().any(
            |event| matches!(event, GameEvent::ReplacementApplied { source_id, .. }
        if *source_id == b.protected)
        ),
        "recipient-hosted shield was applied: {events:?}"
    );
    // CR 109.5 + CR 113.8: "you" is P0, independently of either object's owner or later controller.
    assert_damage(&events, b.sources[1], TargetRef::Player(P0));
    // CR 120.3a + CR 614.9: only the implicit player loses life; the creature takes no damage.
    assert_eq!(b.runner.life(P0), lives[0] - 1);
    assert_eq!(b.runner.life(P1), lives[1]);
    assert_eq!(b.runner.life(P2), lives[2]);
    assert_eq!(b.runner.state().objects[&b.protected].damage_marked, 0);
}

fn resolve_response(b: &mut Board, spell: ObjectId, target: ObjectId) {
    priority(&mut b.runner, P1);
    let _ = b.runner.cast(spell).target_object(target).commit();
    b.runner.resolve_top(); // Uses real PassPriority actions and leaves the older Jade ability alone.
    assert_eq!(
        b.runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "response actually resolved"
    );
}

#[test]
fn full_oracle_hellkite_deals_unshielded_damage() {
    let mut b = board(P1);
    let events = hellkite_damage(&mut b.runner, b.sources[1], b.protected);
    assert_damage(&events, b.sources[1], TargetRef::Object(b.protected));
    // CR 120.3e: positive fixture control reaches real marked damage without a shield.
    assert_eq!(b.runner.state().objects[&b.protected].damage_marked, 1);
}

#[test]
fn jade_redirects_to_activator_not_host_controller_or_owner() {
    let mut b = board(P1);
    assert_eq!(b.runner.state().objects[&b.jade].owner, P2);
    assert_eq!(b.runner.state().objects[&b.jade].controller, P0);
    assert_eq!(b.runner.state().objects[&b.protected].owner, P2);
    assert_eq!(b.runner.state().objects[&b.protected].controller, P1);
    install(&mut b);
    assert_redirected(&mut b);
}

#[test]
fn jade_keeps_activator_when_protected_host_changes_controller() {
    for before_resolution in [true, false] {
        let mut b = board(P0);
        announce_jade(&mut b);
        if !before_resolution {
            choose_source(&mut b);
        }
        let (ray, protected) = (b.ray, b.protected);
        resolve_response(&mut b, ray, protected);
        assert_eq!(b.runner.state().objects[&b.protected].controller, P1);
        // CR 608.2b: Jade's unrestricted creature target stays legal across the control change.
        if before_resolution {
            choose_source(&mut b);
        }
        assert_redirected(&mut b);
    }
}

#[test]
fn jade_keeps_activator_when_memnarch_steals_its_source() {
    for before_resolution in [true, false] {
        let mut b = board(P1);
        announce_jade(&mut b);
        if !before_resolution {
            choose_source(&mut b);
        }
        let abilities = &b.runner.state().objects[&b.memnarch].abilities;
        assert_eq!(
            abilities.len(),
            2,
            "full Memnarch Oracle has two activations"
        );
        assert_eq!(abilities[1].kind, AbilityKind::Activated);
        assert!(
            matches!(abilities[1].effect.as_ref(), Effect::GainControl { .. }),
            "use Memnarch's second printed activation"
        );
        priority(&mut b.runner, P1);
        add_mana(&mut b.runner, P1, ManaType::Blue, 4);
        let before = mana(&b.runner, P1);
        announce_targeted(&mut b.runner, P1, b.memnarch, 1, b.jade);
        assert_eq!(mana(&b.runner, P1), before - 4, "printed {{3}}{{U}} paid");
        b.runner.resolve_top();
        assert_eq!(
            b.runner.state().objects[&b.jade].controller,
            P1,
            "real Memnarch activation resolved"
        );
        assert_eq!(b.runner.state().objects[&b.jade].owner, P2);
        // CR 113.8: the already activated Jade ability still belongs to P0.
        if before_resolution {
            assert_eq!(
                b.runner
                    .state()
                    .stack
                    .back()
                    .expect("Jade still on stack")
                    .controller,
                P0
            );
            choose_source(&mut b);
        }
        assert_redirected(&mut b);
    }
}

#[test]
fn jade_keeps_activator_when_its_source_returns_to_owners_hand() {
    for before_resolution in [true, false] {
        let mut b = board(P1);
        announce_jade(&mut b);
        if !before_resolution {
            choose_source(&mut b);
        }
        let (disperse, jade) = (b.disperse, b.jade);
        resolve_response(&mut b, disperse, jade);
        assert_eq!(b.runner.state().objects[&b.jade].zone, Zone::Hand);
        assert_eq!(b.runner.state().objects[&b.jade].owner, P2);
        assert_eq!(
            b.runner.state().objects[&b.protected].zone,
            Zone::Battlefield
        );
        // CR 113.7a: removing Jade doesn't remove its already stacked ability.
        // The protected host remains in play; no source-host lifetime repair is claimed.
        if before_resolution {
            choose_source(&mut b);
        }
        assert_redirected(&mut b);
    }
}

#[test]
fn jade_controller_binding_survives_source_prompt_and_installed_state_roundtrips() {
    let mut b = board(P1);
    announce_jade(&mut b);
    source_prompt(&mut b);
    let saved = serde_json::to_string(b.runner.state()).expect("serialize paused Jade");
    b.runner = GameRunner::from_state(serde_json::from_str(&saved).expect("restore paused Jade"));
    choose_source(&mut b);
    let saved = serde_json::to_string(b.runner.state()).expect("serialize installed shield");
    b.runner =
        GameRunner::from_state(serde_json::from_str(&saved).expect("restore installed shield"));
    let shield = installed_shield(&b.runner, b.protected);
    assert_eq!(
        shield.damage_source_filter,
        Some(TargetFilter::SpecificObject { id: b.sources[1] })
    );
    assert_eq!(
        shield.redirect_target,
        Some(TargetFilter::SpecificPlayer { id: P0 })
    );
    assert!(matches!(
        shield.shield_kind,
        ShieldKind::Redirection {
            recipient: DamageRedirectTarget::ChosenTarget,
            amount: PreventionAmount::All,
            lifetime: RedirectionLifetime::OneOpportunity,
        }
    ));
    assert_redirected(&mut b);
}

#[test]
fn jade_preserves_chosen_source_original_recipient_and_one_opportunity() {
    let mut b = board(P1);
    install(&mut b);
    let lives = [b.runner.life(P0), b.runner.life(P1), b.runner.life(P2)];
    let events = hellkite_damage(&mut b.runner, b.sources[0], b.protected);
    assert_damage(&events, b.sources[0], TargetRef::Object(b.protected));
    let events = hellkite_damage(&mut b.runner, b.sources[1], b.other);
    assert_damage(&events, b.sources[1], TargetRef::Object(b.other));
    assert_eq!(b.runner.state().objects[&b.protected].damage_marked, 1);
    assert_eq!(b.runner.state().objects[&b.other].damage_marked, 1);
    installed_shield(&b.runner, b.protected);
    // CR 609.7a + CR 614.9: only the selected source hitting the original creature redirects.
    let events = hellkite_damage(&mut b.runner, b.sources[1], b.protected);
    assert_damage(&events, b.sources[1], TargetRef::Player(P0));
    assert_eq!(b.runner.state().objects[&b.protected].damage_marked, 1);
    assert!(b.runner.state().objects[&b.protected].replacement_definitions[0].is_consumed);
    // Jade's printed "next time" was spent by the successful matching event.
    let events = hellkite_damage(&mut b.runner, b.sources[1], b.protected);
    assert_damage(&events, b.sources[1], TargetRef::Object(b.protected));
    assert_eq!(b.runner.state().objects[&b.protected].damage_marked, 2);
    assert_eq!(b.runner.life(P0), lives[0] - 1);
    assert_eq!(b.runner.life(P1), lives[1]);
    assert_eq!(b.runner.life(P2), lives[2]);
}

#[test]
fn vassals_duty_latches_activator_and_spends_exactly_one_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let duty = scenario
        .add_enchantment_from_oracle(P0, "Vassal's Duty", VASSALS_DUTY)
        .id();
    scenario
        .add_creature(P0, "Alternate legend", 0, 9)
        .as_legendary();
    let protected = scenario
        .add_creature(P0, "Protected legend", 0, 9)
        .as_legendary()
        .id();
    let source = scenario
        .add_creature_from_oracle(P1, "Shivan Hellkite", 5, 5, HELLKITE)
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P1, "Ray of Command", true, RAY)
        .id();
    let mut runner = scenario.build();
    let abilities = &runner.state().objects[&duty].abilities;
    assert_eq!(abilities.len(), 1);
    assert!(
        matches!(
            abilities[0].effect.as_ref(),
            Effect::CreateDamageReplacement {
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: Some(PreventionAmount::Next(1)),
                recipient_object_filter: Some(_),
                redirect_object_filter: None,
                source_filter: None,
                ..
            }
        ),
        "full Vassal's Duty Oracle must reach the next-one CDR branch"
    );
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    announce_targeted(&mut runner, P0, duty, 0, protected);
    assert_eq!(mana(&runner, P0), 0);
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no source-choice prompt"
    );
    assert!(matches!(
        installed_shield(&runner, protected).shield_kind,
        ShieldKind::Redirection {
            amount: PreventionAmount::Next(1),
            lifetime: RedirectionLifetime::OneOpportunity,
            ..
        }
    ));
    priority(&mut runner, P1);
    let outcome = runner.cast(ray).target_object(protected).resolve();
    outcome.assert_zone(&[ray], Zone::Graveyard);
    assert_eq!(runner.state().objects[&protected].controller, P1);
    // CR 109.5 + CR 614.9: after resolution "you control" no longer retargets the shield;
    // its implicit destination is still the player who activated Vassal's Duty.
    let lives = [runner.life(P0), runner.life(P1)];
    let events = hellkite_damage(&mut runner, source, protected);
    assert_damage(&events, source, TargetRef::Player(P0));
    assert_eq!(runner.state().objects[&protected].damage_marked, 0);
    let events = hellkite_damage(&mut runner, source, protected);
    assert_damage(&events, source, TargetRef::Object(protected));
    assert_eq!(runner.state().objects[&protected].damage_marked, 1);
    assert_eq!(runner.life(P0), lives[0] - 1);
    assert_eq!(runner.life(P1), lives[1]);
}
