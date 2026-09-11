//! Exact Oracle routing coverage for source-controller damage redirection.

use engine::game::combat::AttackTarget;
use engine::game::effects::{create_damage_replacement, deal_damage};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityKind, CombatDamageScope, DamageRedirectTarget, Effect, QuantityExpr,
    RedirectionLifetime, ResolvedAbility, ShieldKind, TargetFilter, TargetRef, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MIRROR_STRIKE: &str =
    "All combat damage that would be dealt to you this turn by target unblocked creature is dealt to its controller instead.";
const REVERBERATION: &str =
    "All damage that would be dealt this turn by target sorcery spell is dealt to that spell's controller instead.";
const REFLECT_DAMAGE: &str =
    "The next time a source of your choice would deal damage this turn, that damage is dealt to that source's controller instead.";
const AEGIS_OF_HONOR: &str =
    "{1}: The next time an instant or sorcery spell would deal damage to you this turn, that spell deals that damage to its controller instead.";
const CAROM: &str = "The next 1 damage that would be dealt to target creature this turn is dealt to another target creature instead.\nDraw a card.";
const INVULNERABILITY: &str = "Buyback {3} (You may pay an additional {3} as you cast this spell. If you do, put this card into your hand as it resolves.)\nThe next time a source of your choice would deal damage to you this turn, prevent that damage.";
const LAVA_AXE: &str = "Lava Axe deals 5 damage to target player or planeswalker.";
const COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it.";
const REDIRECT: &str = "You may choose new targets for target spell.";

fn spell_effect(text: &str, name: &str) -> Effect {
    *parse_oracle_text(text, name, &[], &["Instant".to_string()], &[])
        .abilities
        .into_iter()
        .find(|ability| matches!(ability.kind, AbilityKind::Spell))
        .expect("the spell ability must parse")
        .effect
}

fn activated_effect(text: &str, name: &str) -> Effect {
    *parse_oracle_text(text, name, &[], &["Enchantment".to_string()], &[])
        .abilities
        .into_iter()
        .find(|ability| matches!(ability.kind, AbilityKind::Activated))
        .expect("the activated ability must parse")
        .effect
}

/// Make a real damage event through the engine's normal `DealDamage` resolver.
/// The replacement event is reached from that resolver; this merely supplies a
/// small repeatable source for the post-choice witness below.
fn damage_from(source: ObjectId, controller: engine::types::player::PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Player(P0)],
        source,
        controller,
    )
}

/// Advance the real multiplayer priority loop until the current top stack item
/// has resolved. The optional retarget rider on Commandeer is deliberately
/// declined: this witness observes control, not target selection.
fn resolve_one_stack_item(runner: &mut engine::game::scenario::GameRunner) {
    let stack_len = runner.state().stack.len();
    for _ in 0..24 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.len() < stack_len => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("a player may pass priority");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("declining Commandeer's optional retarget rider succeeds");
            }
            ref other => panic!("unexpected wait state resolving a stack item: {other:?}"),
        }
    }
    panic!("priority loop did not resolve one stack item");
}

fn pass_priority_to(runner: &mut engine::game::scenario::GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: current } if current == player)
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("a non-active player may pass priority");
    }
    panic!("priority did not reach {player:?}");
}

#[test]
fn exact_source_controller_oracle_texts_route_through_oneshot_damage_replacement() {
    let mirror = spell_effect(MIRROR_STRIKE, "Mirror Strike");
    let reverberation = spell_effect(REVERBERATION, "Reverberation");
    let reflect = spell_effect(REFLECT_DAMAGE, "Reflect Damage");

    for (effect, scope, recipient) in [
        (
            mirror,
            Some(CombatDamageScope::CombatOnly),
            Some(engine::types::ability::DamageTargetFilter::Player {
                player: engine::types::ability::DamageTargetPlayerScope::Controller,
            }),
        ),
        (reverberation, None, None),
    ] {
        let Effect::CreateDamageReplacement {
            source_filter,
            combat_scope,
            target_filter,
            redirect_to,
            redirect_lifetime,
            ..
        } = effect
        else {
            panic!("expected CreateDamageReplacement, got {effect:?}");
        };
        assert_eq!(combat_scope, scope);
        assert_eq!(target_filter, recipient);
        assert_eq!(
            redirect_to,
            Some(DamageRedirectTarget::DamageSourceController)
        );
        assert_eq!(redirect_lifetime, RedirectionLifetime::Continuous);
        assert!(matches!(source_filter, Some(TargetFilter::And { filters })
            if matches!(filters.first(), Some(TargetFilter::ParentTargetSlot { index: 0 }))));
    }

    assert!(matches!(
        reflect,
        Effect::CreateDamageReplacement {
            source_filter: Some(TargetFilter::ChosenDamageSource { .. }),
            redirect_to: Some(DamageRedirectTarget::DamageSourceController),
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
            ..
        }
    ));
}

