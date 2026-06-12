//! CR 702.5a + CR 303.4j: Aura Graft host-restriction tests.
//!
//! Aura Graft ("Gain control of target Aura attached to a permanent. Attach it to
//! another permanent it can enchant.") must constrain the host slot to permanents
//! the moved Aura can legally enchant — defined by the Aura's own `Keyword::Enchant`
//! filter (CR 702.5a). Both the offer side (legal-target enumeration) and the
//! resolve side (CR 303.4j "the Aura doesn't move") enforce this.
//!
//! Tests use direct game-state synthesis (`GameState::new_two_player`,
//! `create_object`) and drive the real targeting pipeline
//! (`build_target_slots` -> `begin_target_selection_for_ability` ->
//! `choose_target_for_ability`) plus the real `attach::resolve` resolver.

use engine::game::ability_utils::{
    begin_target_selection_for_ability, build_target_slots, choose_target_for_ability,
    TargetSelectionAdvance,
};
use engine::game::effects::attach;
use engine::game::game_object::AttachTarget;
use engine::game::zones::create_object;
use engine::types::ability::{
    Effect, ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn setup() -> GameState {
    GameState::new_two_player(42)
}

/// An Aura on the battlefield, optionally carrying an `Enchant(creature)` keyword,
/// already attached to some host.
fn make_aura(state: &mut GameState, controller: PlayerId, enchant_creature: bool) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        "Test Aura".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    if enchant_creature {
        // CR 702.5a: "Enchant creature".
        obj.keywords.push(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )));
    }
    id
}

fn make_creature(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        "Bear".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    id
}

fn make_noncreature_permanent(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        "Signet".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Artifact);
    id
}

/// Build the Aura Graft chain: `GainControl{ target: Aura }` with sub-ability
/// `Attach{ attachment: ParentTarget, target: Permanent }` (the host slot).
fn build_aura_graft(source: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let sub = ResolvedAbility::new(
        Effect::Attach {
            attachment: TargetFilter::ParentTarget,
            target: TargetFilter::Typed(TypedFilter::permanent()),
        },
        vec![],
        source,
        controller,
    );
    let mut outer = ResolvedAbility::new(
        Effect::GainControl {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Aura".to_string()))),
        },
        vec![],
        source,
        controller,
    );
    outer.sub_ability = Some(Box::new(sub));
    outer
}

/// CR 702.5a + CR 303.4j: DISCRIMINATING — with an `Enchant creature` Aura, the
/// host slot must offer ONLY the creature (not the non-creature permanent), and
/// resolving an Attach to the non-creature must leave the Aura where it was.
/// Fails on pre-fix code (every battlefield permanent was offered, and the
/// resolver moved the Aura to an illegal host).
#[test]
fn aura_graft_restricts_host_to_enchantable_and_blocks_illegal_move_cr_702_5a() {
    let mut state = setup();
    // Aura controlled by P1, currently enchanting an existing creature host.
    let aura = make_aura(&mut state, P1, /* enchant_creature */ true);
    let original_host = make_creature(&mut state, P1);
    attach::attach_to(&mut state, aura, original_host);

    // A fresh creature (legal host) and a non-creature permanent (illegal host).
    let legal_creature = make_creature(&mut state, P0);
    let illegal_artifact = make_noncreature_permanent(&mut state, P0);

    // P0 casts Aura Graft (source is some object P0 controls).
    let source = make_noncreature_permanent(&mut state, P0);
    let ability = build_aura_graft(source, P0);

    let target_slots = build_target_slots(&state, &ability).expect("slots");
    assert_eq!(
        target_slots.len(),
        2,
        "expected GainControl(Aura) + Attach(host) slots, got {target_slots:?}",
    );

    // Slot 0 (the Aura) offers the only Aura on the battlefield.
    let progress = begin_target_selection_for_ability(&state, &ability, &target_slots, &[])
        .expect("begin selection");
    assert!(
        progress
            .current_legal_targets
            .contains(&TargetRef::Object(aura)),
        "Aura slot must offer the Aura: {progress:?}",
    );

    // Submit the Aura into slot 0; advance to the host slot.
    let advance = choose_target_for_ability(
        &state,
        &ability,
        &target_slots,
        &[],
        &progress,
        Some(TargetRef::Object(aura)),
    )
    .expect("choose aura");
    let host_progress = match advance {
        TargetSelectionAdvance::InProgress(p) => p,
        TargetSelectionAdvance::Complete(_) => panic!("expected host slot still pending"),
    };

    // CR 702.5a: the host slot must offer ONLY the creature (the Aura enchants
    // creatures), excluding the non-creature artifact and the original host's
    // controller's other permanents.
    assert!(
        host_progress
            .current_legal_targets
            .contains(&TargetRef::Object(legal_creature)),
        "host slot must offer the enchantable creature: {host_progress:?}",
    );
    assert!(
        !host_progress
            .current_legal_targets
            .contains(&TargetRef::Object(illegal_artifact)),
        "host slot must NOT offer a non-creature the Aura can't enchant: {host_progress:?}",
    );

    // CR 303.4j: even if an effect tries to attach the Aura to the illegal host,
    // the Aura doesn't move. Drive the resolver directly with the illegal host.
    let mut resolve_ability = ResolvedAbility::new(
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target: TargetFilter::Typed(TypedFilter::permanent()),
        },
        vec![TargetRef::Object(illegal_artifact)],
        aura,
        P0,
    );
    resolve_ability.targets = vec![TargetRef::Object(illegal_artifact)];
    let mut events: Vec<GameEvent> = Vec::new();
    attach::resolve(&mut state, &resolve_ability, &mut events).expect("resolve");
    assert_eq!(
        state.objects.get(&aura).unwrap().attached_to,
        Some(AttachTarget::Object(original_host)),
        "CR 303.4j: Aura must NOT move to a host it can't enchant; it stays put",
    );
}

