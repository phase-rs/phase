//! Complete target restrictions for next-time damage redirection. Every runtime
//! row announces and resolves the printed ability through the action pipeline.
//! Opponent-announced targets are a separate, still-incomplete parser/runtime seam.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityKind, DamageRedirectTarget, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Scryfall Oracle, independently retrieved 2026-10-05.
const REGALIA: &str = "{3}: The next time a source of your choice would deal damage to you this turn, that damage is dealt to target creature you control instead.";
const JADE: &str = "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.";
const SOLTARI: &str = "Shadow (This creature can block or be blocked by only creatures with shadow.)\n{0}: The next time this creature would deal combat damage to an opponent this turn, it deals that damage to target creature instead.";
const HELLKITE: &str = "Flying (This creature can't be blocked except by creatures with flying or reach.)\n{1}{R}: This creature deals 1 damage to any target.";
const RAY: &str = "Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const NOMADS: &str = "{0}: The next 1 damage that would be dealt to this creature this turn is dealt to target creature you control instead.";
const HARMS_WAY: &str = "The next 2 damage that a source of your choice would deal to you and/or permanents you control this turn is dealt to any target instead.";

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
    if !matches!(runner.state().waiting_for, WaitingFor::Priority { player: current } if current == player)
    {
        runner
            .act(GameAction::PassPriority)
            .expect("pass to the acting player");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: current } if current == player)
    );
}

fn finish_stack(runner: &mut GameRunner) {
    for _ in 0..16 {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected resolution prompt: {:?}",
            runner.state().waiting_for
        );
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("resolve through priority passes");
    }
    panic!("stack did not finish");
}

fn assert_regalia_is_supported() {
    let parsed = parse_oracle_text(
        REGALIA,
        "General's Regalia",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    assert_eq!(parsed.abilities[0].kind, AbilityKind::Activated);
    assert!(parsed.abilities[0].sub_ability.is_none());
    assert!(
        matches!(
            parsed.abilities[0].effect.as_ref(),
            Effect::CreateDamageReplacement {
                redirect_to: Some(DamageRedirectTarget::ChosenTarget),
                redirect_object_filter: Some(_),
                ..
            }
        ),
        "the runtime test must reach a supported replacement, not an Unimplemented short-circuit"
    );
}

fn announce(runner: &mut GameRunner, artifact: ObjectId, target: ObjectId) {
    priority(runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: artifact,
            ability_index: 0,
        })
        .expect("announce the printed activated ability");
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the declared creature target, got {:?}",
            runner.state().waiting_for
        );
    };
    // CR 115.1c + CR 602.2b: the creature target is declared at activation.
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(target)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("submit a legal creature");
    let entry = runner
        .state()
        .stack
        .back()
        .expect("activation on the stack");
    // CR 602.2a: the activated ability retains its source and activator.
    assert_eq!(entry.controller, P0);
    assert!(
        matches!(&entry.kind, StackEntryKind::ActivatedAbility { source_id, ability }
        if *source_id == artifact && ability.controller == P0
            && ability.ability_index == Some(0)
            && ability.targets == vec![TargetRef::Object(target)])
    );
}

fn choose_source(runner: &mut GameRunner, source: ObjectId, nondamaging: ObjectId) {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "ability must reach its source choice"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the activated ability");
    }
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a real source choice, got {:?}",
            runner.state().waiting_for
        );
    };
    // CR 609.7a: the activator chooses on resolution, including nondamaging permanents.
    assert_eq!(*player, P0);
    assert!(options.contains(&source));
    assert!(options.contains(&nondamaging));
    assert_ne!(
        options.first(),
        Some(&source),
        "exercise a non-first source choice"
    );
    runner
        .act(GameAction::ChooseDamageSource { source })
        .expect("resume with the chosen source");
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().last_chosen_damage_source.is_none(),
        "choice continuation was consumed"
    );
}