#[test]
fn aegis_of_honor_exact_oracle_parses_to_source_controller_redirection() {
    let effect = activated_effect(AEGIS_OF_HONOR, "Aegis of Honor");

    assert!(matches!(
        effect,
        Effect::CreateDamageReplacement {
            redirect_to: Some(DamageRedirectTarget::DamageSourceController),
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
            ..
        }
    ));
}

#[test]
fn non_source_controller_oneshot_oracles_fall_through_to_their_existing_routes() {
    let carom = parse_oracle_text(CAROM, "Carom", &[], &["Instant".to_string()], &[]);
    let carom_spell = carom
        .abilities
        .iter()
        .find(|ability| matches!(ability.kind, AbilityKind::Spell))
        .expect("Carom must have a spell ability");
    assert!(matches!(
        carom_spell.effect.as_ref(),
        Effect::CreateDamageReplacement {
            redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
            ..
        }
    ));
    assert!(matches!(
        carom_spell
            .sub_ability
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::Draw { .. })
    ));

    let invulnerability = parse_oracle_text(
        INVULNERABILITY,
        "Invulnerability",
        &["Buyback".to_string()],
        &["Instant".to_string()],
        &[],
    );
    assert!(
        invulnerability
            .extracted_keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Buyback(_))),
        "Invulnerability's exact Buyback line must remain on the normal keyword route"
    );
    assert!(matches!(
        invulnerability.replacements.first(),
        Some(replacement) if matches!(replacement.shield_kind, ShieldKind::Prevention { .. })
    ));
}

/// CR 115.1a + CR 608.2b + CR 614.9: all declared roles define a single
/// replacement event. If the source target becomes illegal while the later
/// redirect-recipient target remains legal, the spell still resolves but this
/// CDR instruction does nothing; its positions cannot compact into a misbound
/// shield.
#[test]
fn invalid_declared_damage_source_cannot_rebind_a_later_redirect_target() {
    let creature = TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature));
    let mut scenario = GameScenario::new();
    let shield_source = scenario.add_creature(P0, "Shield source", 1, 1).id();
    let declared_source = scenario.add_creature(P1, "Declared source", 2, 2).id();
    let redirect_recipient = scenario.add_creature(P0, "Redirect recipient", 3, 3).id();
    let mut runner = scenario.build();

    let original = ResolvedAbility::new(
        Effect::CreateDamageReplacement {
            source_filter: Some(TargetFilter::And {
                filters: vec![
                    TargetFilter::ParentTargetSlot { index: 0 },
                    creature.clone(),
                ],
            }),
            combat_scope: None,
            target_filter: None,
            modification: None,
            redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
            redirect_amount: None,
            redirect_object_filter: Some(creature),
            recipient_object_filter: None,
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
        },
        vec![
            TargetRef::Object(declared_source),
            TargetRef::Object(redirect_recipient),
        ],
        shield_source,
        P0,
    );

    // The recipient remains a legal creature, while only the first (source)
    // role has become illegal after target declaration.
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        declared_source,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    assert!(runner.state().battlefield.contains(&redirect_recipient));

    let validated =
        engine::game::ability_utils::validate_targets_in_chain(runner.state(), &original);
    assert_eq!(
        validated.targets, original.targets,
        "a legal later role preserves the original declaration-order slots: {validated:?}"
    );
    assert!(
        !engine::game::targeting::check_fizzle(&original.targets, &validated.targets),
        "CR 608.2b keeps the spell resolving while its later target remains legal"
    );

    let mut events = Vec::new();
    create_damage_replacement::resolve(runner.state_mut(), &validated, &mut events)
        .expect("a partial CDR instruction resolves as a no-op");
    assert!(matches!(
        events.as_slice(),
        [GameEvent::EffectResolved { .. }]
    ));
    assert!(
        runner.state().objects[&shield_source]
            .replacement_definitions
            .is_empty()
            && runner.state().pending_damage_replacements.is_empty(),
        "an illegal source role installs no replacement shield"
    );
}

