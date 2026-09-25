//! CR 702.5a + CR 303.4j: Aura Graft host-restriction tests.
//!
//! Aura Graft ("Gain control of target Aura that's attached to a permanent.
//! Attach it to another permanent it can enchant.") must constrain the host
//! slot to permanents the moved Aura can legally enchant — defined by the
//! Aura's own `Keyword::Enchant` filter (CR 702.5a). Both the offer side
//! (legal-target enumeration) and the resolve side (CR 303.4j "the Aura doesn't
//! move") enforce this.
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
use engine::game::scenario::GameScenario;
use engine::game::zones::create_object;
use engine::types::ability::{
    Effect, EffectKind, ResolvedAbility, TargetChoiceTiming, TargetFilter, TargetRef, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
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
            selection: engine::types::ability::AttachSelection::Targeted,
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
            selection: engine::types::ability::AttachSelection::Targeted,
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
            selection: engine::types::ability::AttachSelection::Targeted,
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

// ===========================================================================
// A1 / A2 — the resolution-time DESCRIBED host choice (U5)
// ===========================================================================

/// Verbatim Oracle text (Scryfall / the local export).
const AURA_GRAFT: &str = "Gain control of target Aura that's attached to a permanent. Attach it to another permanent it can enchant.";

const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";

/// CR 115.1a + CR 608.2d + CR 702.5a + CR 701.3b: FULL CAST PIPELINE for Aura
/// Graft. The printed host ("another permanent it can enchant") is DESCRIBED —
/// no literal "target" — so the choice arrives while the spell resolves as an
/// `EffectZoneChoice` (CR 115.1a + CR 608.2d), and the offer is restricted to
/// hosts the Aura can legally enchant (CR 702.5a) OTHER than the permanent it is
/// currently on (CR 608.2d + CR 701.3b: an attach to the object the attachment
/// is already on does nothing). Selecting one moves the Aura there.
#[test]
fn aura_graft_full_pipeline_offers_only_enchantable_hosts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Pacifism", PACIFISM)
        .with_subtypes(vec!["Aura"])
        .id();
    // The Aura's current host, plus two more legal hosts (two are required so
    // the choice still PARKS once the current host is excluded), and a
    // non-creature permanent that "Enchant creature" cannot enchant.
    let original_host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let other_host = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let third_host = scenario.add_creature(P1, "Bear Cub", 1, 1).id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Signet", "{T}: Add {C}.")
        .id();
    let graft = scenario
        .add_spell_to_hand_from_oracle(P0, "Aura Graft", false, AURA_GRAFT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    {
        // CR 702.5a: the Aura's own Enchant filter is the restriction authority.
        // Pushed onto BOTH the live and the base keyword lists so the layer
        // engine re-derives it after a flush (a live-only push is wiped by the
        // cast pipeline's layer pass).
        let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&aura)
            .expect("the Aura exists");
        obj.keywords.push(enchant.clone());
        obj.base_keywords.push(enchant);
    }
    attach::attach_to(runner.state_mut(), aura, original_host);
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(original_host)),
        "precondition: the Aura is attached to a permanent (Aura Graft's target requirement)"
    );

    let outcome = runner.cast(graft).target_object(aura).resolve();
    let cards = match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice {
            cards, effect_kind, ..
        } => {
            assert_eq!(
                *effect_kind,
                EffectKind::Attach,
                "the parked choice is the Attach host, not another zone choice"
            );
            cards.clone()
        }
        other => panic!("expected the parked attach host choice, got {other:?}"),
    };
    assert_eq!(
        cards.len(),
        2,
        "exactly the two enchantable creatures OTHER than the current host: {cards:?}"
    );
    // CR 608.2d + CR 701.3b: "another permanent" is HOST-relative. The Aura's
    // current host is not a legal destination (attaching to it would do
    // nothing), so it must never be offered.
    assert!(
        !cards.contains(&original_host),
        "the Aura's current host must not be offered: {cards:?}"
    );
    assert!(
        cards.contains(&other_host) && cards.contains(&third_host),
        "every other enchantable creature must be offered: {cards:?}"
    );
    assert!(
        !cards.contains(&artifact),
        "a non-creature permanent must not be offered (CR 702.5a)"
    );
    assert!(
        !cards.contains(&aura),
        "the Aura cannot enchant itself and must not be offered"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![other_host],
        })
        .expect("the parked host choice must accept the selection");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(other_host)),
        "selecting the other creature moves the Aura there"
    );
    assert!(
        runner.state().objects[&other_host]
            .attachments
            .contains(&aura),
        "the chosen host must list the Aura among its attachments"
    );
    assert!(
        !runner.state().objects[&original_host]
            .attachments
            .contains(&aura),
        "the previous host must drop the Aura"
    );
}