fn hellkite_damage(runner: &mut GameRunner, source: ObjectId, target: TargetRef) -> Vec<GameEvent> {
    // The full Oracle includes a keyword line, so ability zero need not be the activation.
    let ability_index = {
        let mut damage_abilities = runner.state().objects[&source]
            .abilities
            .iter()
            .enumerate()
            .filter(|(_, ability)| {
                ability.kind == AbilityKind::Activated
                    && matches!(ability.effect.as_ref(), Effect::DealDamage { .. })
            });
        let (index, ability) = damage_abilities
            .next()
            .expect("the full Hellkite Oracle must expose an activated DealDamage ability");
        assert!(
            damage_abilities.next().is_none(),
            "the printed damage activation must be unambiguous"
        );
        assert!(ability.sub_ability.is_none());
        index
    };
    priority(runner, P1);
    add_mana(runner, P1, ManaType::Red, 2);
    let mana_before = mana(runner, P1);
    let outcome = match target {
        TargetRef::Player(player) => runner
            .activate(source, ability_index)
            .target_player(player)
            .resolve(),
        TargetRef::Object(object) => runner
            .activate(source, ability_index)
            .target_object(object)
            .resolve(),
    };
    assert_eq!(mana(runner, P1), mana_before - 2, "printed {{1}}{{R}} paid");
    outcome.events().to_vec()
}

fn assert_damage(events: &[GameEvent], source: ObjectId, target: TargetRef, amount: u32) {
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::DamageDealt {
        source_id, target: actual, amount: actual_amount, ..
    } if *source_id == source && *actual == target && *actual_amount == amount)),
        "expected nonzero damage from the selected source to {target:?}: {events:?}"
    );
}

struct Board {
    runner: GameRunner,
    regalia: ObjectId,
    chosen: ObjectId,
    other: ObjectId,
    sources: [ObjectId; 2],
    rock: ObjectId,
    ray: ObjectId,
    bounce: ObjectId,
}

fn board() -> Board {
    assert_regalia_is_supported();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let regalia = scenario
        .add_artifact_from_oracle(P0, "General's Regalia", REGALIA)
        .id();
    let chosen = scenario
        .add_creature(P0, "Chosen recipient", 0, 9)
        .with_keyword(Keyword::Flash)
        .id();
    let other = scenario.add_creature(P0, "Other recipient", 0, 9).id();
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
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Colorless, 3);
    Board {
        runner,
        regalia,
        chosen,
        other,
        sources,
        rock,
        ray,
        bounce,
    }
}

fn install(board: &mut Board) {
    announce(&mut board.runner, board.regalia, board.chosen);
    assert_eq!(
        mana(&board.runner, P0),
        0,
        "printed three-mana activation paid exactly once"
    );
    choose_source(&mut board.runner, board.sources[1], board.rock);
    assert!(
        !board.runner.state().objects[&board.regalia]
            .replacement_definitions
            .is_empty(),
        "resolution installed the shield before any damage"
    );
}

fn change_target(board: &mut Board, spell: ObjectId) {
    priority(&mut board.runner, P1);
    let _ = board
        .runner
        .cast(spell)
        .target_object(board.chosen)
        .commit();
    // GameRunner::resolve_top passes real priority actions; it does not call a resolver directly.
    board.runner.resolve_top();
    assert_eq!(
        board.runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "the actual response spell resolved before checking target legality"
    );
}

#[test]
fn hellkite_full_oracle_activation_deals_unshielded_damage_from_the_selected_source() {
    let mut b = board();
    let source = b.sources[1];
    let life = b.runner.life(P0);
    assert!(b.runner.state().objects[&b.regalia]
        .replacement_definitions
        .is_empty());

    // CR 120.2b + CR 120.3a: the real activated source deals damage and reduces life.
    let events = hellkite_damage(&mut b.runner, source, TargetRef::Player(P0));
    assert_damage(&events, source, TargetRef::Player(P0), 1);
    assert_eq!(b.runner.life(P0), life - 1);

    // CR 120.3e: the same source can instead mark damage on the chosen creature.
    let events = hellkite_damage(&mut b.runner, source, TargetRef::Object(b.chosen));
    assert_damage(&events, source, TargetRef::Object(b.chosen), 1);
    assert_eq!(b.runner.state().objects[&b.chosen].damage_marked, 1);
    assert_eq!(b.runner.life(P0), life - 1);
}