#[test]
fn legal_declared_damage_source_installs_a_redirect_replacement() {
    let creature = TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature));
    let mut scenario = GameScenario::new();
    let shield_source = scenario.add_creature(P0, "Shield source", 1, 1).id();
    let declared_source = scenario.add_creature(P1, "Declared source", 2, 2).id();
    let redirect_recipient = scenario.add_creature(P0, "Redirect recipient", 3, 3).id();
    let mut runner = scenario.build();

    let original = ResolvedAbility::new(
        Effect::CreateDamageReplacement {
            source_filter: Some(TargetFilter::And {
                filters: vec![
                    TargetFilter::ParentTargetSlot { index: 0 },
                    creature.clone(),
                ],
            }),
            combat_scope: None,
            target_filter: None,
            modification: None,
            redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
            redirect_amount: None,
            redirect_object_filter: Some(creature),
            recipient_object_filter: None,
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
        },
        vec![
            TargetRef::Object(declared_source),
            TargetRef::Object(redirect_recipient),
        ],
        shield_source,
        P0,
    );

    let validated =
        engine::game::ability_utils::validate_targets_in_chain(runner.state(), &original);
    assert_eq!(validated.targets, original.targets);

    let mut events = Vec::new();
    create_damage_replacement::resolve(runner.state_mut(), &validated, &mut events)
        .expect("a fully legal CDR instruction resolves");
    assert!(
        !runner.state().objects[&shield_source]
            .replacement_definitions
            .is_empty(),
        "the legal declared source and redirect recipient install a replacement shield"
    );
}

#[test]
fn mirror_strike_uses_the_target_attackers_live_controller_for_both_damage_steps() {
    // Start in P0's end step, then advance through cleanup into P1's turn. This
    // exercises the actual priority/cast pipeline rather than changing the
    // active player or target list directly in state.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["P0 draw"]);
    scenario.with_library_top(P1, &["P1 draw"]);
    let mirror = scenario
        .add_spell_to_hand_from_oracle(P0, "Mirror Strike", true, MIRROR_STRIKE)
        .id();
    let attacker = scenario
        .add_creature(P1, "Double striker", 3, 20)
        .with_keyword(Keyword::DoubleStrike)
        .id();
    let bystander = scenario.add_creature(P1, "Bystander", 2, 20).id();
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P1,
        "P1 must take the combat turn"
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (attacker, AttackTarget::Player(P0)),
            (bystander, AttackTarget::Player(P0)),
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
            .expect("P0 deliberately declares no blockers");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the blocker step must reach a priority window for Mirror Strike"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to the defending caster");

    runner.cast(mirror).target_objects(&[attacker]).resolve();
    let outcome = runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P1),
        -6,
        "the double striker's first-strike and regular damage both redirect to P1"
    );
    assert_eq!(
        outcome.life_delta(P0),
        -2,
        "the untargeted bystander still deals combat damage to P0"
    );
}

/// CR 609.7a + CR 614.5 + CR 614.9: Reflect Damage pauses at the real source
/// choice, binds exactly the chosen source, redirects that source's next damage
/// to its live controller, and consumes itself after that one event.
#[test]
fn reflect_damage_choice_redirects_only_the_chosen_sources_next_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reflect = scenario
        .add_spell_to_hand_from_oracle(P0, "Reflect Damage", true, REFLECT_DAMAGE)
        .id();
    let chosen = scenario.add_creature(P1, "Chosen source", 3, 3).id();
    let other = scenario.add_creature(P1, "Other source", 3, 3).id();
    let mut runner = scenario.build();

    let cast = runner.cast(reflect).resolve();
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = cast.final_waiting_for()
    else {
        panic!(
            "Reflect Damage must stop at its CR 609.7a source choice, got {:?}",
            cast.final_waiting_for()
        );
    };
    assert_eq!(*player, P0);
    assert!(options.contains(&chosen));
    assert!(options.contains(&other));
    runner
        .act(GameAction::ChooseDamageSource { source: chosen })
        .expect("the selected source must bind the pending replacement");

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);
    let mut events = Vec::<GameEvent>::new();
    deal_damage::resolve(runner.state_mut(), &damage_from(other, P1), &mut events)
        .expect("the unchosen source damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_before - 3,
        "unchosen source remains normal"
    );
    assert_eq!(runner.life(P1), p1_before);

    events.clear();
    deal_damage::resolve(runner.state_mut(), &damage_from(chosen, P1), &mut events)
        .expect("the chosen source's first damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_before - 3,
        "chosen damage no longer reaches P0"
    );
    assert_eq!(
        runner.life(P1),
        p1_before - 3,
        "chosen damage redirects to its controller"
    );

    deal_damage::resolve(runner.state_mut(), &damage_from(chosen, P1), &mut events)
        .expect("the chosen source's second damage resolves");
    assert_eq!(
        runner.life(P0),
        p0_before - 6,
        "the one-shot shield is consumed"
    );
    assert_eq!(
        runner.life(P1),
        p1_before - 3,
        "only the first chosen event redirects"
    );
}

