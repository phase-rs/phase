//! Issue #2898: Najeela, the Blade-Blossom — the {W}{U}{B}{R}{G} activated
//! ability is parsed as a chained effect:
//!
//!   SetTapState(Untap, All, attacking creatures)
//!     -> GenericEffect (grant trample/lifelink/haste, affected: ParentTarget, UntilEndOfTurn)
//!       -> AdditionalPhase(BeginCombat, after EndCombat)
//!
//! The reported bug: only the additional combat phase resolves — the untap and
//! the keyword grant chained ahead of `AdditionalPhase` are dropped. This is a
//! chained-effect completeness defect: when the head `SetTapState(All)` resolves
//! it must publish the affected creatures as the chain's tracked set so the
//! `affected: ParentTarget` keyword grant binds to them. This test drives the
//! exact parsed chain through `resolve_ability_chain` and asserts ALL THREE
//! sub-effects apply.

use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, EffectScope, ResolvedAbility, StaticDefinition,
    TapStateChange, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::ExtraPhase;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::types::ability::FilterProp;

/// Build the Najeela activated-ability chain: untap-all-attacking → grant
/// trample/lifelink/haste to those creatures → additional combat phase.
fn najeela_chain(source: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let attacking_creatures = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: None,
        properties: vec![FilterProp::Attacking { defender: None }],
    });

    // Link 3: additional combat phase (the only effect that currently works).
    let additional_phase = ResolvedAbility::new(
        Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::BeginCombat,
            after: Phase::EndCombat,
            followed_by: vec![],
            count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
            attacker_restriction: None,
        },
        vec![],
        source,
        controller,
    );

    // Link 2: grant trample/lifelink/haste to the parent-affected creatures.
    let mut keyword_grant = ResolvedAbility::new(
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::ParentTarget),
                modifications: vec![
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Trample,
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Lifelink,
                    },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    },
                ],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: vec![],
                characteristic_defining: false,
                description: None,
                attack_defended: None,
                source_controller: None,
                source_object: None,
                bypass_beneficiary: None,
            }],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
        },
        vec![],
        source,
        controller,
    );
    keyword_grant.sub_ability = Some(Box::new(additional_phase));

    // Link 1 (head): untap all attacking creatures.
    let mut untap_all = ResolvedAbility::new(
        Effect::SetTapState {
            target: attacking_creatures,
            scope: EffectScope::All,
            state: TapStateChange::Untap,
        },
        vec![],
        source,
        controller,
    );
    untap_all.sub_ability = Some(Box::new(keyword_grant));
    untap_all
}

/// Two attacking creatures (tapped from attacking) controlled by the active
/// player; Najeela is a third permanent that is the ability source.
fn setup() -> (
    engine::types::game_state::GameState,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut state = engine::types::game_state::GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.phase = Phase::DeclareAttackers;

    let make_attacker = |state: &mut engine::types::game_state::GameState, card: u64| {
        let id = create_object(
            state,
            CardId(card),
            PlayerId(0),
            format!("Warrior {card}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        // Attacking creatures are tapped (CR 508.1f) — the ability untaps them.
        obj.tapped = true;
        id
    };
    let attacker_a = make_attacker(&mut state, 1);
    let attacker_b = make_attacker(&mut state, 2);

    let najeela = create_object(
        &mut state,
        CardId(99),
        PlayerId(0),
        "Najeela, the Blade-Blossom".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&najeela)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    state.combat = Some(CombatState {
        attackers: vec![
            AttackerInfo {
                object_id: attacker_a,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
                band_id: None,
            },
            AttackerInfo {
                object_id: attacker_b,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
                band_id: None,
            },
        ],
        ..Default::default()
    });

    (state, attacker_a, attacker_b, najeela)
}

/// CR 500.8 + CR 613 + CR 701.26b: Activating Najeela's ability must apply ALL
/// THREE chained sub-effects, not just the additional combat phase.
#[test]
fn najeela_applies_untap_grant_and_extra_combat() {
    let (mut state, attacker_a, attacker_b, najeela) = setup();
    let ability = najeela_chain(najeela, PlayerId(0));
    let mut events = Vec::new();

    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    engine::game::layers::flush_layers(&mut state);

    // (a) CR 500.8: an additional combat phase is scheduled after EndCombat.
    assert!(
        state.extra_phases.contains(&ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
            attacker_restriction: None,
            attacker_restriction_source: None,
        }),
        "extra combat phase must be scheduled; got {:?}",
        state.extra_phases
    );

    // (b) CR 701.26b: all attacking creatures are untapped.
    assert!(
        !state.objects[&attacker_a].tapped,
        "attacker A must be untapped"
    );
    assert!(
        !state.objects[&attacker_b].tapped,
        "attacker B must be untapped"
    );

    // (c) CR 613: trample, lifelink, and haste are granted to the attacking
    // creatures (the parent-affected set), not to Najeela.
    for keyword in [Keyword::Trample, Keyword::Lifelink, Keyword::Haste] {
        assert!(
            state.objects[&attacker_a].has_keyword(&keyword),
            "attacker A must gain {keyword:?}"
        );
        assert!(
            state.objects[&attacker_b].has_keyword(&keyword),
            "attacker B must gain {keyword:?}"
        );
    }
}