#[test]
fn regalia_activation_uses_controller_not_owner_for_its_creature_target() {
    assert_regalia_is_supported();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let regalia = scenario
        .add_artifact_from_oracle(P1, "General's Regalia", REGALIA)
        .controlled_by(P0)
        .id();
    let own = scenario.add_creature(P0, "Own creature", 0, 9).id();
    let borrowed = scenario
        .add_creature(P1, "Borrowed creature", 0, 9)
        .controlled_by(P0)
        .id();
    let stolen = scenario
        .add_creature(P0, "Stolen creature", 0, 9)
        .controlled_by(P1)
        .id();
    let foe = scenario.add_creature(P1, "Foe", 0, 9).id();
    let shrouded = scenario
        .add_creature(P0, "Shrouded creature", 0, 9)
        .with_keyword(Keyword::Shroud)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Colorless, 3);
    runner
        .act(GameAction::ActivateAbility {
            source_id: regalia,
            ability_index: 0,
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("a real activation must reach target selection");
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    let legal = &target_slots[0].legal_targets;
    // CR 109.5 + CR 115.1c: 'you control' means the activator, regardless of ownership.
    assert_eq!(legal.len(), 2);
    assert!(legal.contains(&TargetRef::Object(own)));
    assert!(legal.contains(&TargetRef::Object(borrowed)));
    for illegal in [stolen, foe, shrouded] {
        assert!(!legal.contains(&TargetRef::Object(illegal)));
    }
    assert_eq!(mana(&runner, P0), 3);
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(stolen))
        })
        .is_err());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(borrowed)),
        })
        .unwrap();
    assert_eq!(mana(&runner, P0), 0);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack.back().unwrap().controller, P0);
}

#[test]
fn regalia_without_a_targetable_controlled_creature_cannot_activate() {
    assert_regalia_is_supported();
    for shroud in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let regalia = scenario
            .add_artifact_from_oracle(P0, "General's Regalia", REGALIA)
            .id();
        if shroud {
            scenario
                .add_creature(P0, "Shrouded", 0, 9)
                .with_keyword(Keyword::Shroud);
        } else {
            scenario.add_creature(P1, "Only opponent creature", 0, 9);
        }
        let mut runner = scenario.build();
        add_mana(&mut runner, P0, ManaType::Colorless, 3);
        // CR 601.2c + CR 602.2b + CR 702.18a: required targets must be legal before payment.
        assert!(runner
            .act(GameAction::ActivateAbility {
                source_id: regalia,
                ability_index: 0
            })
            .is_err());
        assert_eq!(mana(&runner, P0), 3);
        assert!(runner.state().stack.is_empty());
    }
    let mut positive = board();
    install(&mut positive);
    let events = hellkite_damage(
        &mut positive.runner,
        positive.sources[1],
        TargetRef::Player(P0),
    );
    assert_damage(
        &events,
        positive.sources[1],
        TargetRef::Object(positive.chosen),
        1,
    );
}

#[test]
fn regalia_rechecks_control_on_resolution_but_not_after_shield_creation() {
    for before_resolution in [true, false] {
        let mut b = board();
        if before_resolution {
            announce(&mut b.runner, b.regalia, b.chosen);
        } else {
            install(&mut b);
        }
        let ray = b.ray;
        change_target(&mut b, ray);
        assert_eq!(b.runner.state().objects[&b.chosen].controller, P1);
        if before_resolution {
            // CR 608.2b: the sole target is now illegal, so no source choice or shield is created.
            finish_stack(&mut b.runner);
            assert!(b.runner.state().objects[&b.regalia]
                .replacement_definitions
                .is_empty());
            assert!(b.runner.state().pending_damage_replacements.is_empty());
        }
        let life = b.runner.life(P0);
        let events = hellkite_damage(&mut b.runner, b.sources[1], TargetRef::Player(P0));
        // CR 614.9: after creation the latched creature still receives damage despite changing controller.
        let target = if before_resolution {
            TargetRef::Player(P0)
        } else {
            TargetRef::Object(b.chosen)
        };
        assert_damage(&events, b.sources[1], target, 1);
        assert_eq!(b.runner.life(P0), life - i32::from(before_resolution));
        assert_eq!(
            b.runner.state().objects[&b.chosen].damage_marked,
            u32::from(!before_resolution)
        );
    }
}