/// CR 510.1b + CR 510.2 + CR 602.2b + CR 609.7b + CR 614.1a + CR 614.9:
/// Aegis's one-shot replacement ignores a creature source's combat damage,
/// then redirects the next matching spell's damage to the spell's live controller.
#[test]
fn aegis_of_honor_ignores_creature_damage_and_redirects_matching_spell_to_live_controller() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["P0 draw"]);
    scenario.with_library_top(P1, &["P1 draw"]);
    scenario.with_library_top(p2, &["P2 draw"]);
    let aegis = scenario
        .add_enchantment_from_oracle(P0, "Aegis of Honor", AEGIS_OF_HONOR)
        .id();
    let _potential_blocker = scenario.add_creature(P0, "Potential blocker", 1, 1).id();
    let attacker = scenario.add_creature(P1, "Aegis filter witness", 3, 3).id();
    let axe = scenario
        .add_spell_to_hand_from_oracle(P1, "Lava Axe", false, LAVA_AXE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let commandeer = scenario
        .add_spell_to_hand_from_oracle(p2, "Commandeer", true, COMMANDEER)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    for _ in 0..12 {
        if runner.state().phase == Phase::PreCombatMain {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("production priority passing advances P0's end step into P1's main phase");
    }
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));

    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to P2");
    runner
        .act(GameAction::PassPriority)
        .expect("P2 passes priority to P0");
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    let added_mana = runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    );
    assert!(
        added_mana.is_some(),
        "the active priority window accepts the fresh Aegis activation mana"
    );
    runner.activate(aegis, 0).resolve();
    assert!(
        !runner.state().objects[&aegis]
            .replacement_definitions
            .is_empty(),
        "resolving Aegis of Honor must install its role-less replacement"
    );
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));

    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes through P1's precombat main phase");
    runner
        .act(GameAction::PassPriority)
        .expect("P2 passes through P1's precombat main phase");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes through P1's precombat main phase");
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes through P1's beginning of combat step");
    runner
        .act(GameAction::PassPriority)
        .expect("P2 passes through P1's beginning of combat step");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes through P1's beginning of combat step");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1's production combat step accepts the unblocked attacker");
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes after declaring attackers");
    runner
        .act(GameAction::PassPriority)
        .expect("P2 passes after declaring attackers");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes after declaring attackers");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    runner
        .declare_blockers(&[])
        .expect("P0 deliberately declares no blockers");

    let combat = runner.combat_damage();
    assert_eq!(
        combat.life_delta(P0),
        -3,
        "the creature deals combat damage to P0"
    );
    assert_eq!(
        combat.life_delta(P1),
        0,
        "the creature's controller takes no combat damage"
    );
    assert_eq!(
        combat.life_delta(p2),
        0,
        "the third player takes no combat damage"
    );
    assert_eq!(runner.life(P0), 17, "the unblocked creature damages P0");
    assert_eq!(runner.life(P1), 20, "combat does not damage P1");
    assert_eq!(runner.life(p2), 20, "combat does not damage P2");
    assert_eq!(
        runner.state().objects[&aegis].replacement_definitions.len(),
        1,
        "Aegis retains its single replacement definition after nonmatching damage"
    );
    assert!(
        !runner.state().objects[&aegis].replacement_definitions[0].is_consumed,
        "nonmatching creature damage must not consume Aegis's one-shot replacement"
    );

    for _ in 0..12 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        runner.act(GameAction::PassPriority).expect(
            "production priority passing advances end of combat into P1's postcombat main phase",
        );
    }
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));

    runner.cast(axe).target_player(P0).commit();
    pass_priority_to(&mut runner, p2);
    runner.cast(commandeer).target_objects(&[axe]).commit();
    resolve_one_stack_item(&mut runner);

    assert_eq!(
        runner.state().objects[&axe].controller,
        p2,
        "Commandeer must change Axe's live controller before it deals damage"
    );
    resolve_one_stack_item(&mut runner);
    assert_eq!(runner.life(P0), 17, "Aegis redirects Axe away from P0");
    assert_eq!(
        runner.life(P1),
        20,
        "Axe's owner is not its live controller"
    );
    assert_eq!(
        runner.life(p2),
        15,
        "Aegis redirects damage to Axe's live controller"
    );
    assert!(
        runner.state().objects[&aegis].replacement_definitions.len() == 1,
        "Aegis installs exactly one replacement definition"
    );
    assert!(
        runner.state().objects[&aegis].replacement_definitions[0].is_consumed,
        "the matching Lava Axe damage consumes Aegis's one-shot replacement"
    );
}