/// CR 608.2d + CR 609.3 + CR 701.3b (Aura Graft ruling): when the
/// Aura's current host is the ONLY permanent it can enchant, the host-relative
/// "another permanent" leaves no legal destination, so no host choice is offered
/// at all — the attach instruction does nothing and the Aura stays where it is,
/// while the control gain of the same spell still resolves. Scryfall
/// (2004-10-04): if there is no legal place to move the enchantment, it doesn't
/// move but you still control it.
///
/// DISCRIMINATION NOTE: the STATE axis is not revert-failing — before the
/// exclusion the engine auto-bound the single candidate (the current host) and
/// re-delivered an attach to the object the Aura was already on, which
/// CR 701.3b makes a no-op, so the end state is identical. The EVENT axis is:
/// `deliver_attach` emits `EffectResolved { kind: Attach }` even for that
/// same-host no-op, and the assertion below requires its absence. A1 carries
/// the state-axis discrimination (the current host is present in the parked
/// `cards` without the exclusion).
#[test]
fn aura_graft_with_only_its_current_host_is_a_quiet_noop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Pacifism", PACIFISM)
        .with_subtypes(vec!["Aura"])
        .id();
    let only_host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    // A non-enchantable permanent keeps the board from being trivially empty:
    // the host population is non-empty, it simply has no LEGAL other
    // destination.
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Signet", "{T}: Add {C}.")
        .id();
    let graft = scenario
        .add_spell_to_hand_from_oracle(P0, "Aura Graft", false, AURA_GRAFT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    {
        let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&aura)
            .expect("the Aura exists");
        obj.keywords.push(enchant.clone());
        obj.base_keywords.push(enchant);
    }
    attach::attach_to(runner.state_mut(), aura, only_host);

    let outcome = runner.cast(graft).target_object(aura).resolve();

    // No host choice is offered: the only candidate is the current host.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no host choice may be offered when the current host is the only \
         enchantable permanent, got {:?}",
        outcome.final_waiting_for()
    );
    // Reach-guard: the Aura is still attached to its original host.
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(only_host)),
        "the Aura must not move when there is no legal destination"
    );
    assert!(
        runner.state().objects[&only_host]
            .attachments
            .contains(&aura),
        "the original host must still list the Aura"
    );
    // Only the attach instruction does nothing (CR 701.3b); the control gain of
    // the same spell still resolves.
    assert_eq!(
        runner.state().objects[&aura].controller,
        P0,
        "Aura Graft still gains control of the Aura"
    );
    // Reach-guard: the non-enchantable permanent is still on the battlefield,
    // so the no-op is not explained by an emptied board.
    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Battlefield,
        "the non-enchantable permanent must still be on the battlefield"
    );
    // DISCRIMINATING: `deliver_attach` pushes `EffectResolved { kind: Attach }`
    // even for a same-host no-op (CR 701.3b), so the ABSENCE of that event is
    // what separates "no legal destination" (this row) from "re-attached to the
    // current host" (the pre-exclusion behavior). A state-only assertion cannot
    // see that difference — the end state is identical.
    assert!(
        !outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Attach,
                ..
            }
        )),
        "the attach instruction must not resolve at all, events: {:?}",
        outcome.events()
    );
}

/// CR 608.2d + CR 609.3 (recorded decision): a Resolution-timed Attach whose
/// context-ref attachment cascade resolves to nothing is a silent no-op — the
/// resolver returns `Ok`, the waiting state is unchanged and nothing is
/// attached. Before the handler change the same call surfaced
/// `Err(MissingParam("No attachment for Attach"))`.
#[test]
fn described_host_attach_with_no_attachment_is_a_silent_noop() {
    let mut state = setup();
    let host = make_creature(&mut state, P0);
    let source = make_noncreature_permanent(&mut state, P0);
    let before = state.waiting_for.clone();

    let mut ability = ResolvedAbility::new(
        Effect::Attach {
            attachment: TargetFilter::ParentTarget,
            target: TargetFilter::Typed(TypedFilter::permanent()),
            selection: engine::types::ability::AttachSelection::Targeted,
        },
        // No declared targets: the `ParentTarget` attachment cascade has nothing
        // to read (no trigger event, no bound attachment target).
        vec![],
        source,
        P0,
    );
    ability.target_choice_timing = TargetChoiceTiming::Resolution;

    let mut events: Vec<GameEvent> = Vec::new();
    attach::resolve(&mut state, &ability, &mut events)
        .expect("a no-candidate described attach is a no-op, not an error");
    assert_eq!(
        state.waiting_for, before,
        "the no-op must not open a prompt"
    );
    assert_eq!(
        state.objects[&host].attached_to, None,
        "nothing may be attached to the host"
    );
    assert_eq!(
        state.objects[&source].attached_to, None,
        "the source must not attach to itself"
    );
}