#[test]
fn regalia_departed_target_does_not_receive_damage_before_or_after_resolution() {
    for before_resolution in [true, false] {
        let mut b = board();
        if before_resolution {
            announce(&mut b.runner, b.regalia, b.chosen);
        } else {
            install(&mut b);
        }
        let bounce = b.bounce;
        change_target(&mut b, bounce);
        assert_eq!(b.runner.state().objects[&b.chosen].zone, Zone::Hand);
        if before_resolution {
            // CR 608.2b: a target that left its zone is illegal when the ability would resolve.
            finish_stack(&mut b.runner);
            assert!(b.runner.state().objects[&b.regalia]
                .replacement_definitions
                .is_empty());
        }
        let life = b.runner.life(P0);
        let events = hellkite_damage(&mut b.runner, b.sources[1], TargetRef::Player(P0));
        // CR 614.9: redirection does nothing while its recipient is off the battlefield.
        assert_damage(&events, b.sources[1], TargetRef::Player(P0), 1);
        assert_eq!(b.runner.life(P0), life - 1);
    }
    let mut positive = board();
    install(&mut positive);
    let events = hellkite_damage(
        &mut positive.runner,
        positive.sources[1],
        TargetRef::Player(P0),
    );
    assert_damage(
        &events,
        positive.sources[1],
        TargetRef::Object(positive.chosen),
        1,
    );
}

#[test]
fn regalia_chooses_a_source_at_resolution_and_redirects_only_its_next_damage_to_you() {
    let mut b = board();
    install(&mut b);
    let life = b.runner.life(P0);
    let events = hellkite_damage(&mut b.runner, b.sources[0], TargetRef::Player(P0));
    assert_damage(&events, b.sources[0], TargetRef::Player(P0), 1);
    // CR 115.10a: the damage source and implicit 'you' recipient are not creature target slots.
    let events = hellkite_damage(&mut b.runner, b.sources[1], TargetRef::Object(b.other));
    assert_damage(&events, b.sources[1], TargetRef::Object(b.other), 1);
    let events = hellkite_damage(&mut b.runner, b.sources[1], TargetRef::Player(P0));
    assert_damage(&events, b.sources[1], TargetRef::Object(b.chosen), 1);
    assert_eq!(b.runner.life(P0), life - 1);
    let events = hellkite_damage(&mut b.runner, b.sources[1], TargetRef::Player(P0));
    assert_damage(&events, b.sources[1], TargetRef::Player(P0), 1);
    assert_eq!(b.runner.life(P0), life - 2);
    assert_eq!(b.runner.state().objects[&b.chosen].damage_marked, 1);
}

#[test]
fn regalia_does_not_reuse_a_target_that_leaves_and_returns_before_resolution() {
    let mut b = board();
    let incarnation = b.runner.state().objects[&b.chosen].incarnation;
    announce(&mut b.runner, b.regalia, b.chosen);
    let bounce = b.bounce;
    change_target(&mut b, bounce);
    assert_eq!(b.runner.state().objects[&b.chosen].zone, Zone::Hand);
    priority(&mut b.runner, P0);
    // The synthetic recipient has flash, so it can be recast above the pending activation.
    let _ = b.runner.cast(b.chosen).commit();
    b.runner.resolve_top();
    assert_eq!(b.runner.state().objects[&b.chosen].zone, Zone::Battlefield);
    assert_eq!(b.runner.state().objects[&b.chosen].controller, P0);
    assert!(b.runner.state().objects[&b.chosen].incarnation > incarnation);
    // CR 400.7 + CR 608.2b: the returned creature is a new object, not the declared target.
    finish_stack(&mut b.runner);
    assert!(b.runner.state().objects[&b.regalia]
        .replacement_definitions
        .is_empty());
    let events = hellkite_damage(&mut b.runner, b.sources[1], TargetRef::Player(P0));
    assert_damage(&events, b.sources[1], TargetRef::Player(P0), 1);
    assert_eq!(b.runner.state().objects[&b.chosen].damage_marked, 0);

    let mut positive = board();
    install(&mut positive);
    let events = hellkite_damage(
        &mut positive.runner,
        positive.sources[1],
        TargetRef::Player(P0),
    );
    assert_damage(
        &events,
        positive.sources[1],
        TargetRef::Object(positive.chosen),
        1,
    );
}