/// CR 112.2 + CR 613.1b + CR 614.9: in a real three-player response stack,
/// Commandeer changes Lava Axe's live controller before Reverberation resolves;
/// the redirect therefore sends its damage to P2, not the Axe's caster P1.
#[test]
fn reverberation_redirects_to_a_stolen_spells_live_controller() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["P0 draw"]);
    scenario.with_library_top(P1, &["P1 draw"]);
    scenario.with_library_top(p2, &["P2 draw"]);
    let axe = scenario
        .add_spell_to_hand_from_oracle(P1, "Lava Axe", false, LAVA_AXE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let reverb = scenario
        .add_spell_to_hand_from_oracle(P0, "Reverberation", true, REVERBERATION)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let commandeer = scenario
        .add_spell_to_hand_from_oracle(p2, "Commandeer", true, COMMANDEER)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    for _ in 0..12 {
        if runner.state().phase == Phase::PreCombatMain {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("production priority passing advances P0's end step into P1's main phase");
    }
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    pass_priority_to(&mut runner, P1);

    // P1 casts Axe. Then P1 and P2 pass, giving P0 the legal response window.
    runner.cast(axe).target_player(P0).commit();
    pass_priority_to(&mut runner, P0);
    runner.cast(reverb).target_objects(&[axe]).commit();

    // P1 passes; P2 now has priority and casts the actual control-changing spell.
    pass_priority_to(&mut runner, p2);
    runner.cast(commandeer).target_objects(&[axe]).commit();
    resolve_one_stack_item(&mut runner);

    assert_eq!(
        runner.state().objects[&axe].controller,
        p2,
        "Commandeer must control Axe before it resolves"
    );
    resolve_one_stack_item(&mut runner); // Reverberation installs its shield.
    resolve_one_stack_item(&mut runner); // Lava Axe deals its redirected damage.
    assert_eq!(runner.life(P0), 20, "P0 is no longer Lava Axe's recipient");
    assert_eq!(
        runner.life(P1),
        20,
        "P1 cast Axe but does not control it at damage time"
    );
    assert_eq!(runner.life(p2), 15, "P2, Axe's live controller, takes five");
}

/// CR 115.7d + CR 601.2c + CR 614.9: Redirect changes Mirror Strike's real
/// declared source slot from A to B while it is on the stack. The resulting
/// continuous replacement must bind B only, proving the CDR role writeback is
/// positional rather than assuming the first target is the original source.
#[test]
fn redirect_retargets_mirror_strikes_declared_damage_source_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &["P0 draw"]);
    scenario.with_library_top(P1, &["P1 draw"]);
    let mirror = scenario
        .add_spell_to_hand_from_oracle(P0, "Mirror Strike", true, MIRROR_STRIKE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let redirect = scenario
        .add_spell_to_hand_from_oracle(P1, "Redirect", true, REDIRECT)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let attacker_a = scenario.add_creature(P1, "Source A", 3, 20).id();
    let attacker_b = scenario.add_creature(P1, "Source B", 4, 20).id();
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::PreCombatMain);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (attacker_a, AttackTarget::Player(P0)),
            (attacker_b, AttackTarget::Player(P0)),
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
        .expect("P1 passes to P0's Mirror Strike response");
    runner.cast(mirror).target_objects(&[attacker_a]).commit();
    // P0 receives priority after casting; passing gives P1 the legal response.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes to P1's Redirect response");
    runner.cast(redirect).target_objects(&[mirror]).commit();

    for _ in 0..12 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving Redirect");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Redirect's retarget option");
            }
            WaitingFor::RetargetChoice { .. } => break,
            ref other => panic!("unexpected Redirect wait state: {other:?}"),
        }
    }
    let WaitingFor::RetargetChoice {
        legal_new_targets, ..
    } = &runner.state().waiting_for
    else {
        panic!("Redirect must present the real retarget choice");
    };
    assert!(legal_new_targets.contains(&TargetRef::Object(attacker_b)));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Object(attacker_b)],
        })
        .expect("Redirect writes the alternate source into Mirror Strike's slot");
    runner.advance_until_stack_empty();

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);
    runner.combat_damage();
    assert_eq!(
        runner.life(P0),
        p0_before - 3,
        "source A remains unredirected"
    );
    assert_eq!(
        runner.life(P1),
        p1_before - 4,
        "only retargeted source B redirects"
    );
}