/// CR 702.5a regression fence (NOT a fix validator): when the Aura has NO Enchant
/// keyword (e.g. its abilities were stripped by RemoveAllAbilities), there is no
/// restriction — ANY battlefield permanent is a legal host, and the resolver
/// attaches it. Passes both before and after the fix; guards against the
/// restriction over-firing on a no-Enchant Aura.
#[test]
fn aura_graft_no_enchant_keyword_offers_any_host_cr_702_5a() {
    let mut state = setup();
    let aura = make_aura(&mut state, P1, /* enchant_creature */ false);
    let original_host = make_creature(&mut state, P1);
    attach::attach_to(&mut state, aura, original_host);

    let creature = make_creature(&mut state, P0);
    let artifact = make_noncreature_permanent(&mut state, P0);

    let source = make_noncreature_permanent(&mut state, P0);
    let ability = build_aura_graft(source, P0);

    let target_slots = build_target_slots(&state, &ability).expect("slots");
    let progress = begin_target_selection_for_ability(&state, &ability, &target_slots, &[])
        .expect("begin selection");
    let advance = choose_target_for_ability(
        &state,
        &ability,
        &target_slots,
        &[],
        &progress,
        Some(TargetRef::Object(aura)),
    )
    .expect("choose aura");
    let host_progress = match advance {
        TargetSelectionAdvance::InProgress(p) => p,
        TargetSelectionAdvance::Complete(_) => panic!("expected host slot still pending"),
    };

    // No Enchant keyword => no restriction => both permanents are offered.
    assert!(
        host_progress
            .current_legal_targets
            .contains(&TargetRef::Object(creature)),
        "no-Enchant Aura: creature host must be offered: {host_progress:?}",
    );
    assert!(
        host_progress
            .current_legal_targets
            .contains(&TargetRef::Object(artifact)),
        "no-Enchant Aura: any permanent (incl. artifact) must be offered: {host_progress:?}",
    );

    // The resolver attaches to either host (here the artifact) — no CR 303.4j block.
    let resolve_ability = ResolvedAbility::new(
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target: TargetFilter::Typed(TypedFilter::permanent()),
        },
        vec![TargetRef::Object(artifact)],
        aura,
        P0,
    );
    let mut events: Vec<GameEvent> = Vec::new();
    attach::resolve(&mut state, &resolve_ability, &mut events).expect("resolve");
    assert_eq!(
        state.objects.get(&aura).unwrap().attached_to,
        Some(AttachTarget::Object(artifact)),
        "no-Enchant Aura: resolver attaches to any permanent host",
    );
}