#[test]
fn jade_monolith_declares_the_original_creature_and_redirects_to_its_activator() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jade = scenario
        .add_artifact_from_oracle(P0, "Jade Monolith", JADE)
        .id();
    let creature = scenario
        .add_creature(P1, "Protected opponent creature", 0, 9)
        .id();
    let source = scenario
        .add_creature_from_oracle(P1, "Shivan Hellkite", 5, 5, HELLKITE)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    announce(&mut runner, jade, creature);
    choose_source(&mut runner, source, jade);
    let life = runner.life(P0);
    let events = hellkite_damage(&mut runner, source, TargetRef::Object(creature));
    // CR 614.9: Jade's targeted original recipient is distinct from its implicit redirect recipient.
    assert_damage(&events, source, TargetRef::Player(P0), 1);
    assert_eq!(runner.life(P0), life - 1);
    assert_eq!(runner.state().objects[&creature].damage_marked, 0);
}

#[test]
fn soltari_guerrillas_redirects_its_combat_damage_to_an_unqualified_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Soltari Guerrillas", 3, 2)
        .from_oracle_text_with_keywords(&["Shadow"], SOLTARI)
        .id();
    let recipient = scenario.add_creature(P1, "Opponent recipient", 0, 9).id();
    scenario.add_creature(P0, "Own recipient", 0, 9);
    let mut runner = scenario.build();
    announce(&mut runner, source, recipient);
    finish_stack(&mut runner);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .unwrap();
    // CR 117.4 + CR 508.2: pass the post-attack priority window before blockers.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert_eq!(runner.state().phase, Phase::DeclareBlockers);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.declare_blockers(&[]).unwrap();
    }
    // CR 509.2 + CR 702.28b: shadow leaves no legal blockers here; the engine
    // can already have made the empty declaration and returned priority to P0.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    let outcome = runner.combat_damage();
    // CR 614.9: the source's combat damage moves to the declared creature of either controller.
    assert_damage(outcome.events(), source, TargetRef::Object(recipient), 3);
    assert_eq!(outcome.life_delta(P1), 0);
    assert_eq!(outcome.state().objects[&recipient].damage_marked, 3);
}

#[test]
fn next_n_redirect_siblings_preserve_their_distinct_target_roles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nomads = scenario
        .add_creature_from_oracle(P0, "Nomads en-Kor", 1, 1, NOMADS)
        .id();
    let recipient = scenario.add_creature(P0, "Redirect recipient", 0, 9).id();
    let source = scenario
        .add_creature_from_oracle(P1, "Shivan Hellkite", 5, 5, HELLKITE)
        .id();
    let harms_way = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .id();
    let mut runner = scenario.build();
    runner
        .activate(nomads, 0)
        .target_object(recipient)
        .resolve();
    let events = hellkite_damage(&mut runner, source, TargetRef::Object(nomads));
    assert_damage(&events, source, TargetRef::Object(recipient), 1);
    assert_eq!(runner.state().objects[&nomads].zone, Zone::Battlefield);
    priority(&mut runner, P0);
    let cast = runner.cast(harms_way).target_player(P1).resolve();
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::DamageSourceChoice { player: P0, .. }
    ));
    choose_source(&mut runner, source, nomads);
    let life = runner.life(P0);
    let events = hellkite_damage(&mut runner, source, TargetRef::Player(P0));
    assert_damage(&events, source, TargetRef::Player(P1), 1);
    assert_eq!(runner.life(P0), life);
}
