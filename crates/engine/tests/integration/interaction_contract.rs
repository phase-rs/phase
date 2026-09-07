use std::sync::Arc;

use engine::analysis::decision_template::{
    DecisionPoint, DecisionPointKind, DecisionSlot, IterationCount, ShortcutDecisionSchema,
};
use engine::game::engine::apply;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, preview_interaction,
    preview_interaction_with_rejection, resolve_interaction_response, submit_interaction,
};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, Chooser, ChosenAttribute,
    CounterCostSelection, Effect, ManaContribution, ManaProduction, QuantityExpr, ResolvedAbility,
    SacrificeCost, TargetFilter, TargetRef, TypedFilter, ZoneChoiceCandidateSource, ZoneOwner,
};
use engine::types::actions::{GameAction, MulliganChoice, ResolutionOptionalPaymentChoice};
use engine::types::card::CardFace;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AlternativeCastKeyword, AutoPassMode, CastPaymentMode, GameState, MulliganBottomEntry,
    MulliganDecisionEntry, MulliganDecisionPhase, OpeningHandBottomReason, PendingTriggerSummary,
    PlayerDeckPool, ResolutionOptionalPaymentOption, TurnBoundary, WaitingFor,
    ZoneOpponentChooserPurpose,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    AmountAssignment, InteractionActionCode, InteractionAvailability, InteractionChoiceId,
    InteractionManaAbilityActivationScope, InteractionManaColor, InteractionManaRestriction,
    InteractionOpportunityResponse, InteractionOutcomeCode, InteractionPresentationSurface,
    InteractionPreviewRequest, InteractionPreviewStatus, InteractionReasonCode,
    InteractionResponse, InteractionResponseSpec, InteractionRoleCode, InteractionSessionId,
    InteractionShortcutCountSpec, InteractionShortcutDecision, InteractionShortcutPin,
    InteractionShortcutPoint, InteractionShortcutPointKind, InteractionShortcutPreview,
    InteractionShortcutPreviewEntry, InteractionShortcutPreviewFamily,
    InteractionShortcutResponseCode, InteractionSubmission, PreviewRequestId,
    MAX_INTERACTION_LIST_LEN, MAX_SHORTCUT_PREVIEW_ELEMENTS,
};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::match_config::MatchPhase;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn priority_view(state: &GameState) -> engine::types::interaction::ViewerInteraction {
    viewer_interaction(state, P0)
}

fn viewer_interaction(
    state: &GameState,
    viewer: PlayerId,
) -> engine::types::interaction::ViewerInteraction {
    let filtered = filter_state_for_viewer(state, viewer);
    derive_viewer_interaction(state, &filtered, viewer)
}

fn bind(state: &mut GameState, id: &str) {
    bind_interaction_authority(state, InteractionSessionId(id.to_string()))
        .expect("valid interaction authority binding");
}

fn assert_select_schema_materializes_only_select(
    state: &GameState,
    view: &engine::types::interaction::ViewerInteraction,
    request_prefix: &str,
) {
    assert_eq!(view.opportunities.len(), 1);
    let opportunity = &view.opportunities[0];
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Select { .. },
        candidates,
    } = &opportunity.response
    else {
        panic!("bottom-card opportunities use the Select response schema");
    };
    let choice_id = candidates
        .first()
        .expect("a one-card bottom prompt exposes its card candidate")
        .id
        .clone();
    let select_preview = preview_interaction(
        state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId(format!("{request_prefix}-select")),
            interaction_id: opportunity.interaction_id.clone(),
            response: InteractionResponse::Select {
                choice_ids: vec![choice_id.clone()],
            },
        },
    );
    assert_eq!(select_preview.status, InteractionPreviewStatus::Confirmable);

    let choose_preview = preview_interaction(
        state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId(format!("{request_prefix}-choose")),
            interaction_id: opportunity.interaction_id.clone(),
            response: InteractionResponse::Choose { choice_id },
        },
    );
    assert_eq!(
        choose_preview.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::MalformedResponse,
        }
    );
}

fn progress_witness(
    state: &GameState,
    viewer: engine::types::player::PlayerId,
) -> InteractionSubmission {
    let filtered = filter_state_for_viewer(state, viewer);
    let view = derive_viewer_interaction(state, &filtered, viewer);
    let InteractionAvailability::ProgressAvailable { witness } = view.availability else {
        panic!(
            "expected a complete progress witness, got {:?}",
            view.availability
        );
    };
    witness
}

fn schema_choice_id_for_object(
    view: &engine::types::interaction::ViewerInteraction,
    object_id: engine::types::identifiers::ObjectId,
) -> InteractionChoiceId {
    view.opportunities
        .iter()
        .find_map(|opportunity| {
            let engine::types::interaction::InteractionOpportunityResponse::Schema {
                candidates,
                ..
            } = &opportunity.response
            else {
                return None;
            };
            candidates
                .iter()
                .find(|choice| {
                    choice.surfaces.iter().any(|surface| {
                        matches!(
                            surface,
                            InteractionPresentationSurface::Object { reference, .. }
                                if reference == &object_id.0.to_string()
                        )
                    })
                })
                .map(|choice| choice.id.clone())
        })
        .expect("the schema contains the requested object")
}

fn gain_life_effect(source: engine::types::identifiers::ObjectId) -> Box<ResolvedAbility> {
    Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    ))
}

#[test]
fn priority_cast_exposes_auto_and_manual_and_opaque_manual_submission_starts_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_creature_to_hand(P0, "Interaction Manual Cast", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Green,
            engine::types::identifiers::ObjectId(9_900),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    bind(runner.state_mut(), "manual-priority-cast");

    let view = priority_view(runner.state());
    let InteractionOpportunityResponse::ExactChoices { choices } = &view.opportunities[0].response
    else {
        panic!("priority responses are exact choices");
    };
    let cast_choice_for_mode = |mode: &str| {
        choices.iter().find(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Action {
                        code: InteractionActionCode::CastSpell,
                        ..
                    }
                )
            }) && choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Object { reference, .. }
                        if reference == &spell.0.to_string()
                )
            }) && choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Value {
                        role: InteractionRoleCode::PaymentMode,
                        value,
                        ..
                    } if value == mode
                )
            })
        })
    };
    assert!(cast_choice_for_mode("auto").is_some());
    let manual_choice = cast_choice_for_mode("manual")
        .expect("the human priority projection includes a separately validated manual sibling");

    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Choose {
                choice_id: manual_choice.id.clone(),
            },
        },
    )
    .expect("the opaque manual cast choice submits through the interaction authority");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
}

#[test]
fn bottom_card_opportunities_use_and_only_materialize_select_responses() {
    let mut opening_scenario = GameScenario::new();
    opening_scenario.add_land_to_hand(P0, "Opening Bottom Class");
    let mut opening = opening_scenario.build();
    opening.state_mut().waiting_for = WaitingFor::OpeningHandBottomCards {
        pending: vec![MulliganBottomEntry {
            player: P0,
            count: 1,
        }],
        reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
    };
    bind(opening.state_mut(), "response-class-opening-bottom");
    let opening_view = priority_view(opening.state());
    assert_select_schema_materializes_only_select(opening.state(), &opening_view, "opening-bottom");

    let mut mulligan_scenario = GameScenario::new();
    mulligan_scenario.add_land_to_hand(P0, "Mulligan Bottom Class");
    let mut mulligan = mulligan_scenario.build();
    mulligan.state_mut().waiting_for = WaitingFor::MulliganDecision {
        pending: vec![
            MulliganDecisionEntry {
                player: P0,
                mulligan_count: 1,
                phase: MulliganDecisionPhase::BottomCards {
                    count: 1,
                    then: engine::types::game_state::PendingMulliganAction::Keep,
                },
            },
            MulliganDecisionEntry {
                player: P1,
                mulligan_count: 0,
                phase: MulliganDecisionPhase::Declare,
            },
        ],
        free_first_mulligan: false,
    };
    bind(mulligan.state_mut(), "response-class-mulligan-bottom");
    let mulligan_view = priority_view(mulligan.state());
    assert_select_schema_materializes_only_select(
        mulligan.state(),
        &mulligan_view,
        "mulligan-bottom",
    );
}

#[test]
fn resolving_a_response_materializes_the_advertised_action_under_the_same_authorization() {
    let mut state = GameState::new_two_player(42);
    bind(&mut state, "resolve-seam");
    let witness = progress_witness(&state, P0);

    // Authorization parity with `submit_interaction` is the entire risk of a
    // non-mutating sibling: without the actor check it would become a way to
    // materialize — and therefore to read — a decision belonging to another
    // seat. Nothing here asserts that the state is unchanged, because
    // `resolve_interaction_response` takes `&GameState`: non-mutation is a
    // borrow-checker guarantee, and a test of it would pass for reasons that
    // have nothing to do with this function.
    let unauthorized = resolve_interaction_response(&state, P1, &witness)
        .expect_err("resolving authorizes against the actor, not merely the interaction id");
    assert_eq!(unauthorized.code, InteractionReasonCode::NotAuthorized);

    let action = resolve_interaction_response(&state, P0, &witness)
        .expect("the advertised progress witness resolves to the action it denotes");
    assert_eq!(action, GameAction::PassPriority);

    // The same witness really is submittable, so the resolution above concerns a
    // live decision rather than one the engine would have refused anyway.
    // Equivalence between the two paths needs no assertion: `submit_interaction`
    // delegates here, so they cannot disagree.
    let applied = submit_interaction(&mut state, P0, witness)
        .expect("the witness the projection advertised is submittable");
    assert_eq!(
        applied.action,
        GameAction::PassPriority,
        "the post-success transaction exposes the exact engine-materialized action for replay"
    );
}

#[test]
fn priority_projection_previews_submits_and_rejects_stale_or_unauthorized_ids() {
    let mut state = GameState::new_two_player(42);
    bind(&mut state, "priority");
    let view = priority_view(&state);
    assert!(view.can_submit);
    assert_eq!(view.authorized_submitters, vec![P0.0]);
    assert_eq!(view.opportunities.len(), 1);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let witness = match view.availability {
        InteractionAvailability::ProgressAvailable { witness } => witness,
        other => panic!("priority must expose a real progress witness, got {other:?}"),
    };
    assert_eq!(witness.interaction_id, interaction_id);
    let response = witness.response;

    let unauthorized = submit_interaction(
        &mut state,
        P1,
        InteractionSubmission {
            interaction_id: interaction_id.clone(),
            response: response.clone(),
        },
    )
    .expect_err("a non-authorized actor cannot spend another seat's capability");
    assert_eq!(unauthorized.code, InteractionReasonCode::NotAuthorized);

    let preview = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("preview-1".to_string()),
            interaction_id: interaction_id.clone(),
            response: response.clone(),
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    assert!(matches!(
        preview.outcome,
        InteractionOutcomeCode::Advanced | InteractionOutcomeCode::Replaced
    ));

    submit_interaction(
        &mut state,
        P0,
        InteractionSubmission {
            interaction_id: interaction_id.clone(),
            response: response.clone(),
        },
    )
    .expect("the projected progress witness must cross the normal reducer boundary");
    assert!(state
        .active_interaction_slots
        .iter()
        .all(|slot| slot.interaction_id != interaction_id));

    let stale = submit_interaction(
        &mut state,
        P0,
        InteractionSubmission {
            interaction_id,
            response,
        },
    )
    .expect_err("an accepted submission consumes its opaque capability");
    assert_eq!(stale.code, InteractionReasonCode::StaleInteraction);
}

#[test]
fn attachment_fans_are_per_interaction_filtered_and_direct() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Fan Host", 2, 2).id();
    let attachment = scenario.add_creature(P0, "Fan Attachment", 1, 1).id();
    let unrelated = scenario.add_creature(P0, "Fan Unrelated", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        engine::game::effects::attach::attach_to(state, attachment, host);
        state.objects.get_mut(&attachment).unwrap().tapped = true;
        state.objects.get_mut(&unrelated).unwrap().tapped = true;
        state.waiting_for = WaitingFor::ChooseUntapSubset {
            player: P0,
            group: vec![attachment, unrelated],
            max: 1,
        };
        bind(state, "attachment-fan");
    }

    let view = viewer_interaction(runner.state(), P0);
    assert!(
        !view.opportunities.is_empty(),
        "reach guard: the selected attachment has a live opportunity"
    );
    assert_eq!(view.attachment_fans.len(), 1);
    let fan = view
        .attachment_fans
        .get(&host.0)
        .expect("the engine keys the fan by its visible host object");
    assert_eq!(fan.host_id, host.0);
    assert_eq!(fan.children.len(), 1);
    assert_eq!(fan.children[0].object_id, attachment.0);
    let submission = fan.children[0].submission.clone();
    submit_interaction(runner.state_mut(), P0, submission).expect(
        "the engine-authored fan submission resolves through production interaction dispatch",
    );
    assert!(
        !runner.state().objects[&attachment].tapped,
        "the published attachment submission applies its selected untap"
    );

    let mut mismatched_filtered = filter_state_for_viewer(runner.state(), P0);
    mismatched_filtered
        .objects
        .get_mut(&host)
        .expect("fixture host remains visible")
        .attachments
        .clear();
    let mismatched = derive_viewer_interaction(runner.state(), &mismatched_filtered, P0);
    assert!(
        mismatched.attachment_fans.is_empty(),
        "a stale host back-link must not expose an attachment fan from authoritative state"
    );

    let unauthorized = viewer_interaction(runner.state(), P1);
    assert!(
        unauthorized.attachment_fans.is_empty(),
        "non-authorized viewers receive no attachment sidecar before any opportunity derivation"
    );
}

/// Attaches `attachment` to `host` and asserts the engine wrote both directions
/// of the relationship, so a later membership assertion cannot pass on a
/// half-built fixture.
fn attach_and_assert_linked(state: &mut GameState, attachment: ObjectId, host: ObjectId) {
    engine::game::effects::attach::attach_to(state, attachment, host);
    assert!(
        state.objects[&host].attachments.contains(&attachment),
        "fixture guard: the host must list its attachment"
    );
    assert_eq!(
        state.objects[&attachment].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(host)),
        "fixture guard: the attachment must point back at its host"
    );
}

/// A host wearing two attachments, one of them itself a host, with exactly one
/// published pick in the whole subtree.
///
/// This is the shape that made membership and affordance look like one question:
/// the engine publishes a fan per DIRECT host and only for a child with exactly
/// one legal choice, so a consumer that read the fans as the membership list
/// dropped the two cards nothing was published for — off the only surface that
/// shows what is attached at all.
#[test]
fn attachment_views_publish_the_whole_subtree_whatever_is_pickable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "View Host", 2, 2).id();
    let inner = scenario.add_creature(P0, "View Inner", 1, 1).id();
    let nested = scenario.add_creature(P0, "View Nested", 1, 1).id();
    let sibling = scenario.add_creature(P0, "View Sibling", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        attach_and_assert_linked(state, inner, host);
        attach_and_assert_linked(state, nested, inner);
        attach_and_assert_linked(state, sibling, host);
        state.objects.get_mut(&nested).unwrap().tapped = true;
        // Only the nested card is a candidate, so only the INTERMEDIATE host
        // gets a fan — the outer host gets none at all.
        state.waiting_for = WaitingFor::ChooseUntapSubset {
            player: P0,
            group: vec![nested],
            max: 1,
        };
        bind(state, "attachment-view");
    }

    let view = priority_view(runner.state());
    assert_eq!(
        view.attachment_fans.keys().copied().collect::<Vec<_>>(),
        vec![inner.0],
        "reach guard: the engine publishes its pick under the direct host only"
    );

    let outer = view
        .attachment_views
        .get(&host.0)
        .expect("the outer host publishes its own membership");
    assert_eq!(
        outer
            .cards
            .iter()
            .map(|card| card.object_id)
            .collect::<Vec<_>>(),
        vec![inner.0, nested.0, sibling.0],
        "membership is the whole subtree in depth-first order, not the published picks"
    );
    assert!(
        outer.cards[0].submission.is_none() && outer.cards[2].submission.is_none(),
        "a card the engine published no pick for is still a member, without a submission"
    );
    let nested_submission = outer.cards[1]
        .submission
        .clone()
        .expect("a pick published under a nested host reaches the outer host's view");

    let intermediate = view
        .attachment_views
        .get(&inner.0)
        .expect("an attachment that is itself a host publishes its own membership");
    assert_eq!(
        intermediate
            .cards
            .iter()
            .map(|card| card.object_id)
            .collect::<Vec<_>>(),
        vec![nested.0],
        "a nested host lists what hangs on it, and never itself"
    );

    submit_interaction(runner.state_mut(), P0, nested_submission).expect(
        "the submission published in the view resolves through production interaction dispatch",
    );
    assert!(
        !runner.state().objects[&nested].tapped,
        "the nested card's published submission applies its selected untap"
    );
}

/// Membership answers a different question than the fan does, and must not
/// inherit its authorization gate: an attached permanent is an object in play
/// (CR 301.5 / CR 303.4), so it stays visible while another player holds the
/// turn. Both directions of the relationship still have to agree.
#[test]
fn attachment_views_follow_visibility_while_fans_follow_authorization() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Link Host", 2, 2).id();
    let attachment = scenario.add_creature(P0, "Link Attachment", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        attach_and_assert_linked(state, attachment, host);
        state.objects.get_mut(&attachment).unwrap().tapped = true;
        state.waiting_for = WaitingFor::ChooseUntapSubset {
            player: P0,
            group: vec![attachment],
            max: 1,
        };
        bind(state, "attachment-view-links");
    }

    let unauthorized = viewer_interaction(runner.state(), P1);
    assert!(
        unauthorized.attachment_fans.is_empty(),
        "reach guard: the pick sidecar stays authorization-scoped"
    );
    let opponent_view = unauthorized
        .attachment_views
        .get(&host.0)
        .expect("the opponent still sees what is attached to a battlefield permanent");
    assert_eq!(
        opponent_view
            .cards
            .iter()
            .map(|card| card.object_id)
            .collect::<Vec<_>>(),
        vec![attachment.0]
    );
    assert!(
        opponent_view.cards[0].submission.is_none(),
        "a viewer who may not submit is offered nothing to submit"
    );

    let mut stale_back_link = filter_state_for_viewer(runner.state(), P0);
    stale_back_link
        .objects
        .get_mut(&host)
        .expect("fixture host remains visible")
        .attachments
        .clear();
    assert!(
        derive_viewer_interaction(runner.state(), &stale_back_link, P0)
            .attachment_views
            .is_empty(),
        "a host that no longer lists the attachment publishes no membership for it"
    );

    let mut stale_forward_link = filter_state_for_viewer(runner.state(), P0);
    stale_forward_link
        .objects
        .get_mut(&attachment)
        .expect("fixture attachment remains visible")
        .attached_to = None;
    assert!(
        derive_viewer_interaction(runner.state(), &stale_forward_link, P0)
            .attachment_views
            .is_empty(),
        "an attachment that no longer points back at its host publishes no membership"
    );
}

/// The projection crosses the generated adapter as the client reads it: camel
/// case on the wire, a `null` submission for a card with no published pick.
#[test]
fn attachment_views_survive_the_adapter_round_trip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Wire Host", 2, 2).id();
    let attachment = scenario.add_creature(P0, "Wire Attachment", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        attach_and_assert_linked(state, attachment, host);
        bind(state, "attachment-view-wire");
    }

    let view = priority_view(runner.state());
    assert!(view.attachment_views.contains_key(&host.0));
    let wire = serde_json::to_string(&view).expect("serialize the viewer projection");
    assert!(
        wire.contains("\"attachmentViews\"") && wire.contains("\"objectId\""),
        "the adapter reads camel case: {wire}"
    );
    assert!(
        wire.contains("\"submission\":null"),
        "a member with no published pick crosses the wire as null: {wire}"
    );
    let decoded: engine::types::interaction::ViewerInteraction =
        serde_json::from_str(&wire).expect("deserialize the viewer projection");
    assert_eq!(decoded.attachment_views, view.attachment_views);

    // A projection written before this field existed still loads.
    let mut legacy: serde_json::Value = serde_json::from_str(&wire).expect("reparse as value");
    legacy
        .as_object_mut()
        .expect("the projection is an object")
        .remove("attachmentViews");
    let legacy: engine::types::interaction::ViewerInteraction =
        serde_json::from_value(legacy).expect("a projection without the field still loads");
    assert!(legacy.attachment_views.is_empty());
}

/// Hangs one more copy of the engine-written attachment `seed` on `host`,
/// writing both directions exactly as `attach::attach_to` wrote them for the
/// seed itself. Cloning rather than re-attaching keeps a ten-thousand-row
/// fixture cheap without inventing a relationship shape of its own.
fn clone_attachment_onto(
    state: &mut GameState,
    seed: ObjectId,
    host: ObjectId,
    next_id: &mut u64,
) -> ObjectId {
    let id = ObjectId(*next_id);
    *next_id += 1;
    let mut copy = state.objects[&seed].clone();
    copy.id = id;
    copy.attachments.clear();
    copy.attached_to = Some(engine::game::game_object::AttachTarget::Object(host));
    state.objects.insert(id, copy);
    state.battlefield.push_back(id);
    state
        .objects
        .get_mut(&host)
        .expect("host exists")
        .attachments
        .push(id);
    id
}

fn next_object_id(state: &GameState) -> u64 {
    state.objects.keys().map(|id| id.0).max().unwrap_or(0) + 1
}

/// Membership is derived before the authorization, session and slot gates, so
/// an early return carries as much of it as the derived path does. Only the
/// whole-projection bound charges the map and every card in it: each view below
/// is inside the per-view cap, and it is their SUM that has to fail closed —
/// otherwise a viewer who may not submit anything at all still receives an
/// attachment tree of unbounded size.
///
/// The same early return ships its membership normally while it fits — that is
/// `attachment_views_follow_visibility_while_fans_follow_authorization`.
#[test]
fn an_early_return_fails_closed_on_the_aggregate_attachment_budget() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Budget Host", 2, 2).id();
    let seed = scenario.add_creature(P0, "Budget Attachment", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        attach_and_assert_linked(state, seed, host);
        // Fill the host to the per-view maximum, so no single view is oversized
        // on its own and only the aggregate can object.
        let mut next_id = next_object_id(state);
        while state.objects[&host].attachments.len() < MAX_INTERACTION_LIST_LEN {
            clone_attachment_onto(state, seed, host, &mut next_id);
        }
        bind(state, "attachment-view-budget");
    }
    assert!(
        runner.state().objects[&host]
            .attachments
            .iter()
            .all(|id| runner.state().objects[id].attached_to
                == runner.state().objects[&seed].attached_to),
        "fixture guard: every filled row carries the same back-link the engine wrote"
    );

    let unauthorized = viewer_interaction(runner.state(), P1);
    assert_eq!(
        unauthorized.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        },
        "an early return that cannot be bounded says so instead of shipping the payload"
    );
    assert!(
        unauthorized.attachment_views.is_empty()
            && unauthorized.attachment_fans.is_empty()
            && unauthorized.opportunities.is_empty(),
        "failing the budget drops every unbounded list rather than truncating one"
    );
    assert!(
        !unauthorized.can_submit,
        "the fail-closed projection keeps the authority answer it was already carrying"
    );
}

/// A single direct host whose own subtree passes the per-view cap.
///
/// This shape used to be absorbed inside the membership derivation — the host
/// was skipped, and an over-limit host map was replaced by an empty one — which
/// handed the budget gate a small, plausible projection it had no reason to
/// reject. The viewer then read a bounded empty map as an authoritative
/// "nothing is attached", which is the one answer the engine must never invent.
///
/// Read through the unauthorized early return, which is the cheap seat: the
/// derived path would enumerate every legal action over ten thousand
/// permanents, and `a_deep_attachment_chain_fails_closed_on_the_aggregate`
/// already carries the same claim through it.
#[test]
fn an_oversized_attachment_tree_fails_closed_instead_of_publishing_an_empty_map() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Wide Host", 2, 2).id();
    let seed = scenario.add_creature(P0, "Wide Attachment", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        attach_and_assert_linked(state, seed, host);
        let mut next_id = next_object_id(state);
        while state.objects[&host].attachments.len() <= MAX_INTERACTION_LIST_LEN {
            clone_attachment_onto(state, seed, host, &mut next_id);
        }
        bind(state, "attachment-view-wide");
    }
    let wide = viewer_interaction(runner.state(), P1);
    assert_eq!(
        wide.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        },
        "a host whose own subtree is over the cap fails closed; it is not skipped"
    );
    assert!(wide.attachment_views.is_empty() && wide.opportunities.is_empty());
}

/// The nesting half of the same claim, read through the DERIVED path: every
/// view is small, and it is the tree that is too large.
///
/// A chain is the shape where the derivation's own cost is quadratic — every
/// ancestor carries the whole tail beneath it — so the row is also the one that
/// says the budget is charged while membership is derived rather than after it.
/// At this depth the finished payload would hold 499 500 cards; the derivation
/// stops one card past the aggregate instead.
///
/// The depth is capped by what the derived path costs elsewhere, not by what the
/// walk costs: enumerating every legal action over the chain is quadratic too,
/// and measured on this fixture it runs 1 s at 1 000 links, 14 s at 3 000 and
/// 109 s at 10 000. `a_cap_depth_attachment_chain_is_refused_before_it_is_built`
/// carries the depth claim past that ceiling through the cheap seat.
#[test]
fn a_deep_attachment_chain_fails_closed_on_the_aggregate() {
    const CHAIN: usize = 1_000;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let root = scenario.add_creature(P0, "Chain Root", 2, 2).id();
    let seed = scenario.add_creature(P0, "Chain Link", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        attach_and_assert_linked(state, seed, root);
        let mut next_id = next_object_id(state);
        let mut tip = seed;
        for _ in 1..CHAIN {
            tip = clone_attachment_onto(state, seed, tip, &mut next_id);
        }
        bind(state, "attachment-view-deep");
    }
    let deep = priority_view(runner.state());
    assert_eq!(
        deep.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        },
        "nesting sums the same way: {CHAIN} views of at most {CHAIN} cards each \
         exceed the aggregate even though none exceeds the per-view cap"
    );
    assert!(deep.attachment_views.is_empty());
}

/// The depth half, at the worst depth there is: a chain exactly
/// `MAX_INTERACTION_LIST_LEN` links long is the LONGEST one in which no single
/// view exceeds the per-view cap, so it is precisely the shape a per-host check
/// cannot catch. One link shorter and the aggregate is smaller; one longer and
/// the outermost view trips the per-view cap on its own and the derivation stops
/// at the first host. This is the depth the engine permits, too — CR 732.2's
/// runaway-cascade guard (`MAX_OBJECT_GROWTH`, `game::engine`) lets one dispatch
/// grow the board by 16 000 objects.
///
/// The finished payload here holds 49 995 000 cards, the prefix sums of every
/// nested view. Measured on this fixture, deriving membership in full and
/// measuring afterwards costs 23.2 s and peaks at 7.4 GB resident; charging the
/// aggregate as the walk goes costs 0.16 s and 4.7 GB, which is the fixture
/// itself. The engine ships to WASM, where that difference is linear-memory
/// exhaustion rather than a slow frame.
///
/// The answer is the same either way — the finalizer always refused this payload
/// — so the row asserts the answer and the cost above is what the change is for.
/// Read through the unauthorized early return, which reaches the same membership
/// derivation without the action enumeration the derived path owes over ten
/// thousand permanents (109 s, measured); the row above carries the same
/// fail-closed answer through the derived path at an affordable depth.
#[test]
fn a_cap_depth_attachment_chain_is_refused_before_it_is_built() {
    const CHAIN: usize = MAX_INTERACTION_LIST_LEN;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let root = scenario.add_creature(P0, "Cap Depth Root", 2, 2).id();
    let seed = scenario.add_creature(P0, "Cap Depth Link", 1, 1).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        attach_and_assert_linked(state, seed, root);
        let mut next_id = next_object_id(state);
        let mut tip = seed;
        for _ in 1..CHAIN {
            tip = clone_attachment_onto(state, seed, tip, &mut next_id);
        }
        bind(state, "attachment-view-cap-depth");
    }
    let deep = viewer_interaction(runner.state(), P1);
    assert_eq!(
        deep.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        },
        "a chain {CHAIN} links deep is refused, not walked to the bottom"
    );
    assert!(deep.attachment_views.is_empty() && deep.opportunities.is_empty());
}

#[test]
fn authority_requires_explicit_binding_and_rebinding_invalidates_old_capabilities() {
    let mut state = GameState::new_two_player(42);
    let unbound = priority_view(&state);
    assert_eq!(
        unbound.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::AuthorityUnbound,
        }
    );
    assert!(unbound.opportunities.is_empty());

    bind(&mut state, "first-session");
    let old_id = priority_view(&state).opportunities[0]
        .interaction_id
        .clone();
    bind(&mut state, "first-session");
    let same_session_id = priority_view(&state).opportunities[0]
        .interaction_id
        .clone();
    assert_ne!(same_session_id, old_id);
    let stale_same_session = submit_interaction(
        &mut state,
        P0,
        InteractionSubmission {
            interaction_id: old_id.clone(),
            response: InteractionResponse::Choose {
                choice_id: InteractionChoiceId("irrelevant".to_string()),
            },
        },
    )
    .expect_err("rebinding the same session must still retire its prior capability");
    assert_eq!(
        stale_same_session.code,
        InteractionReasonCode::StaleInteraction
    );

    bind(&mut state, "replacement-session");
    let replacement = priority_view(&state);
    assert_ne!(replacement.opportunities[0].interaction_id, same_session_id);
    let stale = submit_interaction(
        &mut state,
        P0,
        InteractionSubmission {
            interaction_id: old_id,
            response: InteractionResponse::Choose {
                choice_id: InteractionChoiceId("irrelevant".to_string()),
            },
        },
    )
    .expect_err("rebinding invalidates every capability from the prior session");
    assert_eq!(stale.code, InteractionReasonCode::StaleInteraction);
}

#[test]
fn malformed_same_session_serial_is_rejected_without_resurrecting_an_old_id() {
    let mut base = GameState::new_two_player(42);
    bind(&mut base, "restored-session");
    let session = base
        .interaction_session_id
        .clone()
        .expect("the base state is bound");
    let old_id = base.active_interaction_slots[0].interaction_id.clone();

    for malformed in ["", "0", "000", "not-decimal"] {
        let mut persisted = base.clone();
        persisted.next_interaction_serial = malformed.to_string();
        let serialized = serde_json::to_string(&persisted).expect("serialize malformed authority");
        let mut restored: GameState =
            serde_json::from_str(&serialized).expect("restore malformed authority");

        assert_eq!(
            priority_view(&restored).availability,
            InteractionAvailability::Unsupported {
                reason: InteractionReasonCode::InvalidAuthorityState,
            }
        );
        let direct_rejection = submit_interaction(
            &mut restored,
            P0,
            InteractionSubmission {
                interaction_id: old_id.clone(),
                response: InteractionResponse::Choose {
                    choice_id: InteractionChoiceId("old-choice".to_string()),
                },
            },
        )
        .expect_err("malformed restored authority rejects an old ID before rebinding");
        assert_eq!(
            direct_rejection.code,
            InteractionReasonCode::InvalidAuthorityState
        );

        let error = bind_interaction_authority(&mut restored, session.clone())
            .expect_err("the same session cannot normalize a malformed serial");
        assert_eq!(error.code, InteractionReasonCode::InvalidAuthorityState);
        assert_eq!(restored.next_interaction_serial, malformed);
        assert!(restored.active_interaction_slots.is_empty());
        assert_eq!(
            priority_view(&restored).availability,
            InteractionAvailability::Unsupported {
                reason: InteractionReasonCode::InvalidAuthorityState,
            }
        );

        let rejected = submit_interaction(
            &mut restored,
            P0,
            InteractionSubmission {
                interaction_id: old_id.clone(),
                response: InteractionResponse::Choose {
                    choice_id: InteractionChoiceId("old-choice".to_string()),
                },
            },
        )
        .expect_err("the persisted old capability cannot be resurrected");
        assert_eq!(rejected.code, InteractionReasonCode::InvalidAuthorityState);
        assert!(!restored
            .active_interaction_slots
            .iter()
            .any(|slot| slot.interaction_id.as_str().ends_with(".1")));
    }
}

#[test]
fn legacy_unbound_state_still_accepts_normal_actions_without_minting_authority() {
    let mut state = GameState::new_two_player(42);
    let initial_revision = state.state_revision;
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    apply(&mut state, P0, GameAction::PassPriority)
        .expect("legacy unbound states continue through the normal reducer");
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert!(state.state_revision > initial_revision);
    assert!(state.interaction_session_id.is_none());
    assert!(state.active_interaction_slots.is_empty());
}

#[test]
fn exact_priority_choices_distinguish_two_engine_authored_card_objects() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_land_to_hand(P0, "Exact Surface Plains").id();
    let second = scenario.add_land_to_hand(P0, "Exact Surface Island").id();
    let mut runner = scenario.build();
    bind(runner.state_mut(), "exact-card-surfaces");

    let view = priority_view(runner.state());
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices { choices } =
        &view.opportunities[0].response
    else {
        panic!("priority is projected as exact choices");
    };
    let references: std::collections::HashSet<_> = choices
        .iter()
        .filter(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Action {
                        code: InteractionActionCode::PlayLand,
                        ..
                    }
                )
            })
        })
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Object {
                role: InteractionRoleCode::Source,
                reference,
                ..
            } => Some(reference.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        references,
        [first.0.to_string(), second.0.to_string()]
            .into_iter()
            .collect()
    );
}

#[test]
fn reordering_hand_rotates_indexed_choices_before_the_new_projection_is_usable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_land_to_hand(P0, "Reorder Contract Plains")
        .id();
    let second = scenario
        .add_land_to_hand(P0, "Reorder Contract Island")
        .id();
    let mut runner = scenario.build();
    bind(runner.state_mut(), "reorder-card-surfaces");

    let old_view = priority_view(runner.state());
    let old_interaction_id = old_view.opportunities[0].interaction_id.clone();
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices {
        choices: old_choices,
    } = &old_view.opportunities[0].response
    else {
        panic!("priority is projected as exact choices");
    };
    let old_first_choice = old_choices
        .iter()
        .find(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Object {
                        role: InteractionRoleCode::Source,
                        reference,
                        ..
                    } if reference == &first.0.to_string()
                )
            })
        })
        .expect("the first land has an exact projected choice")
        .id
        .clone();

    runner
        .act(GameAction::ReorderHand {
            order: vec![second, first],
        })
        .expect("a permutation of the hand is accepted");
    let new_interaction_id = runner.state().active_interaction_slots[0]
        .interaction_id
        .clone();
    assert_ne!(new_interaction_id, old_interaction_id);

    let stale = submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id: old_interaction_id,
            response: InteractionResponse::Choose {
                choice_id: old_first_choice,
            },
        },
    )
    .expect_err("a choice indexed before hand reordering must be stale");
    assert_eq!(stale.code, InteractionReasonCode::StaleInteraction);

    let new_view = priority_view(runner.state());
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices {
        choices: new_choices,
    } = &new_view.opportunities[0].response
    else {
        panic!("priority remains projected as exact choices");
    };
    let new_first_choice = new_choices
        .iter()
        .find(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Object {
                        role: InteractionRoleCode::Source,
                        reference,
                        ..
                    } if reference == &first.0.to_string()
                )
            })
        })
        .expect("the new projection still maps the intended land")
        .id
        .clone();
    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id: new_interaction_id,
            response: InteractionResponse::Choose {
                choice_id: new_first_choice,
            },
        },
    )
    .expect("the new projection submits the intended land action");
    assert!(runner.state().battlefield.contains(&first));
    assert!(!runner.state().battlefield.contains(&second));
}

#[test]
fn exact_casting_variant_choices_include_index_variant_and_mana_cost() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Blue,
            ManaType::Black,
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|mana_type| ManaUnit::new(mana_type, spell, false, Vec::new()))
        .collect(),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the real split card produces its casting-variant prompt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastingVariantChoice { .. }
    ));
    bind(runner.state_mut(), "cast-variant-surfaces");

    let view = priority_view(runner.state());
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices { choices } =
        &view.opportunities[0].response
    else {
        panic!("casting variants are exact choices");
    };
    assert_eq!(choices.len(), 2);
    assert!(choices
        .iter()
        .all(|choice| choice.surfaces.iter().any(|surface| matches!(
            surface,
            InteractionPresentationSurface::Mana {
                role: InteractionRoleCode::CastingCost,
                ..
            }
        ))));
    let indices: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Value {
                role: InteractionRoleCode::OptionIndex,
                value,
                ..
            } => Some(value.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        indices,
        ["0".to_string(), "1".to_string()].into_iter().collect()
    );
    let variants: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Value {
                role: InteractionRoleCode::CastingVariant,
                value,
                ..
            } => Some(value.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(variants, ["Normal", "Fuse"].into_iter().collect());
    let costs: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Mana {
                role: InteractionRoleCode::CastingCost,
                symbols,
                ..
            } => Some(symbols.clone()),
            _ => None,
        })
        .collect();
    assert!(costs.contains(&vec!["U".to_string(), "B".to_string()]));
    assert!(costs.contains(&vec![
        "4".to_string(),
        "U".to_string(),
        "B".to_string(),
        "B".to_string(),
        "R".to_string(),
    ]));
}

#[test]
fn alternative_cast_siblings_use_stable_typed_codes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Alternative Cast Contract", false)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner.state_mut().waiting_for = WaitingFor::AlternativeCastChoice {
        player: P0,
        object_id: spell,
        card_id,
        payment_mode: CastPaymentMode::Auto,
        keyword: AlternativeCastKeyword::Warp,
        normal_cost: ManaCost::NoCost,
        alternative_cost: Some(ManaCost::NoCost),
        alternative_additional_cost: None,
        alternative_additional_cost_description: None,
    };
    bind(runner.state_mut(), "alternative-cast-codes");

    let view = priority_view(runner.state());
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices { choices } =
        &view.opportunities[0].response
    else {
        panic!("alternative cast responses are exact choices");
    };
    let codes: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Value {
                role: InteractionRoleCode::CastCost,
                value,
                ..
            } => Some(value.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(codes, ["alternative", "normal"].into_iter().collect());
}

#[test]
fn modal_schema_includes_mode_indices_and_engine_descriptions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Exact Modal Spell",
            false,
            "Choose one —\n• You gain 1 life.\n• You gain 2 life.",
        )
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("the real modal spell reaches its mode prompt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
    bind(runner.state_mut(), "mode-surfaces");

    let view = priority_view(runner.state());
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Sequence {
            min, max, escape, ..
        },
        candidates: choices,
    } = &view.opportunities[0].response
    else {
        panic!("modal responses use a sequence schema");
    };
    assert_eq!((*min, *max), (1, 1));
    assert_eq!(choices.len(), 3, "two semantic modes plus one escape");
    let escape = escape
        .as_ref()
        .expect("an in-progress cast exposes its cancel escape separately");
    let escape_choice = choices
        .iter()
        .find(|choice| &choice.id == escape)
        .expect("the schema escape references a projected choice");
    assert!(escape_choice.surfaces.iter().any(|surface| matches!(
        surface,
        InteractionPresentationSurface::Action {
            code: InteractionActionCode::CancelCast,
            ..
        }
    )));
    let descriptions: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Value {
                role: InteractionRoleCode::Mode,
                value,
                ..
            } => Some(value.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(descriptions.len(), 2);
    let semantic_choices: Vec<_> = choices
        .iter()
        .filter(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Value {
                        role: InteractionRoleCode::ModeIndex,
                        ..
                    }
                )
            })
        })
        .collect();
    assert_eq!(semantic_choices.len(), 2);
}

#[test]
fn exact_player_and_number_schema_siblings_are_self_describing() {
    let mut player_scenario = GameScenario::new_n_player(3, 42);
    let battle = player_scenario
        .add_creature(P0, "Protector Surface", 1, 1)
        .id();
    let mut player_runner = player_scenario.build();
    player_runner.state_mut().waiting_for = WaitingFor::BattleProtectorChoice {
        player: P0,
        battle_id: battle,
        candidates: vec![P1, PlayerId(2)],
    };
    bind(player_runner.state_mut(), "player-surfaces");
    let player_view = priority_view(player_runner.state());
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices {
        choices: player_choices,
    } = &player_view.opportunities[0].response
    else {
        panic!("protector choices are exact choices");
    };
    let seats: std::collections::HashSet<_> = player_choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Player {
                role: InteractionRoleCode::Protector,
                seat,
                ..
            } => Some(*seat),
            _ => None,
        })
        .collect();
    assert_eq!(seats, [P1.0, 2].into_iter().collect());

    let mut amount_scenario = GameScenario::new();
    amount_scenario.at_phase(Phase::PreCombatMain);
    let source = amount_scenario
        .add_creature_from_oracle(
            P0,
            "Amount Surface Source",
            0,
            1,
            "Pay X speed: Add X mana in any combination of colors.",
        )
        .id();
    let mut amount_runner = amount_scenario.build();
    amount_runner.state_mut().players[0].speed = Some(2);
    let ability_index = amount_runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("the parsed Pay X speed ability has a cost");
    amount_runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("the real activation reaches its amount prompt");
    assert!(matches!(
        amount_runner.state().waiting_for,
        WaitingFor::PayAmountChoice { min: 0, max: 2, .. }
    ));
    bind(amount_runner.state_mut(), "amount-surfaces");
    let amount_view = priority_view(amount_runner.state());
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Number { min, max, .. },
        candidates,
    } = &amount_view.opportunities[0].response
    else {
        panic!("amounts use a bounded number schema");
    };
    assert_eq!((*min, *max), (0, 2));
    assert!(candidates.is_empty());

    let preview = preview_interaction(
        amount_runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("number-above-one".to_string()),
            interaction_id: amount_view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Number { value: 2 },
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
}

#[test]
fn zone_opponent_chooser_exact_choices_surface_distinct_opponents_and_action_code() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let source = scenario
        .add_creature(P0, "Zone Opponent Chooser Source", 1, 1)
        .id();
    scenario.add_creature_to_exile(P0, "Zone Opponent Chooser Card", 1, 1);
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::ChooseFromZoneOpponentChooser {
        player: P0,
        candidates: vec![P1, PlayerId(2)],
        ability: Box::new(ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: vec![],
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Opponent.into(),
                candidate_source: ZoneChoiceCandidateSource::Legacy,
                reciprocal_role: None,
                up_to: false,
                selection: CardSelectionMode::Chosen,
                constraint: None,
            },
            vec![],
            source,
            P0,
        )),
        purpose: ZoneOpponentChooserPurpose::Ordinary,
    };
    bind(runner.state_mut(), "zone-opponent-chooser");

    let view = priority_view(runner.state());
    let InteractionOpportunityResponse::ExactChoices { choices } = &view.opportunities[0].response
    else {
        panic!("zone opponent chooser responses are exact choices");
    };
    assert_eq!(choices.len(), 2);
    assert!(choices.iter().all(|choice| {
        choice.surfaces.iter().any(|surface| {
            matches!(
                surface,
                InteractionPresentationSurface::Action {
                    code: InteractionActionCode::ChooseZoneOpponentChooser,
                    ..
                }
            )
        })
    }));
    let seats: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Player {
                role: InteractionRoleCode::Opponent,
                seat,
                ..
            } => Some(*seat),
            _ => None,
        })
        .collect();
    assert_eq!(seats, [P1.0, 2].into_iter().collect());
}

#[test]
fn mana_group_schema_exposes_engine_authored_symbols() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Any Color Surface", 0, 1)
        .as_artifact()
        .from_oracle_text("{T}: Add one mana of any color.")
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the real mana ability reaches its color prompt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));
    bind(runner.state_mut(), "mana-surfaces");
    let view = priority_view(runner.state());
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::ManaGroups { groups, .. },
        candidates: choices,
    } = &view.opportunities[0].response
    else {
        panic!("mana colors use a grouped mana schema");
    };
    assert_eq!(groups.len(), 1);
    let symbols: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Mana {
                role: InteractionRoleCode::ManaChoice,
                symbols,
                ..
            } => symbols.first().cloned(),
            _ => None,
        })
        .collect();
    assert_eq!(
        symbols,
        ["W", "U", "B", "R", "G"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
}

#[test]
fn tap_land_for_mana_projects_live_castle_output_per_unit_and_rejects_stale_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let castle = scenario
        .add_land_from_oracle(
            P0,
            "Castle Garenbrig",
            "{T}: Add {G}.\n{T}: Add {G}{G}{G}{G}{G}{G}. Spend this mana only to cast creature spells or activate abilities of creatures.",
        )
        .id();
    let mut runner = scenario.build();
    bind(runner.state_mut(), "live-castle-mana-output");

    let view = priority_view(runner.state());
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let InteractionOpportunityResponse::ExactChoices { choices } = &view.opportunities[0].response
    else {
        panic!("priority is projected as exact choices");
    };
    let castle_choices: Vec<_> = choices
        .iter()
        .filter(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Action {
                        code: InteractionActionCode::TapLandForMana,
                        ..
                    }
                )
            }) && choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Object {
                        role: InteractionRoleCode::Source,
                        reference,
                        ..
                    } if reference == &castle.0.to_string()
                )
            })
        })
        .collect();
    assert_eq!(
        castle_choices.len(),
        2,
        "Castle exposes both mana abilities"
    );

    let (one_green, six_green) = castle_choices
        .iter()
        .map(|choice| {
            let produced: Vec<_> = choice
                .surfaces
                .iter()
                .filter_map(|surface| match surface {
                    InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::ProducedMana,
                        symbols,
                        restrictions,
                        ..
                    } => Some((symbols, restrictions)),
                    _ => None,
                })
                .collect();
            (choice.id.clone(), produced)
        })
        .fold(
            (None, None),
            |(one, six), (choice_id, produced)| match produced.len() {
                1 => (Some(choice_id), six),
                6 => (one, Some((choice_id, produced))),
                count => panic!("unexpected Castle mana output count: {count}"),
            },
        );
    let one_green = one_green.expect("the unrestricted one-green ability is projected");
    let (six_green, six_output) = six_green.expect("the restricted six-green ability is projected");
    assert!(six_output.iter().all(|(symbols, restrictions)| {
        *symbols == &vec!["G".to_string()]
            && *restrictions
                == &vec![InteractionManaRestriction::OnlyForTypeSpellsOrAbilities {
                    spell_type: "Creature".to_string(),
                    ability: InteractionManaAbilityActivationScope::OfSpellType,
                }]
    }));

    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id: interaction_id.clone(),
            response: InteractionResponse::Choose {
                choice_id: one_green,
            },
        },
    )
    .expect("the sibling one-green option is a legal activation");
    let stale = submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id,
            response: InteractionResponse::Choose {
                choice_id: six_green,
            },
        },
    )
    .expect_err("the six-green choice is stale after its sibling tapped the land");
    assert_eq!(stale.code, InteractionReasonCode::StaleInteraction);
}

#[test]
fn tap_land_for_mana_projects_resolved_and_missing_chosen_color_restrictions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oracle = "As this land enters, choose a color.\n{T}: Add {C}. Spend this mana only to cast monocolored spells of the chosen color.";
    let red_source = scenario
        .add_land_from_oracle(P0, "Red Chosen Color Contract", oracle)
        .id();
    let blue_source = scenario
        .add_land_from_oracle(P0, "Blue Chosen Color Contract", oracle)
        .id();
    let missing_choice_source = scenario
        .add_land_from_oracle(P0, "Missing Chosen Color Contract", oracle)
        .id();
    let mut runner = scenario.build();
    for (source, color) in [(red_source, ManaColor::Red), (blue_source, ManaColor::Blue)] {
        runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("chosen-color source exists")
            .chosen_attributes
            .push(ChosenAttribute::Color(color));
    }

    let projected_restrictions = |state: &mut GameState, source: ObjectId, binding: &str| {
        bind(state, binding);
        let view = priority_view(state);
        let InteractionOpportunityResponse::ExactChoices { choices } =
            &view.opportunities[0].response
        else {
            panic!("priority is projected as exact choices");
        };
        choices
            .iter()
            .find(|choice| {
                choice.surfaces.iter().any(|surface| {
                    matches!(
                        surface,
                        InteractionPresentationSurface::Action {
                            code: InteractionActionCode::TapLandForMana,
                            ..
                        }
                    )
                }) && choice.surfaces.iter().any(|surface| {
                    matches!(
                        surface,
                        InteractionPresentationSurface::Object {
                            role: InteractionRoleCode::Source,
                            reference,
                            ..
                        } if reference == &source.0.to_string()
                    )
                })
            })
            .and_then(|choice| {
                choice.surfaces.iter().find_map(|surface| match surface {
                    InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::ProducedMana,
                        restrictions,
                        ..
                    } => Some(restrictions.clone()),
                    _ => None,
                })
            })
            .expect("the chosen-color mana source projects one produced mana unit")
    };

    assert_eq!(
        projected_restrictions(runner.state_mut(), red_source, "red-chosen-color-output"),
        vec![
            InteractionManaRestriction::OnlyForSpellWithColorCount {
                comparator: engine::types::interaction::InteractionManaComparator::Equal,
                count: 1,
            },
            InteractionManaRestriction::OnlyForSpellColor {
                color: InteractionManaColor::Red,
            },
        ],
        "the viewer contract preserves the red source's resolved restriction"
    );

    assert_eq!(
        projected_restrictions(runner.state_mut(), blue_source, "blue-chosen-color-output"),
        vec![
            InteractionManaRestriction::OnlyForSpellWithColorCount {
                comparator: engine::types::interaction::InteractionManaComparator::Equal,
                count: 1,
            },
            InteractionManaRestriction::OnlyForSpellColor {
                color: InteractionManaColor::Blue,
            },
        ],
        "each source projects its own chosen color rather than another source's choice"
    );

    assert_eq!(
        projected_restrictions(
            runner.state_mut(),
            missing_choice_source,
            "missing-chosen-color-output"
        ),
        vec![
            InteractionManaRestriction::OnlyForSpellWithColorCount {
                comparator: engine::types::interaction::InteractionManaComparator::Equal,
                count: 1,
            },
            InteractionManaRestriction::Impossible,
        ],
        "a missing choice remains visibly fail-closed instead of appearing unrestricted"
    );
}

#[test]
fn preference_and_failed_actions_preserve_capability_but_same_actor_progress_rotates_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_to_hand(P0, "Contract Test Plains").id();
    let mut runner = scenario.build();
    bind(runner.state_mut(), "preferences");
    let initial = runner.state().active_interaction_slots[0]
        .interaction_id
        .clone();

    runner
        .act(GameAction::SetPhaseStops { stops: Vec::new() })
        .expect("preference propagation remains legal for the priority holder");
    assert_eq!(
        runner.state().active_interaction_slots[0].interaction_id,
        initial
    );

    assert!(apply(runner.state_mut(), P1, GameAction::PassPriority).is_err());
    assert_eq!(
        runner.state().active_interaction_slots[0].interaction_id,
        initial
    );

    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing a legal land returns priority to the same actor");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_ne!(
        runner.state().active_interaction_slots[0].interaction_id,
        initial,
        "accepted A-to-A progress must still mint a new capability"
    );
}

#[test]
fn preference_action_does_not_advance_auto_pass_or_rotate_capability() {
    let mut state = GameState::new_two_player(42);
    bind(&mut state, "preference-auto-pass");
    let initial = state.active_interaction_slots[0].interaction_id.clone();
    state.auto_pass.insert(
        P0,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );

    apply(
        &mut state,
        P0,
        GameAction::SetPhaseStops { stops: Vec::new() },
    )
    .expect("the actor-scoped preference update is legal");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(state
        .active_interaction_slots
        .iter()
        .any(|slot| slot.interaction_id == initial));
}

#[test]
fn simultaneous_mulligan_preserves_only_the_other_owners_slot() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::MulliganDecision {
        pending: vec![
            MulliganDecisionEntry {
                player: P0,
                mulligan_count: 0,
                phase: MulliganDecisionPhase::Declare,
            },
            MulliganDecisionEntry {
                player: P1,
                mulligan_count: 0,
                phase: MulliganDecisionPhase::Declare,
            },
        ],
        free_first_mulligan: false,
    };
    bind(&mut state, "mulligan");
    let p0_id = state
        .active_interaction_slots
        .iter()
        .find(|slot| slot.semantic_owner == P0.0)
        .expect("P0 slot")
        .interaction_id
        .clone();
    let p1_id = state
        .active_interaction_slots
        .iter()
        .find(|slot| slot.semantic_owner == P1.0)
        .expect("P1 slot")
        .interaction_id
        .clone();

    apply(
        &mut state,
        P0,
        GameAction::MulliganDecision {
            choice: MulliganChoice::Keep,
        },
    )
    .expect("one simultaneous owner can keep independently");

    assert!(state
        .active_interaction_slots
        .iter()
        .all(|slot| slot.interaction_id != p0_id));
    assert_eq!(state.active_interaction_slots.len(), 1);
    assert_eq!(state.active_interaction_slots[0].semantic_owner, P1.0);
    assert_eq!(state.active_interaction_slots[0].interaction_id, p1_id);
}

#[test]
fn second_simultaneous_opening_bottom_owner_gets_its_own_validated_candidates() {
    let mut scenario = GameScenario::new();
    let p0_card = scenario.add_land_to_hand(P0, "P0 Opening Bottom").id();
    let p1_card = scenario.add_land_to_hand(P1, "P1 Opening Bottom").id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::OpeningHandBottomCards {
        pending: vec![
            MulliganBottomEntry {
                player: P0,
                count: 1,
            },
            MulliganBottomEntry {
                player: P1,
                count: 1,
            },
        ],
        reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
    };
    bind(runner.state_mut(), "opening-bottom");

    let filtered = filter_state_for_viewer(runner.state(), P1);
    let p1_view = derive_viewer_interaction(runner.state(), &filtered, P1);
    let opportunity = &p1_view.opportunities[0];
    let engine::types::interaction::InteractionOpportunityResponse::Schema {
        candidates: choices,
        ..
    } = &opportunity.response
    else {
        panic!("opening-bottom is a complete selection schema");
    };
    let visible_references: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Object { reference, .. } => Some(reference.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        visible_references,
        [p1_card.0.to_string()].into_iter().collect()
    );
    assert!(!visible_references.contains(&p0_card.0.to_string()));
    let p1_id = opportunity.interaction_id.clone();
    assert!(matches!(
        &p1_view.availability,
        InteractionAvailability::ProgressAvailable { witness }
            if witness.interaction_id == p1_id
                && matches!(&witness.response, InteractionResponse::Select { choice_ids } if choice_ids.len() == 1)
    ));
    let choice_id = schema_choice_id_for_object(&p1_view, p1_card);
    submit_interaction(
        runner.state_mut(),
        P1,
        InteractionSubmission {
            interaction_id: p1_id,
            response: InteractionResponse::Select {
                choice_ids: vec![choice_id],
            },
        },
    )
    .expect("the second simultaneous owner can submit its own bottom candidate");
    assert_eq!(
        runner.state().objects[&p1_card].zone,
        engine::types::zones::Zone::Library
    );
    assert_eq!(
        runner.state().objects[&p0_card].zone,
        engine::types::zones::Zone::Hand
    );
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::OpeningHandBottomCards { pending, .. }
            if pending.len() == 1 && pending[0].player == P0
    ));
}

#[test]
fn turn_controller_receives_and_can_submit_the_controlled_seats_witness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.turn_decision_controller = Some(P0);
        state.priority_passes.clear();
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
        bind(state, "turn-controller");
    }

    let InteractionSubmission {
        interaction_id,
        response,
    } = progress_witness(runner.state(), P0);
    let preview = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("controlled-seat-preview".to_string()),
            interaction_id: interaction_id.clone(),
            response: response.clone(),
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id,
            response,
        },
    )
    .expect("the turn controller submits for the controlled semantic seat");
}

#[test]
fn ordinary_semantic_owner_keeps_its_candidate_and_submission_authority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.turn_decision_controller = None;
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
        bind(state, "ordinary-seat");
    }

    let p0_view = derive_viewer_interaction(
        runner.state(),
        &filter_state_for_viewer(runner.state(), P0),
        P0,
    );
    assert_eq!(p0_view.availability, InteractionAvailability::Waiting);
    let submission = progress_witness(runner.state(), P1);
    submit_interaction(runner.state_mut(), P1, submission)
        .expect("the uncontrolled semantic owner submits its own validated candidate");
}

#[test]
fn sequential_ward_projection_submits_one_object_and_rotates_before_reprompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Ward Contract Source", 1, 1).id();
    let first = scenario.add_creature(P0, "Ward Contract First", 1, 1).id();
    let second = scenario.add_creature(P0, "Ward Contract Second", 1, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![first, second],
        pending_effect: gain_life_effect(source),
        remaining: 2,
        min_total_power: None,
    };
    bind(runner.state_mut(), "ward-sequential");

    let InteractionSubmission {
        interaction_id: first_id,
        response: first_response,
    } = progress_witness(runner.state(), P0);
    assert!(matches!(
        &first_response,
        InteractionResponse::Select { choice_ids } if choice_ids.len() == 1
    ));
    let preview = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("ward-preview".to_string()),
            interaction_id: first_id.clone(),
            response: first_response.clone(),
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id: first_id.clone(),
            response: first_response,
        },
    )
    .expect("the first one-object ward response is accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::WardSacrificeChoice { remaining: 1, .. }
    ));
    let InteractionSubmission {
        interaction_id: second_id,
        response: second_response,
    } = progress_witness(runner.state(), P0);
    assert_ne!(second_id, first_id);
    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id: second_id,
            response: second_response,
        },
    )
    .expect("the second prompt completes the sequential ward payment");
}

#[test]
fn aggregate_ward_projects_and_submits_a_multi_object_power_witness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Aggregate Ward Contract Source", 1, 1)
        .id();
    let first = scenario
        .add_creature(P0, "Aggregate Ward Contract First", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Aggregate Ward Contract Second", 1, 1)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![first, second],
        pending_effect: gain_life_effect(source),
        remaining: 1,
        min_total_power: Some(2),
    };
    bind(runner.state_mut(), "ward-aggregate");

    let submission = progress_witness(runner.state(), P0);
    assert!(matches!(
        &submission.response,
        InteractionResponse::Select { choice_ids } if choice_ids.len() == 2
    ));
    let preview = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("ward-aggregate-preview".to_string()),
            interaction_id: submission.interaction_id.clone(),
            response: submission.response.clone(),
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    submit_interaction(runner.state_mut(), P0, submission)
        .expect("two smaller permanents jointly satisfy aggregate Ward");
    assert_eq!(
        runner.state().objects[&first].zone,
        engine::types::zones::Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&second].zone,
        engine::types::zones::Zone::Graveyard
    );
}

#[test]
fn aggregate_ward_threshold_zero_still_rejects_an_empty_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Ward Zero Source", 1, 1).id();
    let zero = scenario.add_creature(P0, "Ward Zero Permanent", 0, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![zero],
        pending_effect: gain_life_effect(source),
        remaining: 1,
        min_total_power: Some(0),
    };
    bind(runner.state_mut(), "ward-zero");

    let view = priority_view(runner.state());
    assert_eq!(view.opportunities[0].progress.minimum, 1);
    assert!(!view.opportunities[0].progress.confirmable);
    let preview = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("ward-zero-empty".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Select {
                choice_ids: Vec::new(),
            },
        },
    );
    assert_eq!(
        preview.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ConstraintUnsatisfied,
        }
    );
}

#[test]
fn aggregate_ward_counts_negative_power_and_keeps_a_valid_positive_sibling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Signed Ward Source", 1, 1).id();
    let positive = scenario.add_creature(P0, "Signed Ward Positive", 2, 1).id();
    let negative = scenario
        .add_creature(P0, "Signed Ward Negative", -1, 1)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents: vec![positive, negative],
        pending_effect: gain_life_effect(source),
        remaining: 1,
        min_total_power: Some(2),
    };
    bind(runner.state_mut(), "ward-signed-power");

    let view = priority_view(runner.state());
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let positive_choice = schema_choice_id_for_object(&view, positive);
    let negative_choice = schema_choice_id_for_object(&view, negative);
    let invalid = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("ward-signed-invalid".to_string()),
            interaction_id: interaction_id.clone(),
            response: InteractionResponse::Select {
                choice_ids: vec![positive_choice.clone(), negative_choice],
            },
        },
    );
    assert_eq!(
        invalid.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ConstraintUnsatisfied,
        }
    );
    assert!(!invalid.progress.confirmable);

    let valid = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("ward-signed-valid".to_string()),
            interaction_id,
            response: InteractionResponse::Select {
                choice_ids: vec![positive_choice],
            },
        },
    );
    assert_eq!(valid.status, InteractionPreviewStatus::Confirmable);
    assert!(valid.progress.confirmable);
    assert_eq!(valid.progress.aggregate, Some(2));
}

#[test]
fn aggregate_ward_does_not_publish_a_witness_larger_than_the_contract_cap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Aggregate Ward Cap Source", 1, 1)
        .id();
    let permanent = scenario
        .add_creature(P0, "Aggregate Ward Cap Permanent", 1, 1)
        .id();
    // Repeated references exercise the contract-boundary list cap without
    // allocating 10,001 full game objects in this integration fixture.
    let permanents = vec![permanent; MAX_INTERACTION_LIST_LEN + 1];
    let threshold = i32::try_from(MAX_INTERACTION_LIST_LEN + 1)
        .expect("the interaction list cap fits in an aggregate power threshold");
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::WardSacrificeChoice {
        player: P0,
        permanents,
        pending_effect: gain_life_effect(source),
        remaining: 1,
        min_total_power: Some(threshold),
    };
    bind(runner.state_mut(), "ward-aggregate-cap");

    let view = priority_view(runner.state());
    assert_eq!(
        view.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        },
        "an oversized outbound schema fails closed before DTO projection"
    );
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices { choices } =
        &view.opportunities[0].response
    else {
        panic!("oversized opportunity uses the minimal fail-closed response");
    };
    assert!(choices.is_empty());
    assert!(!matches!(
        view.availability,
        InteractionAvailability::ProgressAvailable { .. }
    ));
}

#[test]
fn availability_uses_the_first_progressing_submission_not_the_first_slot() {
    let controller = PlayerId(2);
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    let p1_card = scenario.add_land_to_hand(P1, "Second Slot Bottom").id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.turn_decision_controller = Some(controller);
        state.waiting_for = WaitingFor::OpeningHandBottomCards {
            pending: vec![
                MulliganBottomEntry {
                    player: P0,
                    count: 1,
                },
                MulliganBottomEntry {
                    player: P1,
                    count: 1,
                },
            ],
            reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
        };
        bind(state, "multi-slot-progress");
    }

    let filtered = filter_state_for_viewer(runner.state(), controller);
    let view = derive_viewer_interaction(runner.state(), &filtered, controller);
    assert_eq!(view.opportunities.len(), 2);
    let InteractionAvailability::ProgressAvailable { witness } = view.availability else {
        panic!("the second controlled slot has a complete progress witness");
    };
    assert_eq!(witness.interaction_id, view.opportunities[1].interaction_id);
    let preview = preview_interaction(
        runner.state(),
        controller,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("multi-slot-preview".to_string()),
            interaction_id: witness.interaction_id.clone(),
            response: witness.response.clone(),
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    submit_interaction(runner.state_mut(), controller, witness)
        .expect("the non-first controlled slot witness submits unchanged");
    assert_eq!(
        runner.state().objects[&p1_card].zone,
        engine::types::zones::Zone::Library
    );
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::OpeningHandBottomCards { pending, .. }
            if pending.len() == 1 && pending[0].player == P0
    ));
}

#[test]
fn sequential_unless_bounce_projection_submits_one_object_before_reprompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Unless Bounce Contract Source", 1, 1)
        .id();
    let first = scenario
        .add_creature(P0, "Unless Bounce Contract First", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Unless Bounce Contract Second", 1, 1)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::UnlessBounceChoice {
        player: P0,
        permanents: vec![first, second],
        pending_effect: gain_life_effect(source),
        remaining: 2,
    };
    bind(runner.state_mut(), "bounce-sequential");

    let InteractionSubmission {
        interaction_id: first_id,
        response,
    } = progress_witness(runner.state(), P0);
    assert!(matches!(
        &response,
        InteractionResponse::Select { choice_ids } if choice_ids.len() == 1
    ));
    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id: first_id.clone(),
            response,
        },
    )
    .expect("the first one-object bounce response is accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessBounceChoice { remaining: 1, .. }
    ));
    assert_ne!(
        runner.state().active_interaction_slots[0].interaction_id,
        first_id
    );
}

#[test]
fn from_among_counter_cost_projects_and_submits_typed_amount_assignments() {
    let counter = CounterType::Generic("contract".to_string());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Counter Contract Source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::RemoveCounter {
                count: 2,
                counter_type: CounterMatch::OfType(counter.clone()),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
                selection: CounterCostSelection::AmongObjects,
            }),
        )
        .id();
    let first = scenario
        .add_creature(P0, "Counter Contract First", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Counter Contract Second", 1, 1)
        .id();
    scenario.with_counter(first, counter.clone(), 1);
    scenario.with_counter(second, counter.clone(), 2);
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the activated ability reaches its from-among payment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: engine::types::game_state::PayCostKind::RemoveCounter {
                selection: CounterCostSelection::AmongObjects,
                ..
            },
            ..
        }
    ));
    bind(runner.state_mut(), "counter-amounts");

    let InteractionSubmission {
        interaction_id,
        response,
    } = progress_witness(runner.state(), P0);
    let InteractionResponse::AssignAmounts { assignments } = &response else {
        panic!("from-among counter payment must use amount assignments");
    };
    assert_eq!(assignments.iter().map(|entry| entry.amount).sum::<u32>(), 2);
    let preview = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("counter-preview".to_string()),
            interaction_id: interaction_id.clone(),
            response: response.clone(),
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    submit_interaction(
        runner.state_mut(),
        P0,
        InteractionSubmission {
            interaction_id,
            response,
        },
    )
    .expect("typed per-object/per-counter assignments pay the real cost");
    let remaining = runner.state().objects[&first]
        .counters
        .get(&counter)
        .copied()
        .unwrap_or(0)
        + runner.state().objects[&second]
            .counters
            .get(&counter)
            .copied()
            .unwrap_or(0);
    assert_eq!(remaining, 1);
}

#[test]
fn persistence_roundtrip_retains_authority_while_viewer_filtering_redacts_it() {
    let mut state = GameState::new_two_player(42);
    bind(&mut state, "persisted");
    state.interaction_generation = 7;
    let session = state
        .interaction_session_id
        .clone()
        .expect("explicitly bound state has interaction authority");
    let serialized = serde_json::to_string(&state).expect("serialize authoritative state");
    let restored: GameState =
        serde_json::from_str(&serialized).expect("deserialize authoritative state");
    assert_eq!(restored.interaction_session_id, Some(session));
    assert_eq!(
        restored.interaction_generation,
        state.interaction_generation
    );
    assert_eq!(
        restored.next_interaction_serial,
        state.next_interaction_serial
    );
    assert_eq!(
        restored.active_interaction_slots,
        state.active_interaction_slots
    );

    let filtered = filter_state_for_viewer(&state, P0);
    assert!(filtered.interaction_session_id.is_none());
    assert_eq!(filtered.next_interaction_serial, "1");
    assert!(filtered.active_interaction_slots.is_empty());
    let filtered_json = serde_json::to_value(&filtered).expect("serialize viewer-filtered state");
    assert!(filtered_json.get("interaction_session_id").is_none());
    assert!(filtered_json.get("interaction_generation").is_none());
    assert!(filtered_json.get("next_interaction_serial").is_none());
    assert!(filtered_json.get("active_interaction_slots").is_none());

    let waiting_view = derive_viewer_interaction(&state, &filter_state_for_viewer(&state, P1), P1);
    assert!(!waiting_view.can_submit);
    assert!(waiting_view.opportunities.is_empty());
    assert_eq!(waiting_view.availability, InteractionAvailability::Waiting);
}

#[test]
fn preview_rejects_oversized_inputs_before_cloning_or_materializing() {
    let mut state = GameState::new_two_player(42);
    bind(&mut state, "oversized");
    let interaction_id = state.active_interaction_slots[0].interaction_id.clone();
    let preview = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("preview-large".to_string()),
            interaction_id: interaction_id.clone(),
            response: InteractionResponse::Select {
                choice_ids: vec![InteractionChoiceId("x".repeat(10_001))],
            },
        },
    );
    assert_eq!(
        preview.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::PayloadTooLarge
        }
    );
    assert_eq!(preview.outcome, InteractionOutcomeCode::Rejected);

    let nested = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("preview-large-nested".to_string()),
            interaction_id,
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::Decline,
                pins: (0..MAX_INTERACTION_LIST_LEN)
                    .map(|group| InteractionShortcutPin {
                        group: group as u32,
                        choice_ids: vec![InteractionChoiceId("x".to_string())],
                        amounts: Vec::new(),
                    })
                    .collect(),
            },
        },
    );
    assert_eq!(
        nested.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::PayloadTooLarge,
        }
    );
}

#[test]
fn response_wire_shape_is_tagged_and_camel_case() {
    let serialized = serde_json::to_value(InteractionResponse::Choose {
        choice_id: InteractionChoiceId("choice-1".to_string()),
    })
    .expect("serialize interaction response");
    assert_eq!(serialized["type"], "choose");
    assert_eq!(serialized["data"]["choiceId"], "choice-1");
    assert!(serialized["data"].get("choice_id").is_none());
}

#[test]
fn finite_shortcut_offer_distinguishes_propose_and_decline_without_capability_values() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::PrecastCopyShortcutOffer {
        proposer: P0,
        epoch: 73,
        route_count: 1,
    };
    bind(&mut state, "typed-shortcut");

    let view = priority_view(&state);
    let engine::types::interaction::InteractionOpportunityResponse::ExactChoices { choices } =
        &view.opportunities[0].response
    else {
        panic!("a finite shortcut offer is projected as exact choices");
    };
    let responses: std::collections::HashSet<_> = choices
        .iter()
        .flat_map(|choice| &choice.surfaces)
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::ShortcutResponse { response } => Some(*response),
            _ => None,
        })
        .collect();
    assert_eq!(
        responses,
        [
            InteractionShortcutResponseCode::Propose,
            InteractionShortcutResponseCode::Decline,
        ]
        .into_iter()
        .collect()
    );
    let serialized = serde_json::to_string(&choices).expect("serialize shortcut choices");
    assert!(!serialized.contains("73"));
    assert!(!serialized.contains("routeId"));
    assert!(!serialized.contains("breakpointId"));
    assert!(!serialized.contains("epoch"));
}

#[test]
fn trigger_sequence_materializes_arbitrary_permutations_larger_than_four() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::OrderTriggers {
        player: P0,
        triggers: (0..5)
            .map(|index| PendingTriggerSummary {
                source_id: engine::types::identifiers::ObjectId(index + 1),
                source_name: format!("Trigger source {index}"),
                description: format!("Trigger {index}"),
            })
            .collect(),
    };
    bind(&mut state, "trigger-permutation");

    let view = priority_view(&state);
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::Sequence {
                min,
                max,
                unique,
                include_all,
                ..
            },
        candidates,
    } = &view.opportunities[0].response
    else {
        panic!("trigger ordering uses a sequence schema");
    };
    assert_eq!((*min, *max, *unique, *include_all), (5, 5, true, true));
    let response = InteractionResponse::Sequence {
        choice_ids: [4, 1, 3, 0, 2]
            .map(|index| candidates[index].id.clone())
            .to_vec(),
    };
    let preview = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("trigger-permutation-preview".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response,
        },
    );
    assert_eq!(
        preview.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ReducerRejected,
        },
        "the arbitrary permutation must materialize; this synthetic state lacks only the reducer's pending ordering context"
    );

    let duplicate = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("trigger-duplicate-preview".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Sequence {
                choice_ids: vec![candidates[0].id.clone(); 5],
            },
        },
    );
    assert_eq!(
        duplicate.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ConstraintUnsatisfied,
        }
    );
}

/// NEW-1 — a published CR 732.2a offer carrying `max_iterations: 0` is REJECTED, not
/// clamped. `elimination_bounds` returns `0` to mean "no legal repetition exists and the
/// caller must not offer" (CR 704.5a), so repairing it to `1` would render a
/// one-iteration offer whose single iteration eliminates a player mid-proposal.
///
/// LATENT, NOT LIVE: no in-tree producer can emit `0` here — `build_shortcut_schema`'s two
/// call sites both pass `MAX_SHORTCUT_CYCLES`, the per-viewer projection copies an existing
/// value, and both `Default` and the `#[serde(default)]` resolve to the cap. Hand-assigning
/// `max_iterations: 0` IS the loaded/persisted-authority seat, which is exactly the shape a
/// restored dump can carry. This row is therefore a latent-hole guard, not a live-bug
/// reproduction.
///
/// REVERT-PROBE, and note the FAILURE MODE: delete
/// `if schema.max_iterations == 0 { return Err(..) }` ⇒ post-edit `max` is
/// `0u32.min(1000) == 0`, so `suggested.clamp(1, 0)` trips `Ord::clamp`'s
/// `assert!(min <= max)` and **PANICS** (`min > max. min = 1, max = 0`). That assert is a
/// PLAIN assert, so it survives release — the guard is load-bearing against an engine
/// panic on a malformed restored dump, not merely against a bad offer. The probe flips RED
/// by panic, not by a value mismatch.
#[test]
fn loop_shortcut_zero_max_iterations_is_rejected_not_clamped() {
    let shortcut_state = |max_iterations: u32| {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: P0,
            predicted_winner: Some(P0),
            certificate: engine::analysis::loop_check::LoopCertificate {
                unbounded: Vec::new(),
                win_kind: engine::analysis::loop_check::WinKind::LethalDamage,
                mandatory: false,
                residual_board_delta: engine::analysis::resource::BoardDelta::default(),
                per_cycle: None,
            },
            schema: engine::analysis::decision_template::ShortcutDecisionSchema {
                iteration_count: engine::analysis::decision_template::IterationCount::Fixed(2),
                max_iterations,
                ..Default::default()
            },
            declaration: None,
        };
        bind(&mut state, "loop-zero-bound");
        state
    };

    // ── PAIRED CONTROL, first: the byte-identical schema at the DEFAULT bound projects a
    //    shortcut schema. Without this the rejection below could be the whole window being
    //    unsupported for an unrelated reason.
    let control = shortcut_state(ShortcutDecisionSchema::default().max_iterations);
    let control_view = priority_view(&control);
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { .. },
        ..
    } = &control_view.opportunities[0].response
    else {
        panic!(
            "control: the same window at the default bound must project a shortcut schema, \
             else this row's rejection is not attributable to `max_iterations`"
        );
    };

    // ── SUBJECT: the only variable is `max_iterations: 0`.
    let subject = shortcut_state(0);
    assert_eq!(
        priority_view(&subject).availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::InvalidAuthorityState,
        },
        "CR 704.5a: `max_iterations == 0` means NO legal repetition exists, so the offer is \
         an authority violation to reject — not a number to clamp back up to 1"
    );
}

/// CR-12 — the picker's ceiling is the offer's OWN narrowed CR 732.2a bound, never the
/// raw global safety limit. Before this row the file only ever asserted the default bound,
/// so a projection that ignored `max_iterations` entirely would have stayed green.
///
/// Disclosed: an over-bound `suggested` is CLAMPED, not rejected. That is correct —
/// `suggested` is a hint, `max_iterations` is the authority.
///
/// REVERT-PROBE: change `let max = schema.max_iterations.min(MAX_SHORTCUT_CYCLES)` back to
/// `MAX_SHORTCUT_CYCLES` ⇒ `max` becomes the global cap ⇒ this assertion FAILS.
#[test]
fn loop_shortcut_narrowed_max_iterations_bounds_the_picker() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: engine::analysis::loop_check::LoopCertificate {
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::LethalDamage,
            mandatory: false,
            residual_board_delta: engine::analysis::resource::BoardDelta::default(),
            per_cycle: None,
        },
        schema: engine::analysis::decision_template::ShortcutDecisionSchema {
            // A NARROWED bound, i.e. what `elimination_bounds` produces on a real board.
            iteration_count: engine::analysis::decision_template::IterationCount::Fixed(9),
            max_iterations: 3,
            ..Default::default()
        },
        declaration: None,
    };
    bind(&mut state, "loop-narrowed-bound");

    // Reach-guard: the narrowed bound really is BELOW the global cap, else `min(..)` and
    // the global cap coincide and the row cannot discriminate.
    assert!(
        3 < ShortcutDecisionSchema::default().max_iterations,
        "reach-guard: the narrowed bound must be strictly below the global cap"
    );

    let view = priority_view(&state);
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { count, .. },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("loop shortcut uses a shortcut schema");
    };
    assert_eq!(
        *count,
        InteractionShortcutCountSpec::Fixed {
            min: 1,
            max: 3,
            suggested: 3,
        },
        "CR 732.2a: the picker's ceiling is the offer's own narrowed bound (3), and an \
         over-bound `suggested` (9) is clamped down to it rather than rejected"
    );
}

#[test]
fn loop_shortcut_number_schema_accepts_a_fixed_count_above_one() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: engine::analysis::loop_check::LoopCertificate {
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::LethalDamage,
            mandatory: false,
            residual_board_delta: engine::analysis::resource::BoardDelta::default(),
            per_cycle: None,
        },
        schema: engine::analysis::decision_template::ShortcutDecisionSchema {
            iteration_count: engine::analysis::decision_template::IterationCount::Fixed(2),
            // No narrowed CR 732.2a bound — `Default` carries the global cap.
            ..Default::default()
        },
        declaration: None,
    };
    bind(&mut state, "loop-count");
    let view = priority_view(&state);
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { .. },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("loop shortcut uses a shortcut schema");
    };
    let preview = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("loop-seven".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::Fixed { iterations: 7 },
                pins: Vec::new(),
            },
        },
    );
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
}

/// The per-period signature the C4 preview rows multiply out. Chosen so that three separate
/// ways of getting the preview wrong all show up as a value mismatch:
///
/// * **two mana colors**, so a preview that published raw axes instead of folding them into
///   one engine-side family total would emit two `Mana` rows;
/// * **a life LOSS on a seat that is not the proposer**, which `unbounded_components` drops
///   entirely (it reports only what a cycle accrues) and which a proposer-keyed subject
///   mapping would attribute to the wrong player;
/// * **a whole-game axis** (`tokens_created`) with no seat, so the `Option<u8>` subject is
///   exercised on both sides.
fn preview_period_delta() -> engine::analysis::resource::ResourceVector {
    let mut delta = engine::analysis::resource::ResourceVector::default();
    // `MANA_INDEX` is `[W, U, B, R, G, C]`.
    delta.mana[0] = 1;
    delta.mana[1] = 2;
    delta.life.insert(P1, -2);
    delta.tokens_created = 4;
    delta
}

/// A `LoopShortcut` offer stated exactly the way `certified_bounded_cycle_offer` states one:
/// `Fixed(max_iterations)` as the suggestion and the same number as the ceiling, with the
/// measured period on the certificate, and no announced decision point.
fn preview_offer(
    iteration_count: IterationCount,
    max_iterations: u32,
    per_cycle: Option<engine::analysis::resource::ResourceVector>,
) -> GameState {
    preview_offer_with_points(
        iteration_count,
        max_iterations,
        per_cycle,
        Vec::new(),
        Vec::new(),
    )
}

/// The same offer carrying announced decision points and the period's per-slot life charge.
///
/// An empty `points` schema never publishes a declaration — the same invariant row D4 asserts
/// against `build_bounded_declaration` — so `declaration: None` is what the engine itself would
/// stage. These rows exercise the PREVIEW projection, which reads the certificate and the
/// schema; a declaration here would stage a state the producer cannot emit.
fn preview_offer_with_points(
    iteration_count: IterationCount,
    max_iterations: u32,
    per_cycle: Option<engine::analysis::resource::ResourceVector>,
    points: Vec<DecisionPoint>,
    victim_slot: Vec<(DecisionSlot, i64)>,
) -> GameState {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: engine::analysis::loop_check::LoopCertificate {
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::Advantage,
            mandatory: false,
            residual_board_delta: engine::analysis::resource::BoardDelta::default(),
            per_cycle: per_cycle.map(|delta| engine::analysis::resource::PeriodicDelta {
                frames_per_period: 1,
                delta,
                declarable_victims: Vec::new(),
                victim_slot,
            }),
        },
        schema: ShortcutDecisionSchema {
            iteration_count,
            max_iterations,
            points,
            ..Default::default()
        },
        declaration: None,
    };
    bind(&mut state, "loop-preview");
    state
}

/// The announcement slots the synthetic preview offers speak through — one source, indexed,
/// the shape `certified_bounded_cycle_offer` publishes.
fn preview_slot(index: u8) -> DecisionSlot {
    DecisionSlot {
        source: engine::types::game_state::YieldTarget::AllCopies {
            card_id: CardId(9001),
            trigger_description: None,
        },
        index,
    }
}

/// A `Targets` point over player seats. The bounds follow the candidate list because the
/// projection's own authority guard refuses a positive `max_targets` beside an empty
/// `legal_targets` — which is exactly what leaves a candidate-less `0/0` point admitted.
fn player_targets_point(index: u8, seats: &[PlayerId]) -> DecisionPoint {
    let bound = u32::from(!seats.is_empty());
    DecisionPoint {
        slot: preview_slot(index),
        kind: DecisionPointKind::Targets {
            legal_targets: seats.iter().copied().map(TargetRef::Player).collect(),
            min_targets: bound,
            max_targets: bound,
            ordered: false,
        },
    }
}

/// The published shortcut offer, read whole so a row can compare an element against the count
/// window and the point that minted its allocation ids without transcribing either.
struct ShortcutOffer {
    count: InteractionShortcutCountSpec,
    points: Vec<InteractionShortcutPoint>,
    preview: Vec<InteractionShortcutPreview>,
}

fn shortcut_offer_of(state: &GameState) -> ShortcutOffer {
    let view = priority_view(state);
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::Shortcut {
                count,
                points,
                preview,
                ..
            },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("loop shortcut uses a shortcut schema");
    };
    ShortcutOffer {
        count: *count,
        points: points.clone(),
        preview: preview.clone(),
    }
}

fn shortcut_preview_of(state: &GameState) -> Vec<InteractionShortcutPreview> {
    shortcut_offer_of(state).preview
}

/// The published element for a count, or `None` when that count was not sampled.
fn element_at(
    preview: &[InteractionShortcutPreview],
    count: u32,
) -> Option<&InteractionShortcutPreview> {
    preview.iter().find(|element| element.count == count)
}

fn preview_entry(
    family: InteractionShortcutPreviewFamily,
    player: Option<u8>,
    amount: i32,
) -> InteractionShortcutPreviewEntry {
    InteractionShortcutPreviewEntry {
        family,
        player,
        amount,
    }
}

/// C4a — CR 732.2a: the offer publishes what its stated count actually DOES, computed by the
/// engine as `n × δ` over the certificate's measured per-period delta. Without this the count
/// picker C5 wires up is a number with no displayed consequence, and the only other way to
/// show one is `× count` arithmetic in the display layer, which the layer rule forbids.
///
/// **Asserted at TWO distinct counts, and that is the point of the row.** A single count is
/// satisfiable by an implementation that ignores `count` entirely and publishes the raw
/// per-cycle delta, or by one that hardcodes a constant. Only the pair pins the
/// multiplication.
///
/// REVERT-PROBES, both RUN:
/// * drop the `count` factor (`per_cycle` instead of `per_cycle.saturating_mul(count)`) ⇒
///   both arms fail on values;
/// * hardcode the factor to `3` ⇒ the `n = 3` arm still PASSES and the `n = 5` arm fails,
///   which is exactly the "one value is satisfiable by a constant" hole the second count
///   closes.
#[test]
fn loop_shortcut_preview_states_the_finished_magnitude_for_the_declared_count() {
    use engine::analysis::resource::ResourceAxis;

    // ── REACH-GUARDS on the fixture, before any preview is read. Each one names the wrong
    //    implementation it makes observable; without them this row could pass while the
    //    preview was built on the wrong fold or aggregated in the wrong layer.
    let delta = preview_period_delta();
    assert!(
        !delta
            .unbounded_components()
            .iter()
            .any(|(axis, _)| matches!(axis, ResourceAxis::Life(_))),
        "reach-guard: the victim's life LOSS is INVISIBLE to `unbounded_components`, so a \
         preview rebuilt on that fold would silently publish a lethal drain as producing \
         nothing. The `Life` expectations below are what detect it"
    );
    assert_eq!(
        delta
            .axis_components()
            .iter()
            .filter(|(axis, _)| matches!(axis, ResourceAxis::Mana(_)))
            .count(),
        2,
        "reach-guard: the period moves TWO mana axes, so the single `Mana` entry expected \
         below is proof the engine folded them — not proof that only one existed"
    );
    assert_ne!(
        P1.0, P0.0,
        "reach-guard: the victim is not the proposer, so a subject mapping keyed off the \
         proposer resolves to the wrong seat"
    );

    // The offer's own suggested count, read off the published window rather than assumed, and
    // then looked up in the published list by exact count — the same match the modal makes.
    let at = |n: u32| {
        let offer = shortcut_offer_of(&preview_offer(
            IterationCount::Fixed(n),
            n,
            Some(preview_period_delta()),
        ));
        let InteractionShortcutCountSpec::Fixed { suggested, .. } = offer.count else {
            panic!("a Fixed offer publishes a Fixed window");
        };
        element_at(&offer.preview, suggested)
            .expect("the published sample always states the suggested count")
            .clone()
    };

    let three = at(3);
    assert_eq!(
        three.count, 3,
        "the count travels WITH the magnitudes, so a renderer cannot attach them to another"
    );
    assert_eq!(
        three.entries,
        vec![
            preview_entry(InteractionShortcutPreviewFamily::Mana, None, 9),
            preview_entry(InteractionShortcutPreviewFamily::Life, Some(P1.0), -6),
            preview_entry(InteractionShortcutPreviewFamily::Tokens, None, 12),
        ],
        "CR 732.2a: three repetitions of (+1W +2U, P1 -2 life, +4 tokens) finish at +9 mana, \
         P1 at -6 life, +12 tokens"
    );

    let five = at(5);
    assert_eq!(five.count, 5);
    assert_eq!(
        five.entries,
        vec![
            preview_entry(InteractionShortcutPreviewFamily::Mana, None, 15),
            preview_entry(InteractionShortcutPreviewFamily::Life, Some(P1.0), -10),
            preview_entry(InteractionShortcutPreviewFamily::Tokens, None, 20),
        ],
        "the SECOND count is what makes this row unsatisfiable by a constant: an \
         implementation pinned to 3 passes the arm above and fails here"
    );
}

/// C4a, negative half — a preview is published only when the offer supplies BOTH authorities
/// it multiplies: a measured per-period signature and a finite count. Every arm is paired
/// with the positive control on the same builder, so none of them can pass because the whole
/// window failed to project.
#[test]
fn loop_shortcut_preview_is_absent_without_both_a_period_and_a_finite_count() {
    // ── PAIRED POSITIVE, first.
    assert!(
        !shortcut_preview_of(&preview_offer(
            IterationCount::Fixed(4),
            4,
            Some(preview_period_delta()),
        ))
        .is_empty(),
        "control: both authorities present must publish a preview, else every arm below \
         passes for an unrelated reason"
    );

    // ── No measured period: every mint except the bounded one carries `per_cycle: None`,
    //    as does every save written before that field existed.
    assert_eq!(
        shortcut_preview_of(&preview_offer(IterationCount::Fixed(4), 4, None)),
        Vec::new(),
        "an offer that states no per-period signature has nothing to multiply"
    );

    // ── CR 704.5a: `UntilLethal` is the determinate-drain mode. It names no number, so
    //    there is no declared count to state a finished magnitude for — even though the
    //    period here IS measured, which is what keeps this arm distinct from the one above.
    assert_eq!(
        shortcut_preview_of(&preview_offer(
            IterationCount::UntilLethal,
            4,
            Some(preview_period_delta()),
        )),
        Vec::new(),
        "`UntilLethal` states no finite count to multiply the period by"
    );

    // ── A period whose every family nets to zero (one W gained and one W spent) states
    //    nothing, and is dropped rather than published as a row of zeroes.
    let mut inert = engine::analysis::resource::ResourceVector::default();
    inert.mana[0] = 1;
    inert.mana[5] = -1;
    assert_eq!(
        inert.axis_components().len(),
        2,
        "reach-guard: the inert period really does move two axes, so the `None` below is the \
         family fold cancelling them — not an empty vector arriving empty"
    );
    assert_eq!(
        shortcut_preview_of(&preview_offer(IterationCount::Fixed(4), 4, Some(inert))),
        Vec::new(),
        "a period that nets to nothing on every family publishes no element at any count"
    );
}

/// C4a's hostile guard — the preview is ARITHMETIC, and must never become a clone-apply.
///
/// `game::interaction::preview_interaction` answers a different question (is this response
/// submittable) by cloning the whole `GameState` and applying to the clone. It cannot answer
/// this one: a CR 732.2a shortcut's declared count may reach `MAX_SHORTCUT_CYCLES`, and the
/// entire point of the rule is that the sequence is NOT played out to find out what it does.
/// A future rewrite that reached for the previewer would be quietly quadratic and quietly
/// wrong, and no value assertion would catch it — so this row reads the source.
///
/// ⚠ TWO SPANS, AND THE SECOND ONE IS WHY THIS ROW CAN FAIL AT ALL (fix round 2, F1).
///
/// The first revision read only `shortcut_preview_entries`, whose signature is
/// `(&ResourceVector, u32)` — no `GameState` is in scope anywhere in it, and neither is one in
/// its only caller `loop_shortcut_projection(&WaitingFor)`. The banned construct was therefore
/// not CONSTRUCTIBLE in the span, so the row could not fail no matter what regressed. MEASURED
/// by the reviewer: inserting `let mut probe_clone = authoritative_state.clone();` immediately
/// above the `loop_shortcut_projection` call left all three C4 rows green.
///
/// The clone-apply can only originate where the spec is BUILT: `opportunity_for_slot`'s
/// `LoopShortcut` arm, which holds `authoritative_state` and `filtered_state`, both
/// `&GameState`. Both spans are read now, and the arm span proves its OWN constructibility —
/// the enclosing signature binds two `&GameState` parameters and the span uses one — so it
/// cannot silently degrade into another span where the ban is unwritable. A positive control
/// proves the SEARCH is real; only the constructibility guard proves the SPAN is right.
///
/// WHAT WRONG IMPLEMENTATION WOULD STILL PASS THIS ROW? One that clones the state inside a
/// THIRD function called from the arm — the ban is textual, not a call-graph closure — and one
/// that computes the right numbers by some other expensive means. This is a routing guard; the
/// value rows above pin the arithmetic.
///
/// The likeliest instance of that first gap is closed by TYPE rather than by text: the cheapest
/// way to reach a `GameState` from the preview is to widen one of the two functions that
/// compute it to accept one, which contains none of the banned strings and lives in a span this
/// row does not read. Both parameter lists are pinned below, so the projection sees the
/// waiting-for state and nothing else, and the count-keyed mint sees the offer id and that
/// projection — and neither can anything they call. What remains uncovered is a clone reached
/// through some OTHER existing binding, which no signature can rule out.
///
/// REVERT-PROBES, ALL THREE RUN:
/// * add the line `// preview_interaction` inside `shortcut_preview_entries` ⇒ FAILS on the
///   assert (it still compiles, so the probe discriminates on the assertion, not the build);
/// * insert `let mut probe_clone = authoritative_state.clone();` immediately above the
///   `loop_shortcut_projection` call in the arm — the reviewer's exact probe ⇒ FAILS;
/// * widen `loop_shortcut_projection` to `(waiting_for: &WaitingFor, _state: &GameState)` —
///   the exact evasion the textual ban misses ⇒ FAILS on the signature pin (and on nothing
///   else, which is the point).
#[test]
fn loop_shortcut_preview_never_routes_through_the_clone_apply_previewer() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/game/interaction.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));

    // ── POSITIVE CONTROL: the banned symbol IS in this file. Without this the "does not
    //    contain" assertions below would pass just as happily against an empty read, a
    //    renamed file, or a search that never matched anything.
    assert!(
        text.contains("pub fn preview_interaction("),
        "positive control: `preview_interaction` must exist in this file, else the absence \
         asserted below is the absence of the whole search"
    );

    // The span from `marker` up to the next `terminator`, both anchored at a line start.
    let extract = |scope: &str, marker: &str, terminator: &str| -> String {
        let start = scope.find(marker).unwrap_or_else(|| {
            panic!("reach-guard: `{marker}` must be found by name, or this row is vacuous")
        });
        let rest = &scope[start + marker.len()..];
        let end = rest.find(terminator).unwrap_or(rest.len());
        format!("{marker}{}", &rest[..end])
    };

    // ── SPAN 1: the arithmetic itself.
    let arithmetic = extract(&text, "\nfn shortcut_preview_entries(", "\nfn ");
    assert!(
        arithmetic.contains("saturating_mul"),
        "reach-guard: the extracted span must be the real body — the multiplication is the \
         function's entire job, so its absence means the span is wrong"
    );

    // ── SPAN 2: the attach site, where the spec carrying the preview is built.
    let builder = "\nfn opportunity_for_slot(";
    let builder_start = text.find(builder).expect(
        "reach-guard: the spec builder must be found by name — it is the only scope holding a \
         `GameState` on the preview's path",
    );
    let builder_scope = &text[builder_start..];
    let signature_end = builder_scope
        .find(") -> ")
        .expect("reach-guard: the builder's signature must be delimited");
    let signature = &builder_scope[..signature_end];
    // ── CONSTRUCTIBILITY: the ban below is only a guard where the banned thing can be
    //    WRITTEN. This span sits inside a function that binds two `&GameState` parameters,
    //    so `authoritative_state.clone()` — the reviewer's exact probe — compiles here.
    assert!(
        signature.contains("authoritative_state: &GameState")
            && signature.contains("filtered_state: &GameState"),
        "constructibility: the arm span guards nothing unless a `GameState` is IN SCOPE to be \
         cloned. `shortcut_preview_entries` takes `(&ResourceVector, u32)`, which is exactly \
         why reading only that function produced a row that could not fail"
    );
    let attach = extract(
        builder_scope,
        "\n        HumanResponseModel::LoopShortcut => {",
        "\n        HumanResponseModel::",
    );
    assert!(
        attach.contains("loop_shortcut_projection(") && attach.contains("loop_shortcut_preview("),
        "reach-guard: the extracted arm must be the one that projects the offer AND mints the \
         preview onto the spec, else the ban is being applied to the wrong arm"
    );
    assert!(
        attach.contains("filtered_state"),
        "constructibility, second half: the arm must actually USE one of those `&GameState` \
         bindings, so a clone is writable at the exact point the reviewer's probe inserted one"
    );

    // ── SPAN 2b: the RESPOND-side attach site, inside the same enclosing signature. CR 732.2b's
    //    accept-or-shorten payload is minted here from a declaration whose count may reach
    //    `MAX_SHORTCUT_CYCLES` exactly as the offer's does, so the same ban applies.
    let respond_attach = extract(
        builder_scope,
        "\n        HumanResponseModel::ShortcutReply => {",
        "\n        HumanResponseModel::",
    );
    assert!(
        respond_attach.contains("declared_shortcut_projection(")
            && respond_attach.contains("declared_sequence_preview("),
        "reach-guard: the extracted arm must be the one that decodes the declaration AND mints \
         its element onto the spec, else the ban is being applied to the wrong arm"
    );
    assert!(
        respond_attach.contains("filtered_state"),
        "constructibility: the arm must actually USE one of the enclosing signature's two \
         `&GameState` bindings — asserted above — so a clone is writable at the exact point the \
         payload is minted"
    );

    // ── TYPE-LEVEL PIN: the ban below is TEXTUAL, so its cheapest evasion is to widen
    //    `loop_shortcut_projection` to take a `&GameState` and clone it THERE — a third span
    //    this row does not read, and one that would contain none of the three banned strings.
    //    The projection's parameter list closes that route by TYPE rather than by text: with
    //    only a `&WaitingFor` in scope, no callee it reaches can be handed a `GameState`
    //    either, so "the preview computation cannot see game state" stops being a search
    //    result and becomes a fact about the signature.
    let params_of = |name: &str| {
        // A generic list sits between the name and the parameter list, so a marker ending in
        // `(` never matches `fn shortcut_preview_basis<'a>(`.
        let marker = format!("\nfn {name}");
        let signature = extract(&text, &marker, ") -> ");
        let tail = signature
            .strip_prefix(&marker)
            .expect("`extract` re-emits its own marker, so the prefix is always present");
        let (generics, params) = tail.split_once('(').unwrap_or_else(|| {
            panic!("reach-guard: `{name}`'s parameter list must follow its name")
        });
        assert!(
            generics.is_empty() || (generics.starts_with('<') && generics.ends_with('>')),
            "reach-guard: the name-anchored marker matched `fn {name}{generics}(` — a \
             different function whose name merely starts with this one, so the pin below would \
             be about the wrong signature"
        );
        params
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches(',')
            .to_string()
    };
    assert_eq!(
        params_of("loop_shortcut_projection"),
        "waiting_for: &WaitingFor",
        "type-level pin: the shortcut projection is computed from the WAITING-FOR state alone. \
         Adding a parameter here — a `&GameState`, or anything reaching one — reopens the \
         clone-apply route through a span the textual ban below never reads"
    );
    assert_eq!(
        params_of("loop_shortcut_preview"),
        "interaction_id: &InteractionId, projection: &LoopShortcutProjection",
        "type-level pin, second half: the count-keyed magnitudes are minted from the offer's \
         id and its projection alone. Neither binding reaches a `GameState`, so a textual ban \
         over this span would be unwritable and therefore vacuous — the pin is the guard"
    );
    assert_eq!(
        params_of("declared_shortcut_projection"),
        "waiting_for: &WaitingFor",
        "type-level pin: the respond-side decode reads the already-redacted WAITING-FOR state \
         alone. Widening it to a `&GameState` reopens the clone-apply route through a span the \
         textual ban never reads"
    );
    assert_eq!(
        params_of("declared_sequence_preview"),
        "interaction_id: &InteractionId, declared: &DeclaredSequence",
        "type-level pin: the declared element is minted from the responder's interaction id and \
         the decoded declaration alone — id-minting and arithmetic on opposite sides of one \
         signature"
    );

    for (span_name, body) in [
        ("shortcut_preview_entries", &arithmetic),
        ("opportunity_for_slot's LoopShortcut arm", &attach),
        ("opportunity_for_slot's ShortcutReply arm", &respond_attach),
    ] {
        for banned in ["preview_interaction", "state.clone()", "GameState"] {
            assert!(
                !body.contains(banned),
                "CR 732.2a: the shortcut preview is `n × δ` over the certificate's measured \
                 period. {span_name} must not reach `{banned}` — a clone-apply cannot state \
                 the result of a sequence that is deliberately never played out"
            );
        }
    }

    // ── SPAN 3: the RESPONSE-SIDE attach site — the arm that hangs the previewed element on
    //    the answer, inside the one function that already holds the clone.
    let answerer = "\npub fn preview_interaction(";
    let answerer_start = text
        .find(answerer)
        .expect("reach-guard: the answering entry point must be found by name");
    let answerer_scope = &text[answerer_start..];
    let answerer_signature = &answerer_scope[..answerer_scope
        .find(") -> ")
        .expect("reach-guard: the entry point's signature must be delimited")];
    // ── CONSTRUCTIBILITY: a ban guards nothing where the banned thing cannot be written. This
    //    arm sits in a function that binds a `&GameState`, so a clone compiles at exactly the
    //    point the payload is attached.
    assert!(
        answerer_signature.contains("state: &GameState"),
        "constructibility: the attach arm guards nothing unless a `GameState` is IN SCOPE to be \
         cloned"
    );
    let response_attach = extract(
        answerer_scope,
        "\n        Ok(_) => InteractionPreview {",
        "\n        Err(_) =>",
    );
    assert!(
        response_attach.contains("declared_shortcut_preview("),
        "reach-guard: the extracted arm must be the one that ATTACHES the previewed element, \
         else the ban is being applied to the wrong arm"
    );
    assert!(
        response_attach.contains("state,"),
        "constructibility, second half: the arm must actually USE that `&GameState` binding, so \
         a clone is writable at the exact point the payload is minted"
    );

    // ── SPAN 4: the basis STRUCT and the domain STRUCT it folds in. A parameter list cannot
    //    reach a `GameState` and is pinned below instead; a struct FIELD can, so the textual ban
    //    is a real guard here.
    let basis_struct = extract(&text, "\nstruct ShortcutPreviewBasis<'a> {", "\n}");
    assert!(
        basis_struct.contains("delta:"),
        "reach-guard: the extracted text must be the struct BODY, else this span is not the \
         declaration the ban is written against"
    );
    let domain_struct = extract(&text, "\nstruct ShortcutAllocationDomain<'a> {", "\n}");
    assert!(
        domain_struct.contains("ids:"),
        "reach-guard: the extracted text must be the struct BODY, else this span is not the \
         declaration the ban is written against"
    );

    assert_eq!(
        params_of("shortcut_preview_basis"),
        "interaction_id: &InteractionId, projection: &'a LoopShortcutProjection",
        "type-level pin: the basis producer holds the half of the preview computation that moved \
         out of `loop_shortcut_preview`. Unpinned it is the cheapest widening on the whole path, \
         because a `&GameState` reaching it reaches every element minted from it"
    );
    assert_eq!(
        params_of("shortcut_allocation_domain"),
        "interaction_id: &InteractionId, projection: &'a LoopShortcutProjection",
        "type-level pin: the announced-choice domain is minted from the offer's id and its \
         projection alone. It is the half of the basis that survives a proposal with no measured \
         period, so it sits on every path the basis does and is pinned on the same ground"
    );
    assert_eq!(
        params_of("shortcut_preview_element"),
        "basis: Option<&ShortcutPreviewBasis<'_>>, count: u32, allocation: Vec<AmountAssignment>",
        "type-level pin: the single element producer is minted from a basis, a count and an \
         allocation alone — the basis OPTIONAL, so a partition whose magnitudes are unstatable is \
         published by this producer rather than by a second one. Widening it to take anything \
         reaching a `GameState` reopens the clone-apply route through a span no textual ban reads"
    );
    assert_eq!(
        params_of("declared_shortcut_preview"),
        "waiting_for: &WaitingFor, interaction_id: &InteractionId, response: &InteractionResponse",
        "CR 732.2a: the DECLARED count and its allocation are read from the offer's own \
         waiting-for state and the response that states them — not from a board. The pin is what \
         keeps that a fact about the signature rather than a search result"
    );
    assert_eq!(
        params_of("completed_shortcut_declaration"),
        "interaction_id: &InteractionId, projection: &LoopShortcutProjection, response: \
         &InteractionResponse",
        "type-level pin: the nothing-naming pin's completion sits on the preview path beside \
         every producer above it, and it runs on the SUBMIT path too. Widening it to anything \
         reaching a `GameState` reopens the clone-apply route through a span no textual ban reads"
    );

    for (span_name, body) in [
        ("preview_interaction's attach arm", &response_attach),
        ("the ShortcutPreviewBasis declaration", &basis_struct),
        ("the ShortcutAllocationDomain declaration", &domain_struct),
    ] {
        for banned in ["preview_interaction", "state.clone()", "GameState"] {
            assert!(
                !body.contains(banned),
                "CR 732.1b: the shortcut rules determine the repetition count WITHOUT performing \
                 the actions, so {span_name} must not reach `{banned}`"
            );
        }
    }
}

/// The three legs one `#[serde(default, skip_serializing_if = "Vec::is_empty")]` list carrier
/// owes, plus the positive control that keeps the two absence legs from passing against a
/// serializer that emits nothing at all.
///
/// `pointer` is where the carrier lives in the emitted JSON, so one helper serves a field on a
/// tagged union arm and a field on a plain struct without either being transcribed.
fn assert_defaulting_list_carrier<T>(pointer: &str, populated: &T, empty: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let populated_json = serde_json::to_value(populated).expect("the carrier serializes");
    assert!(
        populated_json
            .pointer(pointer)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|list| !list.is_empty()),
        "positive control: a NON-EMPTY `{pointer}` must be emitted, else every absence leg \
         below is satisfied by a serializer that writes no key under any circumstances"
    );
    assert_eq!(
        &serde_json::from_value::<T>(populated_json.clone()).expect("a populated carrier reads"),
        populated,
        "a populated `{pointer}` round-trips unchanged"
    );

    let empty_json = serde_json::to_value(empty).expect("the empty carrier serializes");
    assert!(
        empty_json.pointer(pointer).is_none(),
        "an EMPTY `{pointer}` is omitted from the emitted JSON rather than written as `[]`"
    );
    assert_eq!(
        &serde_json::from_value::<T>(empty_json).expect("an absent carrier reads"),
        empty,
        "an ABSENT `{pointer}` deserializes to the empty list"
    );

    let mut null_json = populated_json;
    *null_json
        .pointer_mut(pointer)
        .expect("the populated carrier was just asserted present") = serde_json::Value::Null;
    assert!(
        serde_json::from_value::<T>(null_json).is_err(),
        "an explicit `null` at `{pointer}` is refused — a list is not a nullable field"
    );
}

/// The wire shape of the two carriers this offer gained: the count-keyed preview list on the
/// spec, and each element's allocation.
///
/// REVERT-PROBES: drop `#[serde(default)]` on either carrier ⇒ its absent-key leg fails; drop
/// `skip_serializing_if` ⇒ its omission leg fails.
#[test]
fn the_preview_list_and_its_allocation_default_when_absent_and_are_omitted_when_empty() {
    let element = InteractionShortcutPreview {
        count: 3,
        entries: vec![preview_entry(
            InteractionShortcutPreviewFamily::Life,
            Some(P1.0),
            -6,
        )],
        allocation: vec![AmountAssignment {
            choice_id: InteractionChoiceId("k0".to_string()),
            amount: 3,
        }],
    };
    let spec = |preview: Vec<InteractionShortcutPreview>| InteractionResponseSpec::Shortcut {
        count: InteractionShortcutCountSpec::Fixed {
            min: 1,
            max: 3,
            suggested: 3,
        },
        points: Vec::new(),
        allow_decline: true,
        preview,
        confirm: engine::types::interaction::ConfirmSemantics::Explicit,
    };

    assert_defaulting_list_carrier(
        "/data/preview",
        &spec(vec![element.clone()]),
        &spec(Vec::new()),
    );
    let unallocated = InteractionShortcutPreview {
        allocation: Vec::new(),
        ..element.clone()
    };
    assert_defaulting_list_carrier("/allocation", &element, &unallocated);
}

// ═══ CR 732.2b — the respond-side declared sequence ══════════════════════════════════════════
//
// Every row below drives the PRODUCTION projection (`derive_viewer_interaction` over a
// constructed accept-or-shorten window), never the private decoder, so a producer that is right
// in isolation but unwired still fails.

/// The seat the constructed windows address. It is also the first seat those declarations
/// announce, which is what a real window looks like: the responder is one of the victims.
const R_RESPONDER: PlayerId = PlayerId(1);
const R_FIRST: PlayerId = PlayerId(1);
const R_SECOND: PlayerId = PlayerId(2);
/// The seat a constructed period's life map drains. Deliberately OUTSIDE the pair the
/// "charge escapes the declaration" boards announce.
const R_DRAINED: PlayerId = PlayerId(3);

fn four_seat_state() -> GameState {
    GameState::new(FormatConfig::standard(), 4, 42)
}

/// One CR 732.2b accept-or-shorten window on `state`, carrying the proposer's declaration.
///
/// The group key is fixed rather than derived: nothing in the respond-side projection reads it,
/// and deriving it here would state a second rule about it that no row checks.
fn respond_window_on(
    mut state: GameState,
    count: IterationCount,
    per_cycle: Option<engine::analysis::resource::PeriodicDelta>,
    decisions: Vec<engine::analysis::decision_template::PinnedDecision>,
) -> GameState {
    use engine::analysis::decision_template::{
        DecisionGroupKey, DecisionKind, DecisionTemplate, ReplayMode,
    };
    state.waiting_for = WaitingFor::RespondToShortcut {
        player: R_RESPONDER,
        remaining_players: Vec::new(),
        proposal: engine::analysis::loop_check::ShortcutProposal {
            proposer: P0,
            predicted_winner: Some(P0),
            count: count.clone(),
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::Advantage,
            template: Some(DecisionTemplate {
                owner: P0,
                decisions,
                replay: ReplayMode::Scheduled { count },
                key: DecisionGroupKey::from_sources(
                    &[preview_slot(0).source],
                    DecisionKind::LoopChoice,
                ),
            }),
            per_cycle,
        },
    };
    bind(&mut state, "respond-declared");
    state
}

fn respond_window(
    count: IterationCount,
    per_cycle: Option<engine::analysis::resource::PeriodicDelta>,
    decisions: Vec<engine::analysis::decision_template::PinnedDecision>,
) -> GameState {
    respond_window_on(four_seat_state(), count, per_cycle, decisions)
}

/// What the responder's own published `ShortcutReply` schema carries.
struct RespondReply {
    points: Vec<InteractionShortcutPoint>,
    declared: Option<InteractionShortcutPreview>,
    allocation_group: Option<u32>,
    candidates: Vec<engine::types::interaction::InteractionChoice>,
}

fn respond_reply_of(state: &GameState) -> RespondReply {
    let view = viewer_interaction(state, R_RESPONDER);
    let [opportunity] = view.opportunities.as_slice() else {
        panic!(
            "the respond window publishes exactly one opportunity to its responder, got {}",
            view.opportunities.len()
        );
    };
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::ShortcutReply {
                points,
                declared,
                allocation_group,
                ..
            },
        candidates,
    } = &opportunity.response
    else {
        panic!("the accept-or-shorten window uses a shortcut-reply schema");
    };
    RespondReply {
        points: points.clone(),
        declared: declared.clone(),
        allocation_group: *allocation_group,
        candidates: candidates.clone(),
    }
}

/// The seat a published candidate names, read off the engine's own player surface — so a row
/// states an announcement ORDER without transcribing a single choice id.
fn respond_seat_of(reply: &RespondReply, id: &InteractionChoiceId) -> Option<u8> {
    reply
        .candidates
        .iter()
        .find(|choice| choice.id == *id)?
        .surfaces
        .iter()
        .find_map(|surface| match surface {
            InteractionPresentationSurface::Player { seat, .. } => Some(*seat),
            _ => None,
        })
}

fn respond_seats_of(reply: &RespondReply, ids: &[InteractionChoiceId]) -> Vec<u8> {
    ids.iter()
        .map(|id| {
            respond_seat_of(reply, id).unwrap_or_else(|| {
                panic!("every published seat candidate carries a player surface")
            })
        })
        .collect()
}

fn respond_period(
    life: &[(PlayerId, i64)],
    victim_slot: Vec<(DecisionSlot, i64)>,
) -> engine::analysis::resource::PeriodicDelta {
    let mut delta = engine::analysis::resource::ResourceVector::default();
    for (seat, amount) in life {
        delta.life.insert(*seat, *amount);
    }
    engine::analysis::resource::PeriodicDelta {
        frames_per_period: 1,
        delta,
        declarable_victims: Vec::new(),
        victim_slot,
    }
}

/// A piecewise-scheduled announced-target pin: `starts[i]` is where subject `i` takes over, which
/// is that partition's own prefix sums.
fn piecewise_pin(
    slot: DecisionSlot,
    starts: &[u32],
    subjects: &[engine::analysis::decision_template::AnnouncementSubject],
) -> engine::analysis::decision_template::PinnedDecision {
    use engine::analysis::decision_template::{PinnedDecision, Ranking, TargetPin, TargetSchedule};
    assert_eq!(
        starts.len(),
        subjects.len(),
        "fixture guard: a piecewise schedule names one subject per start"
    );
    PinnedDecision::Targets {
        slot,
        targets: vec![TargetPin::Scheduled(TargetSchedule::Piecewise(
            starts
                .iter()
                .zip(subjects)
                .map(|(start, subject)| (*start, Ranking::one(subject.clone())))
                .collect(),
        ))],
    }
}

fn seat_subjects(
    seats: &[PlayerId],
) -> Vec<engine::analysis::decision_template::AnnouncementSubject> {
    use engine::analysis::decision_template::AnnouncementSubject;
    seats
        .iter()
        .copied()
        .map(AnnouncementSubject::Seat)
        .collect()
}

fn object_subject(id: ObjectId) -> engine::analysis::decision_template::AnnouncementSubject {
    engine::analysis::decision_template::AnnouncementSubject::Object(
        engine::types::game_state::YieldTarget::ThisObject {
            source_id: id,
            incarnation: Some(1),
            trigger_description: None,
        },
    )
}

/// The segment LENGTHS a piecewise schedule's starts imply at `count` — a row's restatement of
/// its own fixture, in the producer's arithmetic rather than beside it.
fn segments_from(starts: &[u32], count: u32) -> Vec<u32> {
    starts
        .iter()
        .zip(starts.iter().skip(1).copied().chain(std::iter::once(count)))
        .map(|(start, next)| next - start)
        .collect()
}

fn declared_amounts(element: &InteractionShortcutPreview) -> Vec<u32> {
    element
        .allocation
        .iter()
        .map(|assignment| assignment.amount)
        .collect()
}

/// **CR 119.3 — the attribution guard fires on ONE board and on no other.**
///
/// `victim_charge` refuses for FOUR reasons; only one of them is about this
/// call site's narrower seat domain. The guard therefore re-asks that same rule over the period's
/// own life-map keys and withholds the MAGNITUDES only when the charge it resolves names a seat
/// the declaration does NOT announce — because folding it without a split would key the whole
/// drain on the seat the period was measured on, which is the number the responder decides on.
///
/// # What the guard withholds is the MAGNITUDES, never the partition
///
/// Segment lengths are not magnitudes, so the declaration's own partition is published on the
/// board the guard fires on too — only its entry list is empty there.
///
/// # All four legs run in ONE invocation, so each is the others' positive control
///
/// Leg 1 is the board the guard exists for; legs 2, 3 and 4 each falsify exactly one conjunct.
/// Every leg asserts a POSITIVE fact — a published point set, a present element carrying the
/// declaration's own two distinct segments — so "the projection published nothing" cannot satisfy
/// any of them.
///
/// REVERT-PROBES: delete the guard ⇒ leg 1's empty-entries assertion flips (the responder is
/// handed `[Life P4 −72]`, the whole drain attributed to a seat the declaration never names);
/// delete the `!announced.contains(..)` conjunct ⇒ leg 2 fails; take the charge probe without the
/// `basis.seats` wrapper ⇒ leg 4 fails; use `basis.charge.is_none()` as the probe ⇒ leg 3 fails
/// on its first assertion; refuse the whole element on the magnitude leg instead of emptying its
/// entries ⇒ leg 1's partition assertion fails.
#[test]
fn the_declared_magnitudes_are_withheld_only_when_the_periods_charge_escapes_the_declaration() {
    const COUNT: u32 = 6;
    const STARTS: [u32; 2] = [0, 2];
    let slot = preview_slot(0);
    let announced = [R_FIRST, R_SECOND];

    // ── LEG 1 — IT FIRES. One losing seat, charged through this very slot, and the
    //    declaration announces two OTHER seats. ──
    let escapes = respond_window(
        IterationCount::Fixed(COUNT),
        Some(respond_period(
            &[(R_DRAINED, -12)],
            vec![(slot.clone(), 12)],
        )),
        vec![piecewise_pin(
            slot.clone(),
            &STARTS,
            &seat_subjects(&announced),
        )],
    );
    let reply = respond_reply_of(&escapes);
    assert_eq!(
        respond_seats_of(&reply, &reply.points[0].candidate_ids),
        vec![R_FIRST.0, R_SECOND.0],
        "reach-guard: the declaration IS still published — the guard withholds the magnitudes, \
         never the statement — and it announces exactly the two seats the period does not drain"
    );
    let element = reply.declared.as_ref().expect(
        "CR 732.2b: the partition is the declaration's own segment lengths and not a magnitude, \
         so it is published on the very board the attribution guard fires on",
    );
    assert_eq!(
        declared_amounts(element),
        vec![2, 4],
        "ANTI-VACUITY: TWO segments, pairwise DISTINCT — the responder reads the whole \
         declaration, so an element emptied wholesale cannot satisfy this"
    );
    assert!(
        element.entries.is_empty(),
        "CR 119.3: the period's per-slot charge resolves to a seat this declaration never \
         announces, so NO magnitude is stated rather than one keying the whole drain on the seat \
         the period was measured on. got {:?}",
        element.entries
    );

    // ── LEG 2 — the membership conjunct. The same board with the drained seat announced. ──
    let announces_victim = [R_DRAINED, R_FIRST];
    let member = respond_window(
        IterationCount::Fixed(COUNT),
        Some(respond_period(
            &[(R_DRAINED, -12)],
            vec![(slot.clone(), 12)],
        )),
        vec![piecewise_pin(
            slot.clone(),
            &STARTS,
            &seat_subjects(&announces_victim),
        )],
    );
    let reply = respond_reply_of(&member);
    let element = reply
        .declared
        .as_ref()
        .expect("CR 119.3: a declaration that announces the charged seat states its magnitudes");
    assert_eq!(
        declared_amounts(element),
        segments_from(&STARTS, COUNT),
        "the published partition is the declaration's own segment lengths"
    );
    assert_eq!(
        declared_amounts(element),
        vec![2, 4],
        "ANTI-VACUITY: TWO segments, pairwise DISTINCT — a flag standing in for a count, or a \
         uniform split, cannot satisfy this"
    );
    assert_eq!(
        respond_seats_of(&reply, &reply.points[0].candidate_ids),
        vec![R_DRAINED.0, R_FIRST.0]
    );
    assert_eq!(
        element.entries,
        vec![
            preview_entry(InteractionShortcutPreviewFamily::Life, Some(R_FIRST.0), -48),
            preview_entry(
                InteractionShortcutPreviewFamily::Life,
                Some(R_DRAINED.0),
                -24
            ),
        ],
        "CR 119.3: the drain follows the DECLARATION — 2 cycles on the announced victim and 4 \
         on the seat it named second, at the period's own rate — and the two magnitudes differ, \
         so a producer that re-attributed uniformly fails here"
    );

    // ── LEG 3 — the probe conjunct, over the whole class of remaining refusals. On each board
    //    the charged seat is unannounced AND `victim_charge` refuses over the period's own keys
    //    too, so BOTH sides fold the period unsplit and publishing is correct. ──
    for (why, period) in [
        (
            "no victim_slot entry for this point's slot",
            respond_period(&[(R_DRAINED, -12)], Vec::new()),
        ),
        (
            "a life map naming TWO losing seats",
            respond_period(&[(P0, -12), (R_DRAINED, -36)], vec![(slot.clone(), 36)]),
        ),
        (
            "a charge that is not the whole of the seat's loss",
            respond_period(&[(R_DRAINED, -36)], vec![(slot.clone(), 12)]),
        ),
    ] {
        let expected: Vec<InteractionShortcutPreviewEntry> = period
            .delta
            .life
            .iter()
            .map(|(seat, amount)| {
                preview_entry(
                    InteractionShortcutPreviewFamily::Life,
                    Some(seat.0),
                    (*amount * i64::from(COUNT)) as i32,
                )
            })
            .collect();
        let state = respond_window(
            IterationCount::Fixed(COUNT),
            Some(period),
            vec![piecewise_pin(
                slot.clone(),
                &STARTS,
                &seat_subjects(&announced),
            )],
        );
        let reply = respond_reply_of(&state);
        let element = reply.declared.as_ref().unwrap_or_else(|| {
            panic!("the guard must NOT fire on the board where {why}: both sides refuse alike")
        });
        assert_eq!(
            declared_amounts(element),
            vec![2, 4],
            "the responder still sees the partition on the {why} board"
        );
        assert!(
            !element.entries.is_empty(),
            "reach-guard: the {why} board really states magnitudes, so the equality below is \
             not two empty lists"
        );
        assert_eq!(
            element.entries, expected,
            "CR 732.2a: with no resolvable charge over its own domain the element folds the \
             PERIOD's own seat keys, unsplit — which is what the offer's element does on the \
             identical period ({why})"
        );
    }

    // ── LEG 4 — the seat-domain conjunct. A declaration announcing OBJECTS has no seat domain
    //    at all, so nothing narrowed and the offer's own fold is right for it too. ──
    let objects = respond_window(
        IterationCount::Fixed(COUNT),
        Some(respond_period(
            &[(R_DRAINED, -12)],
            vec![(slot.clone(), 12)],
        )),
        vec![piecewise_pin(
            slot.clone(),
            &STARTS,
            &[
                object_subject(ObjectId(4001)),
                object_subject(ObjectId(4002)),
            ],
        )],
    );
    let reply = respond_reply_of(&objects);
    let element = reply.declared.as_ref().expect(
        "a declaration whose announced subjects are OBJECTS narrows no seat domain, so the \
         attribution guard has nothing to fire on",
    );
    assert!(
        reply.points[0]
            .candidate_ids
            .iter()
            .all(|id| respond_seat_of(&reply, id).is_none()),
        "reach-guard: this board really publishes OBJECT subjects — if either resolved to a \
         seat, `allocated_seats` would answer `Some` and this leg would be leg 2 again"
    );
    assert_eq!(
        declared_amounts(element),
        vec![2, 4],
        "the partition is still the declaration's own"
    );
    assert_eq!(
        element.entries,
        vec![preview_entry(
            InteractionShortcutPreviewFamily::Life,
            Some(R_DRAINED.0),
            -72
        )],
        "CR 732.2a: the period's own seat key, times the declared count — the offer's fold, \
         reached unchanged"
    );
}

/// **CR 601.2c — two announced-target decisions, and the segments belong to the one
/// `allocation_point` picks.**
///
/// A proposal may carry more than one announced-target decision. `allocation_point` names the
/// FIRST in published order as the one the allocation is stated over, and every later one
/// publishes its declared ORDER with no allocation stated over it. Reading the segments and the
/// candidate ids back through that same index is what keeps the two halves of the element about
/// one point.
///
/// # The fixture is built so a producer cannot be right by accident
///
/// The two segment vectors are PERMUTATIONS of each other, so a producer that sorts is visible;
/// they are DIFFERENT, so a producer that carried a flat vector written by whichever decision
/// the walk saw last is visible; and the two points announce the seats in different orders, so
/// zipping one point's segments against the other's ids is visible.
///
/// REVERT-PROBES: carry one flat segment vector written by the LAST `Targets` decision ⇒ (1)
/// fails; read a segment entry chosen by anything other than `basis.group` ⇒ (1) and (2) fail.
#[test]
fn the_declared_allocation_belongs_to_the_first_announced_target_decision() {
    const COUNT: u32 = 6;
    const FIRST_STARTS: [u32; 3] = [0, 1, 3];
    const SECOND_STARTS: [u32; 3] = [0, 3, 5];
    let first_order = [R_FIRST, R_SECOND, R_DRAINED];
    let second_order = [R_DRAINED, R_SECOND, R_FIRST];

    // (0) The fixture's own two vectors, asserted DIFFERENT before anything is read off the
    //     projection. A fixture edit that made them equal takes this row's discriminating power
    //     with it, which is what this assertion is for.
    let first_segments = segments_from(&FIRST_STARTS, COUNT);
    let second_segments = segments_from(&SECOND_STARTS, COUNT);
    assert_eq!(first_segments, vec![1, 2, 3]);
    assert_eq!(second_segments, vec![3, 2, 1]);
    assert_ne!(
        first_segments, second_segments,
        "fixture guard: the two declarations must partition the count DIFFERENTLY, or reading \
         the wrong one would be invisible"
    );

    let state = respond_window(
        IterationCount::Fixed(COUNT),
        Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
        vec![
            piecewise_pin(preview_slot(0), &FIRST_STARTS, &seat_subjects(&first_order)),
            piecewise_pin(
                preview_slot(1),
                &SECOND_STARTS,
                &seat_subjects(&second_order),
            ),
        ],
    );
    let reply = respond_reply_of(&state);
    assert_eq!(
        reply.points.len(),
        2,
        "reach-guard: both decisions publish a point"
    );
    assert!(
        reply
            .points
            .iter()
            .all(|point| point.kind == InteractionShortcutPointKind::Targets),
        "reach-guard: both are announced-target points, so `allocation_point` really has two to \
         choose between"
    );
    let element = reply
        .declared
        .as_ref()
        .expect("the first decision partitions the declared count, so an element is stated");

    // (1) THE DISCRIMINATING ASSERTION.
    assert_eq!(
        declared_amounts(element),
        first_segments,
        "CR 601.2c: the allocation is stated over the FIRST announced-target decision, so its \
         amounts are that decision's segment lengths — a decoder carrying a flat vector written \
         by the last decision publishes {second_segments:?} and fails here"
    );
    // (2) Independently discriminating: zipping the first point's SEGMENTS against the second
    //     point's IDS satisfies (1) and fails this.
    assert!(
        element.allocation.iter().all(|assignment| reply.points[0]
            .candidate_ids
            .contains(&assignment.choice_id)),
        "every allocation position names a candidate of the point it is stated over"
    );
    // (3) The later decision publishes its declared ORDER and no allocation of its own.
    let second_seats = respond_seats_of(&reply, &reply.points[1].candidate_ids);
    assert_eq!(
        second_seats,
        second_order.iter().map(|seat| seat.0).collect::<Vec<_>>(),
        "the second decision publishes the proposer's own announcement order"
    );
    let mut reversed = second_seats.clone();
    reversed.reverse();
    assert_ne!(
        second_seats, reversed,
        "ANTI-VACUITY: the published order differs from itself reversed, so a producer that \
         sorted or reversed it fails and an empty-vs-empty comparison cannot satisfy the \
         equality above"
    );
}

/// **CR 601.2c — the published group is the allocated decision's OWN group, not its position
/// among the announced-target points.**
///
/// The two coincide on every declaration whose first decision is the announced-target one, which
/// is why a reader that infers the allocated decision from a position is invisible on them. Here
/// an OPTIONAL decision is answered first, so the decision the allocation partitions is published
/// at group 1 while being the only `Targets` point — and the responder still has to be able to
/// tell which decision's order the allocation already states.
///
/// # Discrimination
///
/// Number the allocation by its position among `Targets` points, or assume the allocated
/// decision is always the first point ⇒ 0 is published and the equality fails. Drop the field ⇒
/// the `Some` fails. The row asserts the group is NOT the first published point's, so a fixture
/// edit that put the announced-target decision back in front takes the discrimination with it
/// and says so.
#[test]
fn the_allocation_group_is_the_allocated_decisions_own_published_group() {
    use engine::analysis::decision_template::{DecisionSlot, MayChoiceOption, PinnedDecision};
    use engine::types::game_state::YieldTarget;

    const COUNT: u32 = 6;
    const STARTS: [u32; 2] = [0, 2];

    let reply = respond_reply_of(&respond_window(
        IterationCount::Fixed(COUNT),
        Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
        vec![
            PinnedDecision::MayChoice {
                slot: DecisionSlot::may(YieldTarget::ThisObject {
                    source_id: ObjectId(9_317),
                    incarnation: Some(1),
                    trigger_description: None,
                }),
                take: MayChoiceOption::Take,
            },
            piecewise_pin(
                preview_slot(0),
                &STARTS,
                &seat_subjects(&[R_FIRST, R_SECOND]),
            ),
        ],
    ));
    assert_eq!(
        reply
            .points
            .iter()
            .map(|point| point.kind)
            .collect::<Vec<_>>(),
        vec![
            InteractionShortcutPointKind::MayChoice,
            InteractionShortcutPointKind::Targets,
        ],
        "reach-guard: the optional decision publishes AHEAD of the announced-target one, which \
         is what separates a group from a position"
    );
    assert_eq!(
        declared_amounts(
            reply
                .declared
                .as_ref()
                .expect("the announced-target decision partitions the declared count")
        ),
        segments_from(&STARTS, COUNT),
        "reach-guard: the allocation is stated, so there is a group for it to be stated over"
    );
    assert_eq!(reply.allocation_group, Some(reply.points[1].group));
    assert_ne!(
        reply.allocation_group,
        Some(reply.points[0].group),
        "the named group is not the FIRST published point's, so a reader that dropped the first \
         announced decision by position would drop the wrong one"
    );
}

/// **THE PUBLICATION POSTURE, RUN FROM BOTH ENDS.**
///
/// A decision this projection can state nothing about publishes NO POINT, and the rest of the
/// declaration publishes anyway. Leg B is the board where nothing survives that skip: its
/// unstatable announced-target decision is the only decision it carries, so the walk ends with
/// no statement to publish and the responder is shown nothing rather than an empty proposal.
///
/// # Both legs are latent in production, and that is stated rather than implied
///
/// Both `DecisionSlot::may` call sites build their source through `object_decision_source`,
/// which constructs a live-object source unconditionally, so leg A's card-identity branch is
/// wired rather than exercised today; leg B's production reachability is unmeasured. Neither is
/// a reason to leave the branch's behaviour untested — the branch is judged on what it does when
/// reached.
///
/// # Discrimination
///
/// Refuse the whole sequence on an unstatable optional decision ⇒ leg A fails on both halves.
/// Publish a ONE-candidate statement point for it ⇒ leg A's `points` assertion fails, because
/// that posture mints a point on the very board where leg A asserts none. Publish a partial
/// subject list for an unstatable announced target ⇒ leg B's `points.is_empty()` fails.
#[test]
fn an_unstatable_optional_decision_is_skipped_and_a_lone_unstatable_target_states_nothing() {
    use engine::analysis::decision_template::{
        DecisionSlot, MayChoiceOption, PinnedDecision, TargetPin,
    };
    use engine::types::game_state::YieldTarget;

    const COUNT: u32 = 6;
    const STARTS: [u32; 2] = [0, 2];

    let card_identity = YieldTarget::AllCopies {
        card_id: CardId(9_314),
        trigger_description: None,
    };
    let live_object = YieldTarget::ThisObject {
        source_id: ObjectId(9_315),
        incarnation: Some(1),
        trigger_description: None,
    };
    let targets_first = piecewise_pin(
        preview_slot(0),
        &STARTS,
        &seat_subjects(&[R_FIRST, R_SECOND]),
    );
    let with_optional = |source: YieldTarget| {
        respond_window(
            IterationCount::Fixed(COUNT),
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            vec![
                targets_first.clone(),
                PinnedDecision::MayChoice {
                    slot: DecisionSlot::may(source),
                    take: MayChoiceOption::Take,
                },
            ],
        )
    };

    // ── LEG A: the optional decision's slot names a CARD identity, which mints no subject. It
    //    costs the declaration one statement line and nothing else.
    let skipped = respond_reply_of(&with_optional(card_identity));
    assert_eq!(
        skipped
            .points
            .iter()
            .map(|point| point.kind)
            .collect::<Vec<_>>(),
        vec![InteractionShortcutPointKind::Targets],
        "an optional decision whose subject cannot be minted publishes NO point, and the \
         announced-target decision beside it publishes anyway"
    );
    let skipped_element = skipped
        .declared
        .as_ref()
        .expect("and the declaration keeps its partition and its magnitudes");
    assert_eq!(
        declared_amounts(skipped_element),
        vec![2, 4],
        "ANTI-VACUITY: more than one segment, pairwise distinct — 'everything empty' is not \
         what this leg asserts and could not satisfy it"
    );

    // ── The paired positive on the identical instrument: the same declaration whose optional
    //    decision names a LIVE object publishes a second point at arity two — and the declared
    //    element is EQUAL, because the announced-target decision is first in the walk and its
    //    candidate indices are unchanged either way.
    let published = respond_reply_of(&with_optional(live_object));
    assert_eq!(
        published
            .points
            .iter()
            .map(|point| point.kind)
            .collect::<Vec<_>>(),
        vec![
            InteractionShortcutPointKind::Targets,
            InteractionShortcutPointKind::MayChoice,
        ]
    );
    assert_eq!(
        published.points[1].candidate_ids.len(),
        2,
        "a PUBLISHED optional-decision statement point carries exactly two candidates — subject \
         then answer — so the client's positional read is total over what arrives"
    );
    assert_eq!(
        published.declared, skipped.declared,
        "publishing an optional decision does not move the allocation: the announced-target \
         decision is first in the walk, so its minted ids are the same on both boards"
    );

    // ── LEG B: an announced-target subject that cannot be minted is this board's ONLY
    //    decision, so the walk reaches its end with no point to publish.
    let announced = |source: YieldTarget| {
        respond_window(
            IterationCount::Fixed(COUNT),
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            vec![PinnedDecision::Targets {
                slot: preview_slot(0),
                targets: vec![TargetPin::ByIdentity(source)],
            }],
        )
    };
    let refused = respond_reply_of(&announced(YieldTarget::AllCopies {
        card_id: CardId(9_316),
        trigger_description: None,
    }));
    assert!(
        refused.points.is_empty() && refused.declared.is_none(),
        "a card identity mints no announced subject, and it is this board's only decision — so \
         the walk publishes no statement at all rather than an empty proposal"
    );

    // ── Paired positive in the same leg: the same declaration on a live object publishes.
    let minted = respond_reply_of(&announced(YieldTarget::ThisObject {
        source_id: ObjectId(9_317),
        incarnation: Some(1),
        trigger_description: None,
    }));
    assert_eq!(
        minted.points.len(),
        1,
        "the same declaration one identity apart publishes its statement point"
    );
    assert_eq!(minted.points[0].candidate_ids.len(), 1);
    assert_eq!(
        declared_amounts(
            minted
                .declared
                .as_ref()
                .expect("and a single announced subject takes the whole declared count")
        ),
        vec![COUNT]
    );
}

/// **CR 732.2a — a scheduled step carrying a next-episode tail still states this drive.**
///
/// A drive resolves a step's HEAD and never advances past it, so a tail is no part of the
/// sequence the responder is being asked to accept. The head is the whole statement of what its
/// segment performs, and a tail therefore changes nothing the responder reads: the same
/// subjects, the same partition, and the same other decisions.
///
/// No declare ingress mints such a tail — `declaration_conforms` refuses one — so a save or a
/// wire restore is the way in, and the branch is judged on what it does when reached rather than
/// on how it is reached today.
///
/// # Discrimination
///
/// Refuse the sequence on a step whose ranking names more than its head ⇒ every assertion on the
/// tail-carrying board fails, the optional decision beside it included — that refusal returns
/// from the whole decision walk, so the responder loses the partition, the magnitudes, and every
/// unrelated statement to withhold one subject this drive never performs. Publish every subject
/// instead ⇒ the same comparison fails, the tail-carrying board arriving at arity three.
#[test]
fn a_scheduled_step_carrying_a_next_episode_tail_still_states_this_drive() {
    use engine::analysis::decision_template::{
        DecisionSlot, MayChoiceOption, PinnedDecision, Ranking, TargetPin, TargetSchedule,
    };
    use engine::types::game_state::YieldTarget;

    const COUNT: u32 = 6;
    const STARTS: [u32; 2] = [0, 2];

    let scheduled = |second_step: &[PlayerId]| {
        let step = |start: u32, seats: &[PlayerId]| {
            (
                start,
                Ranking::new(seat_subjects(seats)).expect("distinct seats make a legal ranking"),
            )
        };
        respond_window(
            IterationCount::Fixed(COUNT),
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            vec![
                PinnedDecision::Targets {
                    slot: preview_slot(0),
                    targets: vec![TargetPin::Scheduled(TargetSchedule::Piecewise(vec![
                        step(STARTS[0], &[R_FIRST]),
                        step(STARTS[1], second_step),
                    ]))],
                },
                // A SECOND decision, so "the rest of the walk survives" is a fact this row can
                // see: a refusal returns from the walk and takes this point with it.
                PinnedDecision::MayChoice {
                    slot: DecisionSlot::may(YieldTarget::ThisObject {
                        source_id: ObjectId(9_318),
                        incarnation: Some(1),
                        trigger_description: None,
                    }),
                    take: MayChoiceOption::Take,
                },
            ],
        )
    };

    // ── The paired positive, first: one subject per step, the arity the human ingress mints.
    //    It is what the tail-carrying board below is compared against, so an empty publication
    //    cannot satisfy that comparison.
    let one_each = respond_reply_of(&scheduled(&[R_SECOND]));
    assert_eq!(
        one_each
            .points
            .iter()
            .map(|point| point.kind)
            .collect::<Vec<_>>(),
        vec![
            InteractionShortcutPointKind::Targets,
            InteractionShortcutPointKind::MayChoice,
        ],
        "reach-guard: a one-subject-per-step schedule reaches the responder whole, the optional \
         decision beside it included"
    );
    assert_eq!(
        respond_seats_of(&one_each, &one_each.points[0].candidate_ids),
        vec![R_FIRST.0, R_SECOND.0],
        "and its two steps are announced in order"
    );
    assert_eq!(
        declared_amounts(
            one_each
                .declared
                .as_ref()
                .expect("and they partition the declared count")
        ),
        segments_from(&STARTS, COUNT),
        "ANTI-VACUITY: TWO segments, pairwise distinct — 'everything empty' is not what this row \
         asserts and could not satisfy it"
    );

    // ── The tail-carrying board: the SECOND step also names a subject past its head.
    let with_tail = respond_reply_of(&scheduled(&[R_SECOND, R_DRAINED]));
    // The whole-publication claim runs FIRST and by VALUE: a refusal empties `points`, so an
    // assertion indexing it would panic before naming what went wrong.
    assert_eq!(
        with_tail.points, one_each.points,
        "the tail moves NOTHING the responder reads: the same two statements, the optional \
         decision included rather than lost with the whole walk"
    );
    assert_eq!(
        respond_seats_of(&with_tail, &with_tail.points[0].candidate_ids),
        vec![R_FIRST.0, R_SECOND.0],
        "CR 732.2a: each step states the head this drive resolves, and the tail seat — which no \
         iteration reaches — is not announced beside it"
    );
    assert_eq!(
        with_tail.declared, one_each.declared,
        "CR 732.2b: and the partition the responder accepts or shortens is the same one"
    );
}

/// **CR 732.2a — a multi-subject schedule states the HEAD its drive announces.**
///
/// `evaluate_schedule` resolves `Ranking::head` for a schedule of every kind and never advances
/// past it, so no iteration announces a ranking's tail. The responder is shown one subject per
/// step — what the drive performs — and that subject
/// takes the whole of its step. CR 732.1b: an until-lethal proposal names no count to partition,
/// so it states an ORDER and no magnitude.
///
/// # Discrimination
///
/// Publish the whole ranking on either shape ⇒ that shape's tail-carrying board reaches arity
/// three and the equality against its one-subject board fails. Refuse a multi-subject schedule ⇒
/// the same equality fails on an empty publication. Withhold the partition from a multi-subject
/// schedule under a finite count ⇒ the declared-element equality fails.
#[test]
fn a_multi_subject_schedule_states_the_head_its_drive_announces() {
    use engine::analysis::decision_template::{PinnedDecision, Ranking, TargetPin, TargetSchedule};

    const COUNT: u32 = 6;
    /// The two boards share a head, so their publications differ only in the tail.
    const HEAD: [PlayerId; 1] = [R_FIRST];
    const TAILED: [PlayerId; 3] = [R_FIRST, R_SECOND, R_DRAINED];

    // The two schedule shapes that carry a ranking, driven through ONE assertion so the arms
    // cannot answer the same question differently.
    let shapes = |seats: &[PlayerId]| {
        let ranking =
            Ranking::new(seat_subjects(seats)).expect("distinct seats make a legal ranking");
        [
            ("a constant", TargetSchedule::Constant(ranking.clone())),
            (
                "a piecewise step",
                TargetSchedule::Piecewise(vec![(0, ranking)]),
            ),
        ]
    };
    let window = |count: &IterationCount, schedule: TargetSchedule| {
        respond_reply_of(&respond_window(
            count.clone(),
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            vec![PinnedDecision::Targets {
                slot: preview_slot(0),
                targets: vec![TargetPin::Scheduled(schedule)],
            }],
        ))
    };

    for count in [IterationCount::UntilLethal, IterationCount::Fixed(COUNT)] {
        // A one-step schedule partitions the whole declared count, and nothing at all when the
        // proposal names no count.
        let partition = match &count {
            IterationCount::UntilLethal => None,
            IterationCount::Fixed(n) => Some(vec![*n]),
        };
        for ((shape, tailed), (_, single)) in shapes(&TAILED).into_iter().zip(shapes(&HEAD)) {
            // ── The paired positive first: the one-subject board the comparison stands on, so
            //    an empty publication cannot satisfy it.
            let announced = window(&count, single);
            assert_eq!(
                respond_seats_of(&announced, &announced.points[0].candidate_ids),
                vec![R_FIRST.0],
                "reach-guard: a one-subject {shape} publishes the subject it announces"
            );
            assert_eq!(
                announced.declared.as_ref().map(declared_amounts),
                partition,
                "CR 732.1b: {shape} states the count it partitions, and no magnitude where there \
                 is no count to partition"
            );

            let with_tail = window(&count, tailed);
            assert_eq!(
                with_tail.points, announced.points,
                "CR 732.2a: the tail moves nothing the responder reads — {shape} states the head \
                 its drive announces, at the same arity"
            );
            assert_eq!(
                respond_seats_of(&with_tail, &with_tail.points[0].candidate_ids),
                vec![R_FIRST.0],
                "and the tail seats, which no iteration announces, are not published beside it"
            );
            assert_eq!(
                with_tail.declared, announced.declared,
                "CR 732.2b: and the head takes the same partition the one-subject board states"
            );
        }
    }
}

/// **CR 732.2a — a step announced at the declared count itself takes a ZERO-length segment.**
///
/// The partition is successive differences with the last running to the count, so a step whose
/// start IS the count is declared for no iteration. Its segment is stated as zero rather than
/// dropped: the allocation is read back against that decision's own published ids, and a
/// partition one entry short of them is not the one the proposer declared. This is the one shape
/// whose published amounts are not all positive, which is what the `allocation` field doc — and
/// through it the client's mirror — has to stay true to.
///
/// # Discrimination
///
/// Treat a zero-length segment as unstatable ⇒ the decision is skipped and the reach-guard
/// fails. Drop zeros from the partition ⇒ it is one entry short of the published ids, the
/// length check withholds the element, and the `expect` fails.
#[test]
fn a_step_announced_at_the_declared_count_takes_a_zero_length_segment() {
    const COUNT: u32 = 3;
    const STARTS: [u32; 2] = [0, COUNT];

    let reply = respond_reply_of(&respond_window(
        IterationCount::Fixed(COUNT),
        Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
        vec![piecewise_pin(
            preview_slot(0),
            &STARTS,
            &seat_subjects(&[R_FIRST, R_SECOND]),
        )],
    ));
    assert_eq!(
        reply.points.len(),
        1,
        "reach-guard: a legal zero-repetition step stays statable, so the decision publishes"
    );
    let element = reply
        .declared
        .as_ref()
        .expect("and its partition is stated over the point's published ids");
    assert_eq!(
        declared_amounts(element),
        vec![COUNT, 0],
        "the second step is announced at the count, so it is declared for no iteration"
    );
    assert_eq!(
        element.allocation.len(),
        reply.points[0].candidate_ids.len(),
        "one amount per announced subject — a dropped zero would leave the partition short of \
         the ids it is stated over"
    );
    assert_eq!(
        element.count, COUNT,
        "and the segments still total the declared count"
    );
}

/// **CR 732.1b — an order-only declaration publishes the ORDER its drive announces in, and no
/// magnitude.**
///
/// An until-lethal proposal names no count, so there is nothing for a partition to divide and a
/// magnitude stated there could only be invented. What it does state is the sequence of
/// announcements the proposer described (CR 732.2a) — one subject per scheduled step, in their
/// order — which is the object CR 732.2b gives the responder the right to shorten.
///
/// # Anti-vacuity, and the paired positive
///
/// The published order carries TWO entries over DISTINCT seats and is asserted against its own
/// reversal, so a producer that sorted, reversed or emptied it cannot satisfy this row. The same
/// pin under a FIXED count publishes that same two-entry order AND a partition, so the absent
/// magnitude is a branch rather than a projection that never happens.
///
/// # Discrimination
///
/// Resolve `declared_count` to anything but `None` for an until-lethal proposal ⇒ the order-only
/// board states a partition and the `is_none` leg fails. Publish the steps in any other order, or
/// only the first ⇒ the seat equality fails on both boards.
#[test]
fn an_order_only_declaration_publishes_its_announcement_order_and_no_magnitude() {
    const COUNT: u32 = 6;
    const STARTS: [u32; 2] = [0, 2];

    let window = |count: IterationCount| {
        respond_reply_of(&respond_window(
            count,
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            vec![piecewise_pin(
                preview_slot(0),
                &STARTS,
                &seat_subjects(&[R_FIRST, R_SECOND]),
            )],
        ))
    };
    let announced_seats = |reply: &RespondReply| {
        assert_eq!(
            reply.points.len(),
            1,
            "reach-guard: the declaration publishes its announced-target statement point"
        );
        respond_seats_of(reply, &reply.points[0].candidate_ids)
    };

    // ── The paired positive first: the same two-step declaration under a FIXED count states both
    //    halves, so the missing magnitude below cannot be a projection that never ran.
    let counted = window(IterationCount::Fixed(COUNT));
    assert_eq!(announced_seats(&counted), vec![R_FIRST.0, R_SECOND.0]);
    assert_eq!(
        declared_amounts(
            counted
                .declared
                .as_ref()
                .expect("a declared count is partitioned over the steps it is announced across")
        ),
        segments_from(&STARTS, COUNT),
    );

    let order_only = window(IterationCount::UntilLethal);
    let seats = announced_seats(&order_only);
    assert_eq!(
        seats,
        vec![R_FIRST.0, R_SECOND.0],
        "CR 732.2a: the responder is shown the announcement sequence the proposer described, at \
         the arity the drive performs it"
    );
    let mut reversed = seats.clone();
    reversed.reverse();
    assert_ne!(
        seats, reversed,
        "ANTI-VACUITY: TWO entries over distinct seats, differing from their own reversal — an \
         empty or one-entry order could not satisfy the equality above"
    );
    assert!(
        order_only.declared.is_none(),
        "CR 732.1b: an until-lethal proposal names no count, so there is no partition to state \
         and any magnitude here would be invented. got {:?}",
        order_only.declared
    );
    // CR 601.2c: the element and the group it is stated over are published together, so a reader
    // asks the group whether an allocation exists instead of inferring it from the element.
    assert_eq!(counted.allocation_group, Some(0));
    assert_eq!(
        order_only.allocation_group, None,
        "no partition is stated, so no announced decision is named as the one it partitions — \
         and the order above is therefore this decision's to state"
    );
}

/// **CR 601.2c — an unstatable announced-target decision costs one statement line, and refuses
/// the sequence only when the skip MOVES the allocation's domain.**
///
/// `allocation_point` names the FIRST published `Targets` point as the one an allocation is
/// stated over, so the question a skip owes is whether some later decision ends up standing in
/// the slot the skipped one would have owned. Five boards answer it:
///
/// * skipped BEHIND a stated decision — the domain is already that decision's, so the responder
///   keeps its partition, its magnitudes and every unrelated statement;
/// * skipped AHEAD of one — the stated decision would inherit a domain the proposer never gave
///   it, so the whole sequence is refused;
/// * skipped with NO other announced-target decision anywhere — there is no domain to move, so
///   the rest of the declaration publishes and states no allocation;
/// * skipped ahead of one AND AGAIN behind it — the second skip moves nothing, so it settles
///   only for itself and leaves the first skip's debt standing;
/// * skipped behind an OPTIONAL decision — a published point that is not an announced-target
///   point leaves the domain open, so this skip is ahead of the allocation like any other.
///
/// All three unstatable shapes are latent from the human ingress — `materialize_loop_shortcut_response`
/// mints one pin per announced-target decision and `decode_sequenced_targets` mints neither a
/// cyclic nor a stepless schedule — so a save or a wire restore is the way in, and the branch is
/// judged on what it does when reached.
///
/// # Discrimination
///
/// Refuse on ANY skip taken before a domain is fixed ⇒ the third board loses its optional
/// statement and its points. Skip unconditionally ⇒ the second board publishes, with an
/// allocation stated over the decision behind the hole. Refuse a LATER skip ⇒ the first board
/// loses its points and its element. Let each skip OVERWRITE what the ones before it recorded
/// rather than accumulating ⇒ the fourth board publishes. Ask whether ANY point is published
/// rather than an ALLOCATING one ⇒ the fifth board publishes.
#[test]
fn an_unstatable_target_decision_refuses_only_when_the_skip_moves_the_domain() {
    use engine::analysis::decision_template::{
        DecisionSlot, MayChoiceOption, PinnedDecision, Ranking, TargetPin, TargetSchedule,
    };
    use engine::types::game_state::YieldTarget;

    const COUNT: u32 = 6;
    const STARTS: [u32; 2] = [0, 2];

    let ranked =
        |seat: PlayerId| Ranking::new(seat_subjects(&[seat])).expect("one seat is a legal ranking");
    let stated =
        |slot: DecisionSlot| piecewise_pin(slot, &STARTS, &seat_subjects(&[R_FIRST, R_SECOND]));
    let optional = || PinnedDecision::MayChoice {
        slot: DecisionSlot::may(YieldTarget::ThisObject {
            source_id: ObjectId(9_401),
            incarnation: Some(1),
            trigger_description: None,
        }),
        take: MayChoiceOption::Take,
    };
    let window = |decisions: Vec<PinnedDecision>| {
        respond_reply_of(&respond_window(
            IterationCount::Fixed(COUNT),
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            decisions,
        ))
    };
    let kinds = |reply: &RespondReply| reply.points.iter().map(|p| p.kind).collect::<Vec<_>>();

    // ── The paired positive, once: the same three decisions all stated. Both legs are compared
    //    against it, and it is also what a wrongly-skipped FIRST decision would publish.
    let whole = window(vec![
        stated(preview_slot(0)),
        PinnedDecision::Targets {
            slot: preview_slot(1),
            targets: vec![TargetPin::Player(R_DRAINED)],
        },
        optional(),
    ]);
    assert_eq!(
        kinds(&whole),
        vec![
            InteractionShortcutPointKind::Targets,
            InteractionShortcutPointKind::Targets,
            InteractionShortcutPointKind::MayChoice,
        ],
        "reach-guard: three statable decisions publish three statement points"
    );
    assert_eq!(
        declared_amounts(
            whole
                .declared
                .as_ref()
                .expect("and the first of them is partitioned")
        ),
        segments_from(&STARTS, COUNT),
        "ANTI-VACUITY: two segments, pairwise distinct — 'everything empty' could not satisfy \
         the comparisons below"
    );

    // ── And the same positive with an optional decision AHEAD of both announced-target ones, so
    //    the last board below cannot refuse for want of a publishable arrangement.
    let optional_first = window(vec![
        optional(),
        stated(preview_slot(0)),
        stated(preview_slot(1)),
    ]);
    assert_eq!(
        kinds(&optional_first),
        vec![
            InteractionShortcutPointKind::MayChoice,
            InteractionShortcutPointKind::Targets,
            InteractionShortcutPointKind::Targets,
        ],
        "reach-guard: an optional decision ahead of two stated ones publishes all three"
    );
    assert_eq!(
        declared_amounts(
            optional_first
                .declared
                .as_ref()
                .expect("and the first announced-target decision is partitioned")
        ),
        segments_from(&STARTS, COUNT),
        "CR 601.2c: the allocation is stated over the first announced-target point, which is not \
         the first point published"
    );

    for (shape, targets) in [
        (
            "a multi-position slot",
            vec![TargetPin::Player(R_FIRST), TargetPin::Player(R_SECOND)],
        ),
        (
            "a cyclic schedule",
            vec![TargetPin::Scheduled(TargetSchedule::RoundRobin(vec![
                ranked(R_FIRST),
                ranked(R_SECOND),
            ]))],
        ),
        (
            // CR 732.2a: a schedule with no step announces no subject at any iteration, so it
            // states no sequence — publishing an empty one would seat a candidate-less point in
            // the domain and cost the declaration its allocation.
            "a stepless schedule",
            vec![TargetPin::Scheduled(TargetSchedule::Piecewise(Vec::new()))],
        ),
    ] {
        let unstatable = |slot: DecisionSlot| PinnedDecision::Targets {
            slot,
            targets: targets.clone(),
        };

        // ── LATER: the domain is already the first decision's, so the skip moves nothing.
        let skipped = window(vec![
            stated(preview_slot(0)),
            unstatable(preview_slot(1)),
            optional(),
        ]);
        assert_eq!(
            kinds(&skipped),
            vec![
                InteractionShortcutPointKind::Targets,
                InteractionShortcutPointKind::MayChoice,
            ],
            "{shape} in a LATER announced-target decision publishes no point, and the decisions \
             beside it publish anyway"
        );
        assert_eq!(
            respond_seats_of(&skipped, &skipped.points[0].candidate_ids),
            vec![R_FIRST.0, R_SECOND.0],
            "{shape}: the surviving decision keeps its own announcement order"
        );
        assert_eq!(
            skipped.declared, whole.declared,
            "CR 601.2c: and its partition and magnitudes — `allocation_point` names the same \
             decision either way, so the skip moves nothing ({shape})"
        );

        // ── FIRST: skipping would hand the domain to the decision behind it, so the whole
        //    sequence is refused. This board is the positive above with its two announced-target
        //    decisions swapped, so a skipping producer publishes here.
        let refused = window(vec![
            unstatable(preview_slot(0)),
            stated(preview_slot(1)),
            optional(),
        ]);
        assert!(
            refused.points.is_empty() && refused.declared.is_none(),
            "{shape} ahead of a stated announced-target decision refuses the whole sequence: \
             skipping it would silently re-domain the allocation onto the decision behind it. \
             got {:?}",
            kinds(&refused)
        );

        // ── NO SUCCESSOR: the same hole with no other announced-target decision anywhere. No
        //    domain exists for the skip to move, so the refusal above is not owed and the
        //    responder keeps every statement the declaration can still make.
        let alone = window(vec![unstatable(preview_slot(0)), optional()]);
        assert_eq!(
            kinds(&alone),
            vec![InteractionShortcutPointKind::MayChoice],
            "{shape} with no announced-target decision behind it moves no domain, so the \
             optional decision beside it publishes rather than being lost with the whole walk"
        );
        assert_eq!(
            alone.points[0].candidate_ids.len(),
            2,
            "and it arrives whole — subject then answer — rather than as a stub"
        );
        assert!(
            alone.declared.is_none(),
            "CR 601.2c: with no announced-target decision published there is no domain to state \
             an allocation over, so the responder is shown no magnitude rather than one \
             attributed to a decision the proposer never made ({shape})"
        );

        // ── FIRST, THEN AGAIN LATER: the second hole is behind the fixed domain and settles for
        //    itself alone; the first one's debt is still owed at the end of the walk.
        let twice = window(vec![
            unstatable(preview_slot(0)),
            stated(preview_slot(1)),
            unstatable(preview_slot(2)),
        ]);
        assert!(
            twice.points.is_empty() && twice.declared.is_none(),
            "{shape} taken both ahead of and behind a stated announced-target decision still \
             refuses: the later hole moves no domain, so it answers only for itself. got {:?}",
            kinds(&twice)
        );

        // ── Its paired positive, ONE decision apart: the same board with the leading hole
        //    stated, so the trailing hole is not what refuses above.
        let trailing = window(vec![
            stated(preview_slot(0)),
            stated(preview_slot(1)),
            unstatable(preview_slot(2)),
        ]);
        assert_eq!(
            kinds(&trailing),
            vec![
                InteractionShortcutPointKind::Targets,
                InteractionShortcutPointKind::Targets,
            ],
            "reach-guard: {shape} in the LAST decision publishes the two beside it"
        );
        assert_eq!(
            trailing.declared, whole.declared,
            "and the partition and magnitudes stay the first decision's ({shape})"
        );

        // ── BEHIND A NON-ALLOCATING POINT: the optional decision ahead of the hole publishes,
        //    so points are no longer empty — but none of them is the one an allocation is stated
        //    over, so the stated decision behind the hole would still inherit its domain.
        let after_optional = window(vec![
            optional(),
            unstatable(preview_slot(0)),
            stated(preview_slot(1)),
        ]);
        assert!(
            after_optional.points.is_empty() && after_optional.declared.is_none(),
            "CR 601.2c: an optional decision publishes no allocation domain, so {shape} taken \
             after one is still ahead of the allocation — the arrangement itself publishes as \
             `optional_first`. got {:?}",
            kinds(&after_optional)
        );
    }
}

/// **CR 601.2c — a pin carrying nothing this projection states publishes no point, and moves no
/// allocation domain.**
///
/// Four pin kinds reach this walk with no statement in them: a `ManaColor` constant the proposer
/// produced, a `ConvokeTaps` pin whose creatures re-bind live each iteration, and the `Mode` and
/// `UnlessBreak` answers this projection has no statement vocabulary for. Each is skipped like
/// every other unstatable decision. Unlike a skipped announced-target decision, a skip here
/// cannot re-domain the allocation — `allocation_point` names the first `Targets` point and none
/// of the four is one — so the rest of the declaration is published unchanged. All four are on
/// the board, two AHEAD of the announced-target decision and two BEHIND it.
///
/// `materialize_loop_shortcut_response` mints all four into a declared template, so a save or a
/// wire restore carrying any of them reaches this walk.
///
/// # Discrimination
///
/// Publish a point from any of the four arms ⇒ the statement list differs, and for a leading pin
/// the allocation moves off the announced-target decision. Refuse from any arm ⇒ the board loses
/// its points and its element.
#[test]
fn a_pin_stating_nothing_publishes_no_point_and_disturbs_nothing_beside_it() {
    use engine::analysis::decision_template::{PinnedDecision, UnlessPaymentOption};
    use engine::types::mana::ManaColor;

    const COUNT: u32 = 6;
    const STARTS: [u32; 2] = [0, 2];

    let window = |decisions: Vec<PinnedDecision>| {
        respond_reply_of(&respond_window(
            IterationCount::Fixed(COUNT),
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            decisions,
        ))
    };
    let stated = || {
        piecewise_pin(
            preview_slot(0),
            &STARTS,
            &seat_subjects(&[R_FIRST, R_SECOND]),
        )
    };

    // ── The paired positive: the announced-target decision on its own.
    let alone = window(vec![stated()]);
    assert_eq!(
        alone.points.len(),
        1,
        "reach-guard: the statable decision publishes its statement point"
    );
    assert_eq!(
        declared_amounts(
            alone
                .declared
                .as_ref()
                .expect("and the declared count is partitioned over it")
        ),
        segments_from(&STARTS, COUNT),
        "ANTI-VACUITY: two segments, pairwise distinct — an empty partition could not \
         satisfy the comparisons below"
    );
    assert_eq!(alone.allocation_group, Some(0));

    // ── The same decision with two statement-less pins on either side of it.
    let sandwiched = window(vec![
        PinnedDecision::ManaColor {
            slot: preview_slot(1),
            color: ManaColor::Blue,
        },
        PinnedDecision::Mode {
            slot: preview_slot(3),
            indices: vec![0],
        },
        stated(),
        PinnedDecision::UnlessBreak {
            slot: preview_slot(4),
            pay: UnlessPaymentOption::Pay,
        },
        PinnedDecision::ConvokeTaps {
            slot: preview_slot(2),
        },
    ]);
    assert_eq!(
        sandwiched.points, alone.points,
        "none of the four publishes a point, so the statement list is the one the \
         announced-target decision produces alone"
    );
    assert_eq!(
        sandwiched.candidates, alone.candidates,
        "and no candidate is minted for a point that was never published"
    );
    assert_eq!(
        sandwiched.declared, alone.declared,
        "CR 601.2c: the partition is stated over the same decision, at the same magnitudes"
    );
    assert_eq!(
        sandwiched.allocation_group, alone.allocation_group,
        "a pin AHEAD of the announced-target decision publishes nothing, so it does not \
         displace the index the allocation is stated over"
    );
}

/// **CR 732.2b — the respond-side projection inherits the single redaction authority.**
///
/// `opportunity_for_slot` reads `filtered_state.waiting_for`, so a template
/// `game::visibility`'s one authority dropped whole is unreachable from the projection by
/// construction. The projection then publishes NO declared sequence — never a partial one, which
/// would state a proposal nobody made.
///
/// REVERT-PROBE: point the decode at the AUTHORITATIVE waiting-for state ⇒ the hidden identity
/// is published and the negative below flips.
#[test]
fn the_respond_side_projection_publishes_nothing_for_a_redacted_declaration() {
    use engine::analysis::decision_template::{PinnedDecision, TargetPin};
    use engine::game::zones::create_object;

    let window = |zone: Zone| {
        let mut state = four_seat_state();
        let card = create_object(
            &mut state,
            CardId(7742),
            P0,
            "Secret Card".to_string(),
            zone,
        );
        let pin = PinnedDecision::Targets {
            slot: preview_slot(0),
            targets: vec![TargetPin::ByIdentity(
                engine::types::game_state::YieldTarget::ThisObject {
                    source_id: card,
                    incarnation: Some(1),
                    trigger_description: None,
                },
            )],
        };
        respond_window_on(
            state,
            IterationCount::Fixed(4),
            Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
            vec![pin],
        )
    };

    // ── MANDATORY PAIRED POSITIVE, first: the same board whose pin names a battlefield
    //    permanent publishes the sequence, so the absence below is a branch rather than a
    //    projection that never happens.
    let visible = respond_reply_of(&window(Zone::Battlefield));
    assert_eq!(
        visible.points.len(),
        1,
        "a viewable identity reaches the responder as a published statement point"
    );
    let element = visible
        .declared
        .as_ref()
        .expect("and its declared element is stated");
    assert_eq!(declared_amounts(element), vec![4]);

    // ── The hostile board: the same pin naming the PROPOSER's hand card.
    let hidden = respond_reply_of(&window(Zone::Hand));
    assert!(
        hidden.points.is_empty() && hidden.declared.is_none(),
        "CR 732.2b: the single authority dropped the template whole, so the projection reading \
         the FILTERED state has nothing to decode — and states nothing rather than a partial \
         sequence"
    );
}

/// **CR 732.2b — the responder's declared sequence is CHARGED on the outbound budget.**
///
/// `InteractionResponseSpec::ShortcutReply` charges its points list, each point's candidate-id
/// list and each id string on the SAME cumulative `OutboundBudget` the offer's own `Shortcut`
/// arm charges its lists on, rather than riding in the zero-cost group.
///
/// # Discrimination
///
/// The two boards differ ONLY in how many optional decisions the declaration answers. At
/// `CHARGED_POINTS` the published candidates alone stay under the ceiling — that is what
/// `ACCEPTED_POINTS` establishes on the identical instrument, since candidates are charged by
/// `bound_outbound_choices` either way — so the refusal can only come from the spec's own legs.
/// Return the variant to the zero-cost group ⇒ the oversized projection is emitted and the
/// `PayloadTooLarge` assertion fails.
#[test]
fn an_oversized_declared_sequence_is_charged_rather_than_emitted() {
    use engine::analysis::decision_template::{DecisionSlot, MayChoiceOption, PinnedDecision};

    /// Below the ceiling with the spec charge applied.
    const ACCEPTED_POINTS: u32 = 1_000;
    /// Above it with the spec charge applied, BELOW it without.
    const CHARGED_POINTS: u32 = 1_500;

    let answered = |count: u32| -> Vec<PinnedDecision> {
        (0..count)
            .map(|index| PinnedDecision::MayChoice {
                slot: DecisionSlot::may(engine::types::game_state::YieldTarget::ThisObject {
                    source_id: ObjectId(u64::from(index) + 9_000),
                    incarnation: Some(1),
                    trigger_description: None,
                }),
                take: MayChoiceOption::Take,
            })
            .collect()
    };

    let accepted = respond_window(
        IterationCount::Fixed(4),
        Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
        answered(ACCEPTED_POINTS),
    );
    let reply = respond_reply_of(&accepted);
    assert_eq!(
        reply.points.len() as u32,
        ACCEPTED_POINTS,
        "positive control: a declaration answering {ACCEPTED_POINTS} optional decisions is \
         published whole, so the refusal below is the budget speaking and not the decoder"
    );

    let charged = respond_window(
        IterationCount::Fixed(4),
        Some(respond_period(&[(R_DRAINED, -5)], Vec::new())),
        answered(CHARGED_POINTS),
    );
    let view = viewer_interaction(&charged, R_RESPONDER);
    assert_eq!(
        view.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        },
        "the declared sequence is CHARGED: growing the answered decisions from \
         {ACCEPTED_POINTS} to {CHARGED_POINTS} crosses the same cumulative ceiling the offer's \
         own lists charge against"
    );
    let [replaced] = view.opportunities.as_slice() else {
        panic!(
            "the over-budget slot is REPLACED by the fail-closed placeholder, not dropped, got \
             {} opportunities",
            view.opportunities.len()
        );
    };
    assert!(
        matches!(
            replaced.response,
            InteractionOpportunityResponse::ExactChoices { ref choices } if choices.is_empty()
        ),
        "failing the budget hands over the empty fail-closed placeholder rather than a \
         truncated declaration — the responder is told the payload could not be stated, never \
         shown a sequence nobody proposed"
    );
}

/// The five legs one `#[serde(default, skip_serializing_if = "Option::is_none")]` carrier owes.
///
/// It is NOT `assert_defaulting_list_carrier` with a different pointer, and the difference is
/// MEASURED rather than assumed: under these attributes an explicit `null` at the pointer
/// deserializes to `None` and is ACCEPTED, while a `Vec` carrier under its own attributes
/// refuses it. So the final leg INVERTS — and it is stated as an equality against the absent
/// case rather than as a bare `is_ok()`, because a deserializer that accepted `null` as some
/// third thing would pass `is_ok()` and fail this.
fn assert_defaulting_option_carrier<T>(pointer: &str, populated: &T, absent: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let populated_json = serde_json::to_value(populated).expect("the carrier serializes");
    assert!(
        populated_json
            .pointer(pointer)
            .is_some_and(|value| !value.is_null()),
        "positive control: a POPULATED `{pointer}` must be emitted, else every absence leg \
         below is satisfied by a serializer that writes no key under any circumstances"
    );
    assert_eq!(
        &serde_json::from_value::<T>(populated_json.clone()).expect("a populated carrier reads"),
        populated,
        "a populated `{pointer}` round-trips unchanged"
    );

    let absent_json = serde_json::to_value(absent).expect("the absent carrier serializes");
    assert!(
        absent_json.pointer(pointer).is_none(),
        "an ABSENT `{pointer}` is omitted from the emitted JSON rather than written as `null`"
    );
    assert_eq!(
        &serde_json::from_value::<T>(absent_json).expect("an absent carrier reads"),
        absent,
        "an ABSENT `{pointer}` deserializes to the empty option"
    );

    let mut null_json = populated_json;
    *null_json
        .pointer_mut(pointer)
        .expect("the populated carrier was just asserted present") = serde_json::Value::Null;
    assert_eq!(
        &serde_json::from_value::<T>(null_json).expect("an explicit `null` reads as the option"),
        absent,
        "an explicit `null` at `{pointer}` deserializes to the SAME value the absent key does — \
         an option is a nullable field, which is exactly where it parts company with a list"
    );
}

/// **The three carriers the respond-side spec gained are ADDITIVE, proven rather than asserted.**
///
/// A `ShortcutReply` serialized before these fields existed still deserializes, and empty or
/// absent ones are omitted from the emitted JSON — so no protocol version constant moves.
///
/// REVERT-PROBES: drop `#[serde(default)]` on any carrier ⇒ its absent-key leg fails; drop
/// `skip_serializing_if` ⇒ its omission leg fails.
#[test]
fn the_respond_side_points_and_declared_default_when_absent_and_are_omitted_when_empty() {
    let point = InteractionShortcutPoint {
        group: 0,
        kind: InteractionShortcutPointKind::Targets,
        min: 0,
        max: 0,
        unique: true,
        ordered: true,
        read_only: true,
        candidate_ids: vec![InteractionChoiceId("k0".to_string())],
    };
    let element = InteractionShortcutPreview {
        count: 3,
        entries: vec![preview_entry(
            InteractionShortcutPreviewFamily::Life,
            Some(P1.0),
            -6,
        )],
        allocation: vec![AmountAssignment {
            choice_id: InteractionChoiceId("k0".to_string()),
            amount: 3,
        }],
    };
    let spec = |points: Vec<InteractionShortcutPoint>,
                declared: Option<InteractionShortcutPreview>,
                allocation_group: Option<u32>| {
        InteractionResponseSpec::ShortcutReply {
            min_iteration: 0,
            max_iteration: 4,
            points,
            declared,
            allocation_group,
            confirm: engine::types::interaction::ConfirmSemantics::Explicit,
        }
    };

    // The shape a save written before the declared-sequence fields existed carries: the two
    // numbers and the confirm semantics, and none of those keys.
    let legacy = serde_json::json!({
        "type": "shortcutReply",
        "data": { "minIteration": 0, "maxIteration": 4, "confirm": "explicit" }
    });
    assert_eq!(
        serde_json::from_value::<InteractionResponseSpec>(legacy)
            .expect("a pre-field payload still deserializes"),
        spec(Vec::new(), None, None),
        "ADDITIVITY: the fields default, so no protocol version constant moves for them"
    );

    assert_defaulting_list_carrier(
        "/data/points",
        &spec(vec![point.clone()], None, None),
        &spec(Vec::new(), None, None),
    );
    assert_defaulting_option_carrier(
        "/data/declared",
        &spec(Vec::new(), Some(element), None),
        &spec(Vec::new(), None, None),
    );
    // A group of ZERO, deliberately: `skip_serializing_if = "Option::is_none"` keys on the
    // option, so a carrier whose only populated fixture is a truthy number cannot tell it from
    // one that skipped on the value.
    assert_defaulting_option_carrier(
        "/data/allocationGroup",
        &spec(Vec::new(), None, Some(0)),
        &spec(Vec::new(), None, None),
    );
}

/// A window whose three count axes are all DISTINCT — the only shape that can separate the
/// `min`, `suggested` and `max` seeds from one another.
///
/// Every offer the engine mints today has `suggested == max` (the bounded producer builds its
/// schema from one number), so a real board cannot tell those two seeds apart. `max_iterations`
/// stays below the engine's own cycle ceiling so the staged window is the one published.
fn separating_window() -> GameState {
    preview_offer(
        IterationCount::Fixed(500),
        999,
        Some(preview_period_delta()),
    )
}

/// CR 732.2a: the picker's window is engine-owned, and the published sample always states its
/// endpoints — so the count the box opens on always has magnitudes, and both ends are readable.
///
/// REVERT-PROBES, one per seed, all on the separating window: drop the `min` seed ⇒ 1 is absent
/// (the stride loop starts at `k = 1`); drop `suggested` ⇒ 500 is absent (`1 + 63k` never lands
/// on it); drop `max` ⇒ 999 is absent (the loop's guard is `< max`).
#[test]
fn the_published_preview_always_states_the_count_window_endpoints() {
    let separating = shortcut_offer_of(&separating_window());
    let InteractionShortcutCountSpec::Fixed {
        min,
        max,
        suggested,
    } = separating.count
    else {
        panic!("a Fixed offer publishes a Fixed window");
    };
    assert_eq!(
        [min, suggested, max]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "reach-guard: the three seeds must be DISTINCT here, or dropping any one of them \
         leaves the published set unchanged and this row cannot fail. window = \
         {min}/{suggested}/{max}"
    );

    let collapsed = shortcut_offer_of(&preview_offer(
        IterationCount::Fixed(1),
        1,
        Some(preview_period_delta()),
    ));
    for offer in [&separating, &collapsed] {
        let InteractionShortcutCountSpec::Fixed {
            min,
            max,
            suggested,
        } = offer.count
        else {
            panic!("a Fixed offer publishes a Fixed window");
        };
        let counts: Vec<u32> = offer.preview.iter().map(|element| element.count).collect();
        for endpoint in [min, suggested, max] {
            assert!(
                counts.contains(&endpoint),
                "CR 732.2a: the window's own {endpoint} must be published; got {counts:?} for \
                 window {min}/{suggested}/{max}"
            );
        }
    }
}

/// CR 732.2a: the sample is a BOUNDED projection of the window — thinned by a stride, capped by
/// element count, and spread across the window rather than clustered at its floor.
///
/// REVERT-PROBES: drop the `counts.len() < MAX_SHORTCUT_PREVIEW_ELEMENTS` guard ⇒ the cap leg
/// fails, the separating window then publishing 18; collapse the stride to 1 ⇒ the spread leg
/// fails and NO other leg moves, because a stride of 1 fills the cap just as well; yield a
/// non-empty sample for `UntilLethal` ⇒ the finite-count leg fails.
#[test]
fn the_published_preview_thins_its_interior_and_stops_at_the_element_cap() {
    let offer = shortcut_offer_of(&separating_window());
    let InteractionShortcutCountSpec::Fixed {
        min,
        max,
        suggested,
    } = offer.count
    else {
        panic!("a Fixed offer publishes a Fixed window");
    };
    let counts: Vec<u32> = offer.preview.iter().map(|element| element.count).collect();

    assert!(
        usize::try_from(max - min).is_ok_and(|span| span + 1 > MAX_SHORTCUT_PREVIEW_ELEMENTS),
        "reach-guard: the UNTHINNED axis {min}..={max} must exceed the cap, or the length leg \
         below is satisfied by a window that fits anyway"
    );
    assert!(
        counts.len() <= MAX_SHORTCUT_PREVIEW_ELEMENTS,
        "the published sample is capped; got {} counts",
        counts.len()
    );
    assert!(
        counts.windows(2).all(|pair| pair[0] < pair[1]),
        "the published counts are strictly increasing, so no count is stated twice: {counts:?}"
    );
    assert!(
        counts
            .iter()
            .any(|count| ![min, suggested, max].contains(count)
                && usize::try_from(count - min)
                    .is_ok_and(|span| span > MAX_SHORTCUT_PREVIEW_ELEMENTS)),
        "the interior sample is SPREAD across the window, not clustered at its floor: a \
         stride of 1 would reach only {} above {min}. got {counts:?}",
        MAX_SHORTCUT_PREVIEW_ELEMENTS
    );

    // ── HOSTILE, the collapsed windows: one element per distinct endpoint, never a duplicate.
    for width in [1u32, 2] {
        let narrow = shortcut_offer_of(&preview_offer(
            IterationCount::Fixed(width),
            width,
            Some(preview_period_delta()),
        ));
        let InteractionShortcutCountSpec::Fixed {
            min: narrow_min,
            max: narrow_max,
            ..
        } = narrow.count
        else {
            panic!("a Fixed offer publishes a Fixed window");
        };
        let published: Vec<u32> = narrow.preview.iter().map(|element| element.count).collect();
        let expected: Vec<u32> = [narrow_min, narrow_max]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(
            published, expected,
            "a window of width {width} publishes each of its endpoints exactly once"
        );
    }

    // ── HOSTILE: `UntilLethal` names no number, so the exhaustive match yields no sample and
    //    the producer publishes nothing — the offer's single finite-count gate.
    assert!(
        shortcut_preview_of(&preview_offer(
            IterationCount::UntilLethal,
            999,
            Some(preview_period_delta()),
        ))
        .is_empty(),
        "`UntilLethal` states no finite count, so no element is published"
    );
}

/// The canonical-allocation property, asserted over EVERY published element of an offer that
/// publishes a `Targets` point with candidates. Read from the same projection under test.
fn assert_canonical_allocation(offer: &ShortcutOffer) {
    let point = offer
        .points
        .iter()
        .find(|point| point.kind == InteractionShortcutPointKind::Targets)
        .expect("reach-guard: this offer must publish a Targets point");
    assert!(
        !point.candidate_ids.is_empty() && !offer.preview.is_empty(),
        "reach-guard: the point must publish candidates and the offer must publish elements, \
         or every leg below is vacuous"
    );
    for element in &offer.preview {
        let parts = usize::try_from(element.count)
            .expect("a published count fits usize")
            .min(point.candidate_ids.len());
        assert_eq!(
            element.allocation.len(),
            parts,
            "the split has one part per allocated candidate, truncated by the count itself"
        );
        assert_eq!(
            element
                .allocation
                .iter()
                .map(|assignment| assignment.choice_id.clone())
                .collect::<Vec<_>>(),
            point.candidate_ids[..parts].to_vec(),
            "CR 601.2c: the ids are the FIRST {parts} of the point's own candidate ids, in the \
             order it published them"
        );
        let amounts: Vec<u32> = element
            .allocation
            .iter()
            .map(|assignment| assignment.amount)
            .collect();
        assert!(
            amounts.iter().all(|amount| *amount >= 1),
            "no part is empty — a zero segment names a cycle no candidate absorbs: {amounts:?}"
        );
        assert!(
            amounts.windows(2).all(|pair| pair[0] >= pair[1]),
            "the remainder lands on the EARLIEST ids, so the amounts are non-increasing: \
             {amounts:?}"
        );
        let spread =
            amounts.iter().max().copied().unwrap_or(0) - amounts.iter().min().copied().unwrap_or(0);
        assert!(spread <= 1, "the split is even to within one: {amounts:?}");
        assert_eq!(
            amounts.iter().sum::<u32>(),
            element.count,
            "the split covers the element's whole count: {amounts:?}"
        );
    }
}

/// CR 601.2c + CR 732.2a: every published element states the canonical even split of ITS OWN
/// count over the offer's announced candidates.
///
/// REVERT-PROBE: derive the choice id from a seat index instead of the offer's own
/// `interaction_choice_id(.., 'k', ..)` ⇒ the membership leg fails wherever the allocated
/// point is not the first one published.
#[test]
fn every_published_element_states_the_canonical_split_of_its_own_count() {
    let seats = [P0, P1, PlayerId(2), PlayerId(3)];
    // The window reaches ABOVE the candidate count so both hostile shapes are published: counts
    // below four exercise the truncation, counts above it that four does not divide exercise
    // the remainder.
    let offer = shortcut_offer_of(&preview_offer_with_points(
        IterationCount::Fixed(6),
        6,
        Some(preview_period_delta()),
        vec![player_targets_point(0, &seats)],
        Vec::new(),
    ));
    let candidates = offer
        .points
        .iter()
        .find(|point| point.kind == InteractionShortcutPointKind::Targets)
        .map(|point| point.candidate_ids.len())
        .expect("the offer publishes a Targets point");

    // ── REACH-GUARDS: the published sample must actually reach each hostile shape, or the
    //    property below holds only over the easy elements.
    assert!(
        offer
            .preview
            .iter()
            .any(|element| usize::try_from(element.count).is_ok_and(|count| count < candidates)),
        "reach-guard: an element BELOW the candidate count must be published, or the \
         truncation is never exercised"
    );
    assert!(
        offer.preview.iter().any(|element| {
            usize::try_from(element.count)
                .is_ok_and(|count| count > candidates && count % candidates != 0)
        }),
        "reach-guard: an element whose count does NOT divide the candidate count must be \
         published, or the remainder distribution is never exercised"
    );
    assert_canonical_allocation(&offer);

    // ── The count-1 element names exactly the FIRST published candidate.
    let point = offer
        .points
        .iter()
        .find(|point| point.kind == InteractionShortcutPointKind::Targets)
        .expect("the offer publishes a Targets point");
    let single = element_at(&offer.preview, 1).expect("the window's floor is published");
    assert_eq!(
        single
            .allocation
            .iter()
            .map(|assignment| &assignment.choice_id)
            .collect::<Vec<_>>(),
        vec![&point.candidate_ids[0]],
    );

    // ── A single-legal-victim offer: one part per element, never an empty allocation
    //    masquerading as "no split".
    let lone = shortcut_offer_of(&preview_offer_with_points(
        IterationCount::Fixed(3),
        4,
        Some(preview_period_delta()),
        vec![player_targets_point(0, &[P1])],
        Vec::new(),
    ));
    assert_canonical_allocation(&lone);
    for element in &lone.preview {
        assert_eq!(
            element.allocation.len(),
            1,
            "one legal victim absorbs the whole count"
        );
        assert_eq!(element.allocation[0].amount, element.count);
    }

    // ── HOSTILE: TWO `Targets` points. The allocation's domain is the FIRST in published order.
    let paired = shortcut_offer_of(&preview_offer_with_points(
        IterationCount::Fixed(3),
        4,
        Some(preview_period_delta()),
        vec![
            player_targets_point(0, &[P0, P1]),
            player_targets_point(1, &[PlayerId(2), PlayerId(3)]),
        ],
        Vec::new(),
    ));
    let later: Vec<InteractionChoiceId> = paired
        .points
        .iter()
        .filter(|point| point.kind == InteractionShortcutPointKind::Targets)
        .skip(1)
        .flat_map(|point| point.candidate_ids.clone())
        .collect();
    assert!(
        !later.is_empty(),
        "reach-guard: the second Targets point must publish ids of its own, or 'the first \
         point' is not a claim about anything"
    );
    assert_canonical_allocation(&paired);
    for element in &paired.preview {
        assert!(
            element
                .allocation
                .iter()
                .all(|assignment| !later.contains(&assignment.choice_id)),
            "no id from a LATER Targets point may appear: {:?}",
            element.allocation
        );
    }
}

/// CR 601.2c: `allocation` is empty if and only if the FIRST `Targets` point in published order
/// holds no candidate. The qualifier is the code's behaviour, not a hedge: the ids come from
/// that one point's `candidate_indices`, and a candidate-less point mints none — a later point
/// holding candidates does not fill the gap, because moving the domain there would state the
/// split over choices the reader cannot identify.
///
/// REVERT-PROBES: key emptiness on `points.is_empty()` ⇒ the may-choice leg fails; divide before
/// the empty-ids return in the generator ⇒ the candidate-less leg panics on a division by zero;
/// drop BOTH the announced-seat conjunct of the charge and the split's non-empty filter ⇒ the
/// candidate-less leg's charged `Life` entry silently disappears (that leg takes two drops
/// because either refusal alone still holds the entry); make `allocation_point` skip a
/// candidate-less point to reach a later one ⇒ the trailing leg publishes a split.
#[test]
fn the_allocation_is_empty_exactly_when_the_first_targets_point_holds_no_candidate() {
    // ── PAIRED POSITIVE, first: a Targets point with candidates publishes a split everywhere.
    let allocated = shortcut_offer_of(&preview_offer_with_points(
        IterationCount::Fixed(3),
        4,
        Some(preview_period_delta()),
        vec![player_targets_point(0, &[P1, PlayerId(2)])],
        Vec::new(),
    ));
    assert!(
        !allocated.preview.is_empty()
            && allocated
                .preview
                .iter()
                .all(|element| !element.allocation.is_empty()),
        "control: every element of an offer with announced candidates carries a split"
    );

    // ── HOSTILE: points, but none of them announces targets. A point-free offer cannot
    //    separate this from "no points at all".
    let may_only = shortcut_offer_of(&preview_offer_with_points(
        IterationCount::Fixed(3),
        4,
        Some(preview_period_delta()),
        vec![DecisionPoint {
            slot: preview_slot(0),
            kind: DecisionPointKind::MayChoice,
        }],
        Vec::new(),
    ));
    assert!(
        !may_only.points.is_empty()
            && !may_only
                .points
                .iter()
                .any(|point| point.kind == InteractionShortcutPointKind::Targets),
        "reach-guard: the offer must publish points and none of them a Targets point"
    );
    assert!(
        !may_only.preview.is_empty()
            && may_only
                .preview
                .iter()
                .all(|element| element.allocation.is_empty()),
        "no announced target, no split — while elements are still published"
    );

    // ── HOSTILE, the member the qualifier admits: a `Targets` point with no candidates, whose
    //    slot the period nonetheless charges. The charge resolves; only the split is missing.
    let rate = 3i64;
    let mut charged = engine::analysis::resource::ResourceVector::default();
    charged.life.insert(P1, -rate);
    let empty_point = shortcut_offer_of(&preview_offer_with_points(
        IterationCount::Fixed(3),
        4,
        Some(charged),
        vec![player_targets_point(0, &[])],
        vec![(preview_slot(0), rate)],
    ));
    let point = empty_point
        .points
        .iter()
        .find(|point| point.kind == InteractionShortcutPointKind::Targets)
        .expect("reach-guard: the candidate-less Targets point is ADMITTED and published");
    assert!(
        point.candidate_ids.is_empty(),
        "reach-guard: the admitted point publishes no candidate id"
    );
    assert!(
        !empty_point.preview.is_empty(),
        "reach-guard: elements are published"
    );
    for element in &empty_point.preview {
        assert!(
            element.allocation.is_empty(),
            "a Targets point with no candidate announces nothing to split over"
        );
        assert!(
            element.entries.contains(&preview_entry(
                InteractionShortcutPreviewFamily::Life,
                Some(P1.0),
                i32::try_from(-rate * i64::from(element.count)).unwrap(),
            )),
            "a split with no parts leaves the charged seat's own magnitude standing: {:?}",
            element.entries
        );
    }

    // ── HOSTILE, the member an unqualified "no Targets point holds a candidate" would refuse:
    //    a candidate-less FIRST point followed by one that DOES hold candidates. A Targets
    //    point holding a candidate exists, and the allocation is still empty.
    let later_candidates = shortcut_offer_of(&preview_offer_with_points(
        IterationCount::Fixed(3),
        4,
        Some(preview_period_delta()),
        vec![
            player_targets_point(0, &[]),
            player_targets_point(1, &[P1, PlayerId(2)]),
        ],
        Vec::new(),
    ));
    assert!(
        later_candidates
            .points
            .iter()
            .filter(|point| point.kind == InteractionShortcutPointKind::Targets)
            .any(|point| !point.candidate_ids.is_empty()),
        "reach-guard: a Targets point holding candidates IS published, or this leg does not \
         separate the qualified claim from the unqualified one"
    );
    assert!(
        !later_candidates.preview.is_empty()
            && later_candidates
                .preview
                .iter()
                .all(|element| element.allocation.is_empty()),
        "the domain is the FIRST Targets point or nothing: a later point's candidates do not \
         fill an allocation the first point mints no ids for"
    );
}

/// CR 119.3: the published life magnitudes follow the allocation when — and only when — the
/// period's life map names exactly one losing seat that the declaration itself announces and
/// the announced charge is the whole of that seat's loss, which is what makes the charge
/// positive.
///
/// The announced magnitude is an aggregate over every seat, so on any other shape it names the
/// worst-off seat rather than this slot's victim. The ambiguous, uneven and unannounced-loser
/// legs carry the magnitude production would announce for their own life map; the
/// negative-magnitude, undercharge and unnegatable legs stage a charge DECOUPLED from the
/// period, which production does not emit but the type admits and `WaitingFor` carries across
/// the persistence boundary.
///
/// REVERT-PROBES: fold with no split at all ⇒ the positive leg publishes one `Life` seat where
/// the allocation names several; take the FIRST losing seat instead of requiring exactly one ⇒
/// the ambiguous leg re-attributes a tied seat; pick the seat BY the announced magnitude
/// instead of by "exactly one loser" ⇒ the uneven leg re-attributes the worst-off seat; drop
/// the announced-seat requirement ⇒ the unannounced-loser leg erases that seat and charges
/// announced seats that lose nothing; drop the equality with the losing seat's own loss ⇒ the
/// negative-magnitude leg spreads a GAIN across the allocated seats and, behind it, the
/// undercharge leg's `Life` magnitudes total the charge times the count where the period takes
/// three times that; respell that equality as one against the NEGATED charge ⇒ the unnegatable
/// leg overflows where the addition refuses.
#[test]
fn the_preview_spreads_a_charged_life_magnitude_only_over_an_unambiguous_positive_charge() {
    let seats = [P1, PlayerId(2), PlayerId(3)];
    let period = |life: Vec<(PlayerId, i64)>| {
        let mut delta = engine::analysis::resource::ResourceVector::default();
        for (seat, magnitude) in life {
            delta.life.insert(seat, magnitude);
        }
        // A seat-keyed axis the slot charge does NOT attribute — the paired control that must
        // stay on the seat `payload_seat` gave it.
        delta.library_delta.insert(P0, -1);
        delta
    };
    let offer_at = |life: Vec<(PlayerId, i64)>, charge: i64| {
        shortcut_offer_of(&preview_offer_with_points(
            IterationCount::Fixed(3),
            4,
            Some(period(life)),
            vec![player_targets_point(0, &seats)],
            vec![(preview_slot(0), charge)],
        ))
    };
    // The magnitude production announces for a charged slot, derived here the way
    // `ResourceVector::worst_seat_life_loss` derives it rather than hand-set: the worst single
    // seat's loss over the whole period, clamped at zero, and the SAME number on every slot.
    let announced = |life: &[(PlayerId, i64)]| {
        life.iter()
            .map(|(_, magnitude)| (-magnitude).max(0))
            .max()
            .unwrap_or(0)
    };
    let life_entries = |element: &InteractionShortcutPreview| {
        let mut published: Vec<(Option<u8>, i32)> = element
            .entries
            .iter()
            .filter(|entry| entry.family == InteractionShortcutPreviewFamily::Life)
            .map(|entry| (entry.player, entry.amount))
            .collect();
        published.sort_unstable();
        published
    };

    // ── THE CHARGE RESOLVES: rate 3, one matching seat, three announced candidates.
    let rate = 3i64;
    let spread = offer_at(vec![(P1, -rate)], rate);
    assert!(
        spread
            .preview
            .iter()
            .any(|element| element.allocation.len() > 1),
        "reach-guard: some element must allocate over MORE THAN ONE candidate, or a \
         seat-per-part claim is satisfied by a one-part split"
    );
    assert!(
        spread.preview.iter().any(|element| {
            u32::try_from(element.allocation.len())
                .is_ok_and(|parts| parts > 0 && element.count % parts != 0)
        }),
        "reach-guard: some element's count must NOT divide its part count, or a split that \
         dropped its remainder still totals correctly"
    );
    assert!(
        spread.preview.len() > 1,
        "reach-guard: more than one count must be published, or a producer that ignores \
         `count` entirely passes"
    );
    for element in &spread.preview {
        let mut expected: Vec<(Option<u8>, i32)> = seats
            .iter()
            .zip(element.allocation.iter())
            .map(|(seat, assignment)| {
                (
                    Some(seat.0),
                    i32::try_from(-rate * i64::from(assignment.amount)).unwrap(),
                )
            })
            .collect();
        expected.sort_unstable();
        assert_eq!(
            life_entries(element),
            expected,
            "CR 119.3: each allocated candidate is charged the rate times ITS OWN share of \
             count {}",
            element.count
        );
        assert_eq!(
            life_entries(element)
                .iter()
                .map(|(_, amount)| i64::from(*amount))
                .sum::<i64>(),
            -rate * i64::from(element.count),
            "the split is exact: the seats together absorb the whole count"
        );
        assert!(
            element.entries.contains(&preview_entry(
                InteractionShortcutPreviewFamily::Mill,
                Some(P0.0),
                i32::try_from(-i64::from(element.count)).unwrap(),
            )),
            "PAIRED CONTROL: an axis the slot charge does not attribute keeps its own seat \
             and its own unscaled magnitude: {:?}",
            element.entries
        );
    }

    // ── HOSTILE: the charged magnitude is matched by TWO seats, so it names none of them.
    let ambiguous = offer_at(vec![(P1, -1), (PlayerId(2), -1)], 1);
    for element in &ambiguous.preview {
        assert!(
            !element.allocation.is_empty(),
            "the declaration is still published — the allocation is its shape, not a \
             magnitude claim"
        );
        assert_eq!(
            life_entries(element),
            vec![
                (Some(P1.0), -i32::try_from(element.count).unwrap()),
                (Some(PlayerId(2).0), -i32::try_from(element.count).unwrap()),
            ],
            "an ambiguous charge is refused, so the entries are the raw per-seat fold and no \
             third seat appears"
        );
    }

    // ── HOSTILE: two seats lose UNEQUAL amounts, so the announced aggregate is the WORST
    //    seat's loss and names no slot's own victim. P0 loses too and is not announced.
    let uneven = vec![(P0, -1i64), (P1, -3i64)];
    let worst = announced(&uneven);
    assert_eq!(
        uneven
            .iter()
            .filter(|(seat, magnitude)| *magnitude == -worst && seats.contains(seat))
            .count(),
        1,
        "reach-guard: the announced aggregate is matched by exactly one seat and that seat is \
         announced, so a victim picked BY that magnitude would resolve here"
    );
    let uneven_offer = offer_at(uneven, worst);
    for element in &uneven_offer.preview {
        assert!(
            !element.allocation.is_empty(),
            "the declaration is still published — the allocation is its shape, not a \
             magnitude claim"
        );
        assert_eq!(
            life_entries(element),
            vec![
                (Some(P0.0), -i32::try_from(element.count).unwrap()),
                (Some(P1.0), -3 * i32::try_from(element.count).unwrap()),
            ],
            "an aggregate over two losing seats charges neither of them, so the entries are \
             the raw per-seat fold and each seat keeps its own rate"
        );
    }

    // ── HOSTILE: the only losing seat is one the declaration does not announce.
    let outside = vec![(P0, -3i64)];
    assert!(
        !seats.contains(&P0) && announced(&outside) > 0,
        "reach-guard: the period charges a positive magnitude and names exactly one loser, so \
         only that seat's absence from the announced candidates refuses the split"
    );
    let outside_offer = offer_at(outside.clone(), announced(&outside));
    for element in &outside_offer.preview {
        assert!(!element.allocation.is_empty());
        assert_eq!(
            life_entries(element),
            vec![(Some(P0.0), -3 * i32::try_from(element.count).unwrap())],
            "the seat that actually loses keeps its whole magnitude, and no announced seat is \
             charged a loss it does not take"
        );
    }

    // ── HOSTILE: the announced magnitude is a GAIN, which no losing seat's loss can balance.
    let gaining_period = vec![(P0, 2i64), (P1, -2i64)];
    assert_eq!(
        gaining_period
            .iter()
            .filter(|(seat, magnitude)| *magnitude < 0 && seats.contains(seat))
            .count(),
        1,
        "reach-guard: exactly one announced seat loses life and that seat is announced, so \
         this period reaches the charge/loss equality and only that equality refuses it"
    );
    let gaining = offer_at(gaining_period, -2);
    for element in &gaining.preview {
        assert!(!element.allocation.is_empty());
        assert_eq!(
            life_entries(element),
            vec![
                (Some(P0.0), 2 * i32::try_from(element.count).unwrap()),
                (Some(P1.0), -2 * i32::try_from(element.count).unwrap()),
            ],
            "an announced GAIN can never be a losing seat's own loss, so it is refused \
             rather than spread over the announced candidates"
        );
    }

    // ── HOSTILE: exactly one losing seat, that seat announced, and the charge positive —
    //    every conjunct but the equality holds — but the charge is SMALLER than the seat's own
    //    per-period loss. The fold re-states the charged seat by dropping its whole `Life`
    //    axis and re-adding the charge once per allocated cycle, so a split here publishes a
    //    shallower drain than the count actually runs.
    let seat_loss = 3i64;
    let undercharge = 1i64;
    let coupled = offer_at(vec![(P1, -seat_loss)], seat_loss);
    assert!(
        coupled.preview.iter().any(|element| {
            element
                .entries
                .iter()
                .filter(|entry| entry.family == InteractionShortcutPreviewFamily::Life)
                .count()
                > 1
        }),
        "PAIRED CONTROL: the SAME life map charged its own loss DOES spread over several \
         seats, so the refusal below is the charge's doing and not an unreachable arm"
    );
    let shallow = offer_at(vec![(P1, -seat_loss)], undercharge);
    for element in &shallow.preview {
        assert!(
            !element.allocation.is_empty(),
            "the declaration is still published — the allocation is its shape, not a \
             magnitude claim"
        );
        assert_eq!(
            life_entries(element),
            vec![(
                Some(P1.0),
                -i32::try_from(seat_loss * i64::from(element.count)).unwrap()
            )],
            "an undercharge is refused, so the only losing seat keeps its whole per-period loss"
        );
        assert_eq!(
            life_entries(element)
                .iter()
                .map(|(_, amount)| i64::from(*amount))
                .sum::<i64>(),
            -seat_loss * i64::from(element.count),
            "CR 119.3: the published life magnitudes total the period the count runs — a \
             split at the announced charge states {undercharge} per cycle where the period \
             takes {seat_loss}"
        );
    }

    // ── HOSTILE: a charge no magnitude can negate. `i64::MIN` reaches the same equality
    //    every leg above ends at, and the addition answers over the whole of `i64` where a
    //    negation would overflow.
    let unnegatable = offer_at(vec![(P1, -seat_loss)], i64::MIN);
    for element in &unnegatable.preview {
        assert_eq!(
            life_entries(element),
            vec![(
                Some(P1.0),
                -i32::try_from(seat_loss * i64::from(element.count)).unwrap()
            )],
            "a charge that cannot be balanced is refused, so the losing seat keeps its whole \
             per-period loss"
        );
    }
}

#[test]
fn loop_shortcut_schema_and_materializer_cover_every_decision_point_kind() {
    let mut scenario = GameScenario::new();
    let target = scenario
        .add_creature(P0, "Shortcut Contract Target", 1, 1)
        .id();
    let mut runner = scenario.build();
    let source = engine::types::game_state::YieldTarget::AllCopies {
        card_id: CardId(9001),
        trigger_description: None,
    };
    let slot = |index| DecisionSlot {
        source: source.clone(),
        index,
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: engine::analysis::loop_check::LoopCertificate {
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::Advantage,
            mandatory: false,
            residual_board_delta: engine::analysis::resource::BoardDelta::default(),
            per_cycle: None,
        },
        schema: ShortcutDecisionSchema {
            iteration_count: IterationCount::Fixed(2),
            // No narrowed CR 732.2a bound — `Default` carries the global cap.
            max_iterations: ShortcutDecisionSchema::default().max_iterations,
            points: vec![
                DecisionPoint {
                    slot: slot(0),
                    kind: DecisionPointKind::Targets {
                        legal_targets: vec![TargetRef::Object(target), TargetRef::Player(P1)],
                        min_targets: 1,
                        max_targets: 2,
                        ordered: true,
                    },
                },
                DecisionPoint {
                    slot: slot(1),
                    kind: DecisionPointKind::ConvokeTaps {
                        tappable: vec![target],
                    },
                },
                DecisionPoint {
                    slot: slot(2),
                    kind: DecisionPointKind::Mode {
                        available_modes: vec![0, 2],
                        min_modes: 1,
                        max_modes: 2,
                        allow_repeats: false,
                    },
                },
                DecisionPoint {
                    slot: slot(3),
                    kind: DecisionPointKind::MayChoice,
                },
                DecisionPoint {
                    slot: slot(4),
                    kind: DecisionPointKind::UnlessBreak,
                },
                DecisionPoint {
                    slot: slot(5),
                    kind: DecisionPointKind::ManaColor {
                        color: ManaColor::Blue,
                    },
                },
            ],
            convoke_tappable_count: 1,
        },
        declaration: None,
    };
    bind(runner.state_mut(), "loop-point-kinds");

    let view = priority_view(runner.state());
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { points, .. },
        candidates,
    } = &view.opportunities[0].response
    else {
        panic!("loop shortcut uses a shortcut schema");
    };
    assert_eq!(
        points.iter().map(|point| point.kind).collect::<Vec<_>>(),
        vec![
            InteractionShortcutPointKind::Targets,
            InteractionShortcutPointKind::ConvokeTaps,
            InteractionShortcutPointKind::Mode,
            InteractionShortcutPointKind::MayChoice,
            InteractionShortcutPointKind::UnlessBreak,
            InteractionShortcutPointKind::ManaColor,
        ]
    );
    assert_eq!(
        (points[0].min, points[0].max, points[0].ordered),
        (1, 2, true)
    );
    assert!(points[1].read_only);
    assert!(points[5].read_only);
    assert_eq!(candidates.len(), 10);

    let selected_pins = [0usize, 2, 3, 4]
        .into_iter()
        .map(|group| InteractionShortcutPin {
            group: group as u32,
            choice_ids: vec![points[group].candidate_ids[0].clone()],
            amounts: Vec::new(),
        })
        .collect::<Vec<_>>();
    let valid = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("loop-points-valid".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::AcceptSuggested,
                pins: selected_pins.clone(),
            },
        },
    );
    assert_eq!(valid.status, InteractionPreviewStatus::Confirmable);

    let mut invalid_pins = selected_pins;
    invalid_pins[0].choice_ids[0] = InteractionChoiceId("not-an-offered-target".to_string());
    let invalid = preview_interaction(
        runner.state(),
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("loop-points-invalid".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::AcceptSuggested,
                pins: invalid_pins,
            },
        },
    );
    assert_eq!(
        invalid.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::UnknownChoice,
        }
    );
}

/// **Row R2-f — the HUMAN ingress emits the same spelling as the engine's own producer.**
///
/// CR 601.2c: one shape per point kind, whoever submitted it. `materialize_loop_shortcut_response`
/// decodes a submitted player candidate on a `Targets` point into
/// `Scheduled(Constant(Ranking::one(AnnouncementSubject::Seat(..))))` — the same value
/// `game::engine::record_trigger_target_answer` journals for an announced seat — and an OBJECT
/// candidate on the SAME point into `TargetPin::ByIdentity`, unchanged.
///
/// # Discrimination
///
/// Migrate only the engine's producer and leave this decoder emitting `TargetPin::Player(*player)`
/// ⇒ one `Targets` point yields two different pin spellings depending on WHO submitted the
/// answer, and the seat assertion below FAILS while the object assertion stays green. That
/// asymmetry — one arm moving, one not — is what makes this a spelling row rather than a
/// smoke test.
///
/// # Paired positive reach-guard
///
/// The decoder must still ACCEPT end to end: `resolve_interaction_response` returns
/// `Ok(GameAction::DeclareShortcut { .. })`, which means `declaration_conforms` ran
/// `predictability_gate` and `validate_pins` at range 1 and passed. Without it the row would be
/// satisfied by a decoder that had simply started refusing everything.
///
/// # ⚠ WHY THIS ROW BUILDS ITS OWN BOARD (measured, not preference)
///
/// The file's other shortcut rows share a schema whose only slot source is
/// `AllCopies { CardId(9001) }`, which no battlefield object carries. After the split a `Seat`
/// pin on such a slot resolves through `resolve_ability_instance` ⇒ `resolve_source`'s
/// `AllCopies` arm (`.filter(|o| o.zone == Zone::Battlefield && o.card_id == *card_id)`) ⇒
/// `None` ⇒ `IllegalTarget` ⇒ `validate_pins` ⇒ `declaration_conforms == false` ⇒
/// `ConstraintUnsatisfied`. The positive reach-guard above would be UNSATISFIABLE there, and the
/// cheapest-looking repair would be to loosen a fail-closed predicate. So the slot source here is
/// a `ThisObject` naming a live battlefield creature, at that object's LIVE incarnation read from
/// state (CR 400.7) — never a hard-coded one. `AllCopies` cannot take the CR 114.4 / CR 113.6p
/// command-zone disjunct either: that disjunct is `ThisObject`-only, so a command-zone source
/// named by CARD identity (a conspiracy, an Eminence commander — both of which DO have cards)
/// still resolves `None` and fails closed. Measured residual, disclosed rather than closed.
///
/// The three shipped `Shortcut` rows in this file are untouched by the split, but by INDEX
/// ORDERING rather than by design: the file has exactly one candidate-selection site and it takes
/// `candidate_ids[0]`, which on the one board offering both is the OBJECT. That vector must not
/// be reordered.
///
/// This row's own board deliberately exercises BOTH indices, and the two arms key each other: if
/// the projection's candidate order did not follow `legal_targets`, both assertions would fail
/// rather than one silently passing on the wrong candidate.
#[test]
fn loop_shortcut_human_ingress_emits_the_target_class_spelling_for_a_submitted_seat() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, PinnedDecision, Ranking, TargetPin, TargetSchedule,
    };

    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P0, "R2f Ability Source", 1, 1).id();
    let mut runner = scenario.build();
    let incarnation = runner.state().objects[&target].incarnation;
    let slot = DecisionSlot {
        source: engine::types::game_state::YieldTarget::ThisObject {
            source_id: target,
            incarnation: Some(incarnation),
            trigger_description: None,
        },
        index: 0,
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: engine::analysis::loop_check::LoopCertificate {
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::Advantage,
            mandatory: false,
            residual_board_delta: engine::analysis::resource::BoardDelta::default(),
            per_cycle: None,
        },
        schema: ShortcutDecisionSchema {
            iteration_count: IterationCount::Fixed(2),
            max_iterations: ShortcutDecisionSchema::default().max_iterations,
            points: vec![DecisionPoint {
                slot: slot.clone(),
                kind: DecisionPointKind::Targets {
                    // Index 0 is the OBJECT, index 1 is the SEAT. Both are exercised below.
                    legal_targets: vec![TargetRef::Object(target), TargetRef::Player(P1)],
                    min_targets: 1,
                    max_targets: 1,
                    ordered: true,
                },
            }],
            convoke_tappable_count: 0,
        },
        declaration: None,
    };
    bind(runner.state_mut(), "r2f-human-seat-pin");

    let view = priority_view(runner.state());
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { points, .. },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("the loop shortcut offer uses a shortcut schema");
    };
    assert_eq!(
        points.len(),
        1,
        "reach-guard: exactly one published point, so the pin below addresses the point this \
         row is about"
    );
    assert_eq!(
        points[0].candidate_ids.len(),
        2,
        "reach-guard: BOTH legal targets must be offered as candidates, else one of the two \
         arms below is unreachable"
    );

    let decode = |candidate: usize| {
        resolve_interaction_response(
            runner.state(),
            P0,
            &InteractionSubmission {
                interaction_id: view.opportunities[0].interaction_id.clone(),
                response: InteractionResponse::Shortcut {
                    decision: InteractionShortcutDecision::AcceptSuggested,
                    pins: vec![InteractionShortcutPin {
                        group: 0,
                        choice_ids: vec![points[0].candidate_ids[candidate].clone()],
                        amounts: Vec::new(),
                    }],
                },
            },
        )
    };

    // ── THE CLAIM: a submitted SEAT decodes to the CR 601.2c TARGET-class spelling ──
    let GameAction::DeclareShortcut {
        template: Some(seat_template),
        ..
    } = decode(1).expect(
        "paired positive: the human ingress still ACCEPTS end to end — `declaration_conforms` \
         ran `predictability_gate` and `validate_pins` at range 1 and passed",
    )
    else {
        panic!("a shortcut acceptance carrying pins materializes a template");
    };
    assert_eq!(
        seat_template.decisions,
        vec![PinnedDecision::Targets {
            slot: slot.clone(),
            targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                Ranking::one(AnnouncementSubject::Seat(P1))
            ))],
        }],
        "CR 601.2c: a candidate on a `Targets` point is an ANNOUNCED target, so a submitted \
         seat takes the TARGET-class spelling — the same value the engine's own producer \
         journals. `TargetPin::Player(P1)` here would select the authority by WHO SUBMITTED \
         the answer rather than by WHAT IT IS"
    );

    // ── THE SIBLING: an OBJECT candidate on the SAME point is unchanged ──
    let GameAction::DeclareShortcut {
        template: Some(object_template),
        ..
    } = decode(0).expect("the object candidate is accepted on the same point")
    else {
        panic!("a shortcut acceptance carrying pins materializes a template");
    };
    assert_eq!(
        object_template.decisions,
        vec![PinnedDecision::Targets {
            slot,
            targets: vec![TargetPin::ByIdentity(
                engine::types::game_state::YieldTarget::ThisObject {
                    source_id: target,
                    incarnation: Some(incarnation),
                    trigger_description: None,
                }
            )],
        }],
        "the migration re-spelled the SEAT branch only: an object candidate still binds by \
         CR 400.7 identity"
    );
}

#[test]
fn coin_flip_sequence_supports_multi_keep_and_rejects_duplicates() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::CoinFlipKeepChoice {
        player: P0,
        results: vec![true, false, true, false],
        keep_count: 2,
    };
    bind(&mut state, "coin-multi-keep");
    let view = priority_view(&state);
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Sequence { min, max, .. },
        candidates,
    } = &view.opportunities[0].response
    else {
        panic!("coin flips use a sequence schema");
    };
    assert_eq!((*min, *max, candidates.len()), (2, 2, 4));

    let valid = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("coin-valid".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Sequence {
                choice_ids: vec![candidates[3].id.clone(), candidates[1].id.clone()],
            },
        },
    );
    assert_eq!(
        valid.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ReducerRejected,
        },
        "the multi-keep response materializes before the synthetic state's missing frame rejects"
    );
    let duplicate = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("coin-duplicate".to_string()),
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Sequence {
                choice_ids: vec![candidates[0].id.clone(), candidates[0].id.clone()],
            },
        },
    );
    assert_eq!(
        duplicate.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ConstraintUnsatisfied,
        }
    );
}

#[test]
fn untap_choice_direct_authority_includes_accept_and_decline() {
    let mut scenario = GameScenario::new();
    let permanent = scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&permanent)
        .unwrap()
        .tapped = true;
    runner.state_mut().waiting_for = WaitingFor::UntapChoice {
        player: P0,
        candidates: vec![permanent],
        chosen_not_to_untap: Vec::new(),
    };
    bind(runner.state_mut(), "untap-both");
    let view = priority_view(runner.state());
    let InteractionOpportunityResponse::ExactChoices { choices } = &view.opportunities[0].response
    else {
        panic!("untap is a complete direct choice set");
    };
    assert_eq!(choices.len(), 2);
    for choice in choices {
        let preview = preview_interaction(
            runner.state(),
            P0,
            &InteractionPreviewRequest {
                request_id: PreviewRequestId(format!("untap-{}", choice.id.as_str())),
                interaction_id: view.opportunities[0].interaction_id.clone(),
                response: InteractionResponse::Choose {
                    choice_id: choice.id.clone(),
                },
            },
        );
        assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    }
}

#[test]
fn resolution_optional_payment_ai_candidates_are_exact() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::ResolutionOptionalPaymentChoice {
        player: P0,
        source_id: ObjectId(7),
        costs: vec![
            ResolutionOptionalPaymentOption {
                index: 0,
                cost: AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
            },
            ResolutionOptionalPaymentOption {
                index: 2,
                cost: AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
            },
        ],
    };

    let actions: Vec<_> = engine::ai_support::candidate_actions(&state)
        .into_iter()
        .map(|candidate| candidate.action)
        .collect();
    assert_eq!(
        actions,
        vec![
            GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Decline,
            },
            GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
            },
            GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index: 2 },
            },
        ]
    );
    assert!(engine::ai_support::legal_actions_for_viewer(&state, P1)
        .0
        .is_empty());
}

#[test]
fn recursive_outbound_budget_counts_nested_choice_surfaces() {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::OrderTriggers {
        player: P0,
        triggers: (0..3_500)
            .map(|index| PendingTriggerSummary {
                source_id: engine::types::identifiers::ObjectId(index + 1),
                source_name: "source".to_string(),
                description: "trigger".to_string(),
            })
            .collect(),
    };
    bind(&mut state, "nested-budget");
    let view = priority_view(&state);
    assert_eq!(
        view.availability,
        InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::PayloadTooLarge,
        }
    );
    assert!(matches!(
        &view.opportunities[0].response,
        InteractionOpportunityResponse::ExactChoices { choices } if choices.is_empty()
    ));
}

#[test]
fn generated_contract_and_projection_source_exclude_unstable_internal_strings() {
    let generated = include_str!("../../../../client/src/adapter/generated/interaction/index.ts");
    assert!(generated.contains("\"invalidAuthorityState\""));
    assert!(generated.contains("InteractionActionCode"));
    assert!(generated.contains("InteractionRoleCode"));
    assert!(generated.contains("InteractionShortcutResponseCode"));
    assert!(!generated.contains("semanticCode"));

    let projection_source = include_str!("../../src/game/interaction.rs");
    assert!(
        projection_source.contains("Vec<(LoopShortcutPointProjection, Vec<u32>)>"),
        "declared points and their segments accumulate as one paired vector, so no arm can publish a point without its segment"
    );
    assert!(!projection_source.contains(":?}"));
    assert!(!projection_source.contains(".variant_name()"));
    assert!(!projection_source.contains("let semantic_code"));
    assert!(!projection_source.contains("action.into()"));
    for forbidden in [
        "\"manaPip\"",
        "\"epoch\"",
        "\"routeId\"",
        "\"breakpointId\"",
        "\"shortcutResponse\"",
        "\"iterationCount\"",
    ] {
        assert!(
            !projection_source.contains(forbidden),
            "interaction projection must not expose {forbidden}"
        );
    }
}

#[test]
fn interaction_serial_increments_within_the_protocol_bound() {
    let mut state = GameState::new_two_player(42);
    bind(&mut state, "serial");
    state.next_interaction_serial = "999999999999999999999999999999".to_string();
    apply(&mut state, P0, GameAction::PassPriority).expect("pass priority");
    assert!(state.active_interaction_slots[0]
        .interaction_id
        .as_str()
        .ends_with(".999999999999999999999999999999"));
    assert_eq!(
        state.next_interaction_serial,
        "1000000000000000000000000000000"
    );
}

#[test]
fn oversized_session_fails_closed_and_serial_rolls_to_next_generation() {
    let mut oversized_session = GameState::new_two_player(42);
    let error = bind_interaction_authority(
        &mut oversized_session,
        InteractionSessionId("s".repeat(129)),
    )
    .expect_err("session IDs are bounded before capability minting");
    assert_eq!(error.code, InteractionReasonCode::InvalidAuthorityState);
    assert!(oversized_session.active_interaction_slots.is_empty());

    let mut serial = GameState::new_two_player(42);
    bind(&mut serial, &"s".repeat(128));
    serial.next_interaction_serial = "9".repeat(32);
    apply(&mut serial, P0, GameAction::PassPriority).expect("normal action still resolves");
    assert_eq!(serial.interaction_generation, 1);
    assert_eq!(serial.next_interaction_serial, "1");
    assert!(serial.active_interaction_slots[0]
        .interaction_id
        .as_str()
        .ends_with(&format!(".0.{}", "9".repeat(32))));
    assert_eq!(viewer_interaction(&serial, P1).opportunities.len(), 1);

    let mut longest_valid = GameState::new_two_player(42);
    bind(&mut longest_valid, &"v".repeat(128));
    longest_valid.next_interaction_serial = "8".repeat(32);
    apply(&mut longest_valid, P0, GameAction::PassPriority).expect("bounded serial resolves");
    let view = viewer_interaction(&longest_valid, P1);
    assert!(view.opportunities.iter().all(|opportunity| {
        opportunity.interaction_id.as_str().len() <= 256
            && match &opportunity.response {
                InteractionOpportunityResponse::ExactChoices { choices }
                | InteractionOpportunityResponse::Schema {
                    candidates: choices,
                    ..
                } => choices.iter().all(|choice| choice.id.as_str().len() <= 256),
            }
    }));
}

fn sideboard_deck_entry(name: &str, count: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: name.to_string(),
            ..Default::default()
        },
        count,
    }
}

/// A Standard match between games with a registered 60/15 pool. `Aaa` sorts
/// before `Bbb`, so the projection's candidate indices are stable.
fn between_games_sideboard_state() -> GameState {
    let mut state = GameState::new_two_player(11);
    state.match_phase = MatchPhase::BetweenGames;
    state.game_number = 2;
    state.deck_pools = vec![PlayerDeckPool {
        player: P0,
        registered_main: Arc::new(vec![sideboard_deck_entry("Aaa", 60)]),
        registered_sideboard: Arc::new(vec![sideboard_deck_entry("Bbb", 15)]),
        current_main: Arc::new(vec![sideboard_deck_entry("Aaa", 60)]),
        current_sideboard: Arc::new(vec![sideboard_deck_entry("Bbb", 15)]),
        ..Default::default()
    }];
    // The projection recomputes its bounds from `deck_pools` + `format_config`
    // via the same authority `handle_submit_sideboard` uses, so these published
    // copies are the client's display hint, not the gate.
    state.waiting_for = WaitingFor::BetweenGamesSideboard {
        player: P0,
        game_number: 2,
        score: Default::default(),
        min_main_deck_size: 60,
        max_sideboard_size: Some(15),
    };
    state
}

fn deck_partition_opportunity(
    view: &engine::types::interaction::ViewerInteraction,
) -> (
    &engine::types::interaction::InteractionOpportunity,
    u32,
    u32,
) {
    let opportunity = view
        .opportunities
        .iter()
        .find(|opportunity| {
            matches!(
                &opportunity.response,
                InteractionOpportunityResponse::Schema {
                    spec: InteractionResponseSpec::DeckPartition { .. },
                    ..
                }
            )
        })
        .expect("a between-games seat is offered a deck-partition schema");
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::DeckPartition {
                min_main_total,
                max_main_total,
                ..
            },
        ..
    } = &opportunity.response
    else {
        unreachable!("filtered for DeckPartition above");
    };
    (opportunity, *min_main_total, *max_main_total)
}

fn partition_choice_ids(
    opportunity: &engine::types::interaction::InteractionOpportunity,
) -> Vec<InteractionChoiceId> {
    let InteractionOpportunityResponse::Schema { candidates, .. } = &opportunity.response else {
        unreachable!("deck partition is a schema response");
    };
    candidates.iter().map(|choice| choice.id.clone()).collect()
}

/// CR 100.2a + CR 100.4a + CR 100.5: `deck_size.min_cards()` is the floor of the
/// format's `DeckSizeRule` and non-Commander decks have no maximum, so the
/// between-games schema must publish the interval the engine will accept —
/// `[minimum, whole pool]` — not one exact size. A
/// player who registered 60/15 may legally present anything from 60 up to all
/// 75 cards; the sideboard cap is what pins the floor at 60.
///
/// This drives `HumanResponseModel::SideboardPartition` end-to-end (schema →
/// submission → applied state) rather than calling `handle_submit_sideboard`
/// directly, because the interaction layer carries its own copy of the gate.
#[test]
fn deck_partition_schema_publishes_an_interval_not_an_exact_deck_size() {
    let mut state = between_games_sideboard_state();
    bind(&mut state, "sideboard-interval");

    let view = viewer_interaction(&state, P0);
    let (opportunity, min_main_total, max_main_total) = deck_partition_opportunity(&view);
    assert_eq!(
        (min_main_total, max_main_total),
        (60, 75),
        "60-card minimum, and the whole 75-card pool may go to the main deck"
    );
    // No exact aggregate exists for a range, so `total` must stay absent.
    assert!(opportunity
        .surfaces
        .contains(&InteractionPresentationSurface::Amount {
            min: 60,
            max: 75,
            total: None,
        }));

    let choice_ids = partition_choice_ids(opportunity);
    let interaction_id = opportunity.interaction_id.clone();

    // 59 main cards would leave a 16-card sideboard: below the floor.
    let too_small = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("sideboard-too-small".to_string()),
            interaction_id: interaction_id.clone(),
            response: InteractionResponse::DeckPartition {
                main: vec![AmountAssignment {
                    choice_id: choice_ids[0].clone(),
                    amount: 59,
                }],
            },
        },
    );
    assert_eq!(
        too_small.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ConstraintUnsatisfied,
        }
    );

    // 61/14 — siding one card in without siding one out. This is the exact
    // shape the old exact-total contract rejected.
    submit_interaction(
        &mut state,
        P0,
        InteractionSubmission {
            interaction_id,
            response: InteractionResponse::DeckPartition {
                main: vec![
                    AmountAssignment {
                        choice_id: choice_ids[0].clone(),
                        amount: 60,
                    },
                    AmountAssignment {
                        choice_id: choice_ids[1].clone(),
                        amount: 1,
                    },
                ],
            },
        },
    )
    .expect("a 61-card main deck is legal when the sideboard still fits under 15");

    let pool = &state.deck_pools[0];
    assert_eq!(
        pool.current_main
            .iter()
            .map(|entry| entry.count)
            .sum::<u32>(),
        61
    );
    assert_eq!(
        pool.current_sideboard
            .iter()
            .map(|entry| entry.count)
            .sum::<u32>(),
        14
    );
}

/// The interaction contract omits a debug-capability gate at the transport
/// (`SessionManager::handle_interaction`) on the grounds that candidate
/// enumeration never produces one. This converts that "cannot happen" into
/// something that fails the day it starts happening.
///
/// It asserts on the **client-visible** publication — `derive_viewer_interaction`
/// -> `opportunity_for_slot` -> `actor_candidates` -> `ai_support`'s validated
/// candidate set — rather than on an internal helper, so it covers what a
/// remote seat could actually submit.
///
/// The sandbox capability is armed *fully* and deliberately: the claim is not
/// that debug actions are unreachable because sandbox mode is off, it is that
/// enumeration ignores the flag even when it is on. All three of
/// `allow_debug_actions`, `debug_mode`, and `debug_permitted` are set because
/// `apply`'s own gate requires the latter two together — arming only one would
/// leave the capability half-granted and the test could pass for the wrong
/// reason.
#[test]
fn published_interaction_choices_never_offer_a_debug_action_in_a_sandbox_game() {
    let mut state = GameState::new_two_player(42);
    state.format_config.allow_debug_actions = true;
    state.debug_mode = true;
    state.debug_permitted.insert(P0);
    bind(&mut state, "sandbox-debug-enumeration");

    let view = priority_view(&state);

    // Reach guard (1): a `ViewerInteraction` with `can_submit: false`, or a
    // terminal `waiting_for`, publishes no opportunities at all and would
    // satisfy the negative below vacuously.
    assert!(
        !view.opportunities.is_empty(),
        "the fixture must publish something for the negative assertion to bite"
    );

    // Reach guard (3): the capability is genuinely in force at assertion time.
    assert!(
        state.format_config.allow_debug_actions
            && state.debug_mode
            && state.debug_permitted.contains(&P0),
        "the sandbox capability must be armed, or this asserts nothing"
    );

    // Reach guard (2): `WaitingFor::Priority` maps to
    // `HumanResponseModel::ExactCandidates`, which is the `actor_candidates`
    // branch — the enumerator whose output this test is about.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the enumerating branch is selected by the waiting_for shape, got {:?}",
        state.waiting_for
    );

    let mut saw_choices = false;
    for opportunity in &view.opportunities {
        let InteractionOpportunityResponse::ExactChoices { choices } = &opportunity.response else {
            continue;
        };
        saw_choices |= !choices.is_empty();
        for choice in choices {
            for surface in &choice.surfaces {
                if let InteractionPresentationSurface::Action { code, .. } = surface {
                    assert!(
                        !matches!(
                            code,
                            InteractionActionCode::Debug
                                | InteractionActionCode::GrantDebugPermission
                                | InteractionActionCode::RevokeDebugPermission
                        ),
                        "candidate enumeration published a debug action ({code:?}); \
                         `SessionManager::handle_interaction`'s missing debug gate is \
                         no longer safe"
                    );
                }
            }
        }
    }

    assert!(
        saw_choices,
        "an ExactChoices opportunity with real choices is what proves the \
         actor_candidates path ran"
    );
}

// ---------------------------------------------------------------------------
// Issue #6944: a flexible-mana land rendered an unlabelled "Tap for mana".
//
// `TapLandForMana` candidates are minted from `ManaSourceOption::semantic_selection`
// (one *concrete* row per producible color) and executed via
// `live_land_mana_option_for_selection`. The label projection resolved them
// through the *manual* authority (`live_mana_source_option_for_selection`)
// instead, whose `manual_selection_for_option` deliberately collapses a flexible
// source to `Colorless` + `DeferredColorChoice`. The concrete row therefore never
// matched, the resolver returned `Err`, and the projection silently emitted no
// `ProducedMana` surface at all.
//
// Every test below asserts a *non-empty* produced-mana label for a flexible
// source, which is exactly the surface that was missing before the fix.
// ---------------------------------------------------------------------------

/// Produced-mana symbols projected for each `TapLandForMana` candidate whose
/// source is `source` — one inner `Vec` per candidate, one entry per produced
/// mana unit. An unlabelled candidate surfaces as an empty inner `Vec`.
fn projected_land_mana_labels(
    state: &mut GameState,
    source: ObjectId,
    binding: &str,
) -> Vec<Vec<String>> {
    bind(state, binding);
    let view = priority_view(state);
    let InteractionOpportunityResponse::ExactChoices { choices } = &view.opportunities[0].response
    else {
        panic!("priority is projected as exact choices");
    };
    choices
        .iter()
        .filter(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Action {
                        code: InteractionActionCode::TapLandForMana,
                        ..
                    }
                )
            }) && choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Object {
                        role: InteractionRoleCode::Source,
                        reference,
                        ..
                    } if reference == &source.0.to_string()
                )
            })
        })
        .map(|choice| {
            choice
                .surfaces
                .iter()
                .filter_map(|surface| match surface {
                    InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::ProducedMana,
                        symbols,
                        ..
                    } => symbols.first().cloned(),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// Flatten per-candidate labels into one sorted symbol list, asserting that no
/// candidate was left unlabelled. The unlabelled case is the #6944 regression.
fn sorted_labelled_symbols(labels: &[Vec<String>], context: &str) -> Vec<String> {
    assert!(
        !labels.is_empty(),
        "{context}: expected at least one TapLandForMana candidate"
    );
    assert!(
        labels.iter().all(|units| !units.is_empty()),
        "{context}: every mana candidate must carry a produced-mana label, got {labels:?}"
    );
    let mut symbols: Vec<String> = labels.iter().flatten().cloned().collect();
    symbols.sort();
    symbols
}

#[test]
fn tap_land_for_mana_labels_each_color_of_an_any_one_color_land() {
    // ManaProduction::AnyOneColor — the card from issue #6944.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let city = scenario
        .add_land_from_oracle(
            P0,
            "City of Brass",
            "Whenever this land becomes tapped, it deals 1 damage to you.\n{T}: Add one mana of any color.",
        )
        .id();
    let mut runner = scenario.build();

    let labels = projected_land_mana_labels(runner.state_mut(), city, "city-of-brass-mana-label");
    assert_eq!(
        sorted_labelled_symbols(&labels, "City of Brass"),
        ["B", "G", "R", "U", "W"],
        "each concrete color row must project its own color, not an unlabelled tap"
    );
    assert!(
        labels.iter().all(|units| units.len() == 1),
        "'Add one mana of any color' produces exactly one unit per row: {labels:?}"
    );
}

#[test]
fn tap_land_for_mana_labels_a_granted_flexible_mana_ability() {
    // ManaProduction::AnyOneColor { count: 2 } reached through a `GrantAbility`
    // static — the second card named in issue #6944. The label must carry both
    // produced units and the granted spend restriction.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(
        P0,
        "Resonating Lute",
        "Lands you control have \"{T}: Add two mana of any one color. Spend this mana only to cast instant and sorcery spells.\"\n{T}: Draw a card. Activate only if you have seven or more cards in your hand.",
    );
    // An explicitly-printed mana ability, not `add_basic_land`: a basic land's
    // production is subtype-inferred by `land_mana_options`, and that fallback is
    // deliberately suppressed once any explicit `Effect::Mana` ability exists —
    // which the grant itself supplies. Printing the ability keeps this test about
    // the label projection rather than the basic-land fallback.
    let forest = scenario
        .add_land_from_oracle(P0, "Forest", "{T}: Add {G}.")
        .id();
    let mut runner = scenario.build();
    // `GameScenario::build` does not run a layer pass, so the `GrantAbility`
    // static has not yet been applied to the land's ability list.
    engine::game::layers::evaluate_layers(runner.state_mut());

    let labels = projected_land_mana_labels(runner.state_mut(), forest, "resonating-lute-grant");
    let symbols = sorted_labelled_symbols(&labels, "Resonating Lute granted ability");
    let granted: Vec<&Vec<String>> = labels.iter().filter(|units| units.len() == 2).collect();
    assert_eq!(
        granted.len(),
        5,
        "the granted 'two mana of any one color' ability exposes one two-unit row \
         per color: {labels:?}"
    );
    assert!(
        granted
            .iter()
            .all(|units| units[0] == units[1] && symbols.contains(&units[0])),
        "'any one color' produces two units of the SAME chosen color: {granted:?}"
    );
    assert!(
        labels.iter().any(|units| units == &vec!["G".to_string()]),
        "the Forest's own printed mana ability is still labelled: {labels:?}"
    );
}

#[test]
fn tap_land_for_mana_labels_an_any_type_produceable_by_land() {
    // ManaProduction::AnyTypeProduceableBy.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pool = scenario
        .add_land_from_oracle(
            P0,
            "Reflecting Pool",
            "{T}: Add one mana of any type that a land you control could produce.",
        )
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let labels = projected_land_mana_labels(runner.state_mut(), pool, "reflecting-pool-mana-label");
    assert_eq!(
        sorted_labelled_symbols(&labels, "Reflecting Pool"),
        ["G"],
        "the surveyed Forest's type is the only produceable type"
    );
}

#[test]
fn tap_land_for_mana_labels_an_opponent_land_colors_land() {
    // ManaProduction::OpponentLandColors.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let orchard = scenario
        .add_land_from_oracle(
            P0,
            "Exotic Orchard",
            "{T}: Add one mana of any color that a land an opponent controls could produce.",
        )
        .id();
    scenario.add_basic_land(P1, ManaColor::Blue);
    let mut runner = scenario.build();

    let labels = projected_land_mana_labels(runner.state_mut(), orchard, "exotic-orchard-label");
    assert_eq!(
        sorted_labelled_symbols(&labels, "Exotic Orchard"),
        ["U"],
        "the opponent's Island is the only surveyed color"
    );
}

#[test]
fn tap_land_for_mana_labels_a_commander_color_identity_land() {
    // ManaProduction::AnyInCommandersColorIdentity.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tower = scenario
        .add_land_from_oracle(
            P0,
            "Command Tower",
            "{T}: Add one mana of any color in your commander's color identity.",
        )
        .id();
    let commander = scenario
        .add_creature(P0, "Mono-Red Commander", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_commander(commander);
    let mut runner = scenario.build();

    let labels = projected_land_mana_labels(runner.state_mut(), tower, "command-tower-label");
    assert_eq!(
        sorted_labelled_symbols(&labels, "Command Tower"),
        ["R"],
        "the label follows the commander's color identity"
    );
}

#[test]
fn tap_land_for_mana_labels_an_any_color_among_permanents_land() {
    // ManaProduction::AnyOneColorAmongPermanents.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plaza = scenario
        .add_land_from_oracle(
            P0,
            "Plaza of Heroes",
            "{T}: Add {C}.\n{T}: Add one mana of any color. Spend this mana only to cast a legendary spell.\n{T}: Add one mana of any color among legendary permanents you control.\n{3}, {T}, Exile this land: Target legendary creature gains hexproof and indestructible until end of turn.",
        )
        .id();
    scenario
        .add_creature(P0, "Legendary Red Bear", 2, 2)
        .as_legendary()
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        });
    let mut runner = scenario.build();

    let labels = projected_land_mana_labels(runner.state_mut(), plaza, "plaza-of-heroes-label");
    let symbols = sorted_labelled_symbols(&labels, "Plaza of Heroes");
    assert!(
        symbols.contains(&"R".to_string()),
        "the among-legendary-permanents ability projects the legend's color: {labels:?}"
    );
    assert!(
        symbols.contains(&"C".to_string()),
        "the sibling colorless ability stays labelled: {labels:?}"
    );
}

#[test]
fn tap_land_for_mana_labels_a_choice_among_exiled_colors_land() {
    // ManaProduction::ChoiceAmongExiledColors.
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pit = scenario
        .add_land_from_oracle(
            P0,
            "Pit of Offerings",
            "{T}: Add {C}.\n{T}: Add one mana of any of the exiled cards' colors.",
        )
        .id();
    let exiled = scenario.add_real_card(P0, "Lightning Bolt", Zone::Exile, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .exile_links
        .push(engine::types::game_state::ExileLink {
            exiled_id: exiled,
            source_id: pit,
            kind: engine::types::game_state::ExileLinkKind::TrackedBySource,
        });

    let labels = projected_land_mana_labels(runner.state_mut(), pit, "pit-of-offerings-label");
    assert_eq!(
        sorted_labelled_symbols(&labels, "Pit of Offerings"),
        ["C", "R"],
        "the exiled red card's color is labelled alongside the colorless sibling"
    );
}

// ---------------------------------------------------------------------------
// Sibling coverage: `ActivateManaSource`.
//
// The two mana surfaces now share `push_produced_mana_surfaces`, each passing
// its own reducer's resolver. The tests above pin the `TapLandForMana` arm; this
// one pins the `ActivateManaSource` arm so the shared helper cannot be changed
// to satisfy one caller while silently dropping the other's labels.
//
// `ActivateManaSource` is only ever projected from the
// `WaitingFor::ManaSourceSelection` arm of `direct_choice_projection` — no
// priority arm mints it — so the fixture must drive the real cast pipeline into
// that window. `CastPaymentMode::AutoExceptSacrificialMana` does exactly that:
// the automatic planner refuses to spend an irreversible sacrifice row without
// explicit consent and hands the choice back as `ManaSourceSelection`.
// ---------------------------------------------------------------------------

fn sacrificial_mana_source(produced: ManaProduction) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Sacrifice(SacrificeCost::count(
        TargetFilter::SelfRef,
        1,
    )))
}

/// Produced-mana symbols projected for the `ActivateManaSource` candidates whose
/// source is `source` — one inner `Vec` per candidate, one entry per produced
/// mana unit. An unlabelled candidate surfaces as an empty inner `Vec`.
fn projected_mana_source_labels(
    state: &mut GameState,
    source: ObjectId,
    binding: &str,
) -> Vec<Vec<String>> {
    bind(state, binding);
    let view = viewer_interaction(state, P0);
    let InteractionOpportunityResponse::ExactChoices { choices } = &view.opportunities[0].response
    else {
        panic!("the mana-source prompt is projected as exact choices");
    };
    choices
        .iter()
        .filter(|choice| {
            choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Action {
                        code: InteractionActionCode::ActivateManaSource,
                        ..
                    }
                )
            }) && choice.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    InteractionPresentationSurface::Object {
                        role: InteractionRoleCode::Source,
                        reference,
                        ..
                    } if reference == &source.0.to_string()
                )
            })
        })
        .map(|choice| {
            choice
                .surfaces
                .iter()
                .filter_map(|surface| match surface {
                    InteractionPresentationSurface::Mana {
                        role: InteractionRoleCode::ProducedMana,
                        symbols,
                        ..
                    } => symbols.first().cloned(),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

#[test]
fn activate_mana_source_labels_fixed_and_flexible_sacrificial_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Mana Source Label Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    // Both rows must be sacrifice-only: a non-sacrificial row on either source
    // would let the automatic planner pay without ever opening the prompt.
    let fixed = scenario
        .add_creature(P0, "Fixed Output Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_source(ManaProduction::Fixed {
            colors: vec![ManaColor::Black],
            contribution: ManaContribution::Base,
        }))
        .id();
    let flexible = scenario
        .add_creature(P0, "Flexible Output Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_source(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 2 },
            color_options: vec![ManaColor::Red, ManaColor::Green],
            contribution: ManaContribution::Base,
        }))
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
        })
        .expect("the production cast path should stop for sacrificial-mana consent");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaSourceSelection { .. }
        ),
        "ActivateManaSource is projected only from this window, got {:?}",
        runner.state().waiting_for
    );

    let fixed_labels = projected_mana_source_labels(runner.state_mut(), fixed, "fixed-mana-source");
    assert_eq!(
        fixed_labels,
        vec![vec!["B".to_string()]],
        "a fixed sacrificial source projects its one concrete produced unit"
    );

    let flexible_labels =
        projected_mana_source_labels(runner.state_mut(), flexible, "flexible-mana-source");
    assert_eq!(
        flexible_labels,
        vec![vec!["R".to_string(), "R".to_string()]],
        "a flexible source is offered as ONE deferred-color candidate whose label \
         still carries both produced units; `manual_selection_for_option` collapses \
         it to Colorless + DeferredColorChoice, so resolving it through the land \
         authority (the #6944 bug) would drop this label entirely"
    );
}

// ═════════════════════════════════════════════════════════════════════════════════════════
// PHASE 4 — the SEQUENCED-pin ingress: the A3 coherence relation, the hostile allocations,
// the until-lethal withdrawal, the wire charge, serde additivity, and the progress window.
// ═════════════════════════════════════════════════════════════════════════════════════════

/// One staged loop-shortcut offer whose points are exactly `kinds`, each on its own slot index
/// over a live battlefield creature read at its CURRENT incarnation (CR 400.7).
///
/// The slot source is a live battlefield object for the reason
/// [`loop_shortcut_human_ingress_emits_the_target_class_spelling_for_a_submitted_seat`] records
/// above: an `AllCopies` source no object carries resolves `None` and fails `validate_pins`
/// closed, which would make every positive leg below unsatisfiable and invite loosening a
/// fail-closed predicate to "fix" it.
fn stage_sequenced_offer(
    label: &str,
    iteration_count: IterationCount,
    max_iterations: u32,
    kinds: Vec<DecisionPointKind>,
) -> (engine::game::scenario::GameRunner, Vec<DecisionSlot>) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    let source = scenario.add_creature(P0, "P4 Ability Source", 1, 1).id();
    let mut runner = scenario.build();
    let incarnation = runner.state().objects[&source].incarnation;
    let slots: Vec<DecisionSlot> = (0..kinds.len())
        .map(|index| DecisionSlot {
            source: engine::types::game_state::YieldTarget::ThisObject {
                source_id: source,
                incarnation: Some(incarnation),
                trigger_description: None,
            },
            index: index as u8,
        })
        .collect();
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: engine::analysis::loop_check::LoopCertificate {
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::Advantage,
            mandatory: false,
            residual_board_delta: engine::analysis::resource::BoardDelta::default(),
            per_cycle: None,
        },
        schema: ShortcutDecisionSchema {
            iteration_count,
            max_iterations,
            points: slots
                .iter()
                .cloned()
                .zip(kinds)
                .map(|(slot, kind)| DecisionPoint { slot, kind })
                .collect(),
            convoke_tappable_count: 0,
        },
        declaration: None,
    };
    bind(runner.state_mut(), label);
    (runner, slots)
}

/// The three opponent seats of the staged 4-player board, as a `Targets` point over all of
/// them at a single position.
fn victims_point(min_targets: u32, max_targets: u32) -> DecisionPointKind {
    DecisionPointKind::Targets {
        legal_targets: vec![
            TargetRef::Player(P1),
            TargetRef::Player(PlayerId(2)),
            TargetRef::Player(PlayerId(3)),
        ],
        min_targets,
        max_targets,
        ordered: true,
    }
}

fn shortcut_points(
    view: &engine::types::interaction::ViewerInteraction,
) -> Vec<InteractionShortcutPoint> {
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { points, .. },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("the loop shortcut offer uses a shortcut schema");
    };
    points.clone()
}

fn submit_pins(
    state: &GameState,
    view: &engine::types::interaction::ViewerInteraction,
    decision: InteractionShortcutDecision,
    pins: Vec<InteractionShortcutPin>,
) -> Result<GameAction, engine::game::interaction::InteractionSubmitError> {
    resolve_interaction_response(
        state,
        P0,
        &InteractionSubmission {
            interaction_id: view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Shortcut { decision, pins },
        },
    )
}

/// A sequenced pin naming `allocation` as `(candidate index, amount)` pairs against `point`.
fn sequenced_pin(
    point: &InteractionShortcutPoint,
    allocation: &[(usize, u32)],
) -> InteractionShortcutPin {
    InteractionShortcutPin {
        group: point.group,
        choice_ids: allocation
            .iter()
            .map(|(candidate, _)| point.candidate_ids[*candidate].clone())
            .collect(),
        amounts: allocation
            .iter()
            .map(|(candidate, amount)| AmountAssignment {
                choice_id: point.candidate_ids[*candidate].clone(),
                amount: *amount,
            })
            .collect(),
    }
}

/// **Row (3)** — the A3 COHERENCE RELATION, each refusal with its point named and each paired
/// against the conforming declaration that differs only in the clause under test.
///
/// # Discrimination
///
/// Delete any one conjunct and its own leg starts accepting while the others stay red.
///
/// # The paired positive that proves the relaxation is CONFINED
///
/// A pin whose `choice_ids.len()` is inside `[min, max]` with EMPTY `amounts` still decodes
/// exactly as before this phase — asserted here, and asserted independently on both candidate
/// classes by the shipped
/// [`loop_shortcut_human_ingress_emits_the_target_class_spelling_for_a_submitted_seat`].
#[test]
fn p4_row_3_the_sequenced_pin_coherence_relation_refuses_each_incoherent_shape() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, PinnedDecision, Ranking, TargetPin, TargetSchedule,
    };

    // ── A `Fixed` offer publishing a single-position Targets point AND a MayChoice point ──
    let (runner, slots) = stage_sequenced_offer(
        "p4-coherence-fixed",
        IterationCount::Fixed(6),
        6,
        vec![victims_point(1, 1), DecisionPointKind::MayChoice],
    );
    let view = priority_view(runner.state());
    let points = shortcut_points(&view);
    assert_eq!(
        (points.len(), points[0].max, points[0].candidate_ids.len()),
        (2, 1, 3),
        "reach-guard: a single-position Targets point over three candidates, beside a \
         non-Targets point — the two shapes every leg below distinguishes"
    );
    let may_pin = InteractionShortcutPin {
        group: points[1].group,
        choice_ids: vec![points[1].candidate_ids[0].clone()],
        amounts: Vec::new(),
    };
    let fixed = InteractionShortcutDecision::Fixed { iterations: 6 };

    // ── PAIRED POSITIVE: a FLAT pin decodes exactly as at BASE ──
    let flat = submit_pins(
        runner.state(),
        &view,
        fixed,
        vec![
            InteractionShortcutPin {
                group: points[0].group,
                choice_ids: vec![points[0].candidate_ids[0].clone()],
                amounts: Vec::new(),
            },
            may_pin.clone(),
        ],
    )
    .expect("a flat in-window pin with empty amounts still decodes");
    let GameAction::DeclareShortcut {
        template: Some(flat_template),
        ..
    } = flat
    else {
        panic!("a shortcut acceptance carrying pins materializes a template");
    };
    assert!(
        flat_template.decisions.contains(&PinnedDecision::Targets {
            slot: slots[0].clone(),
            targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                Ranking::one(AnnouncementSubject::Seat(P1))
            ))],
        }),
        "the relaxation is CONFINED to sequenced pins: a flat pin still lowers to the \
         one-entry per-position spelling. got {:?}",
        flat_template.decisions
    );

    // ── PAIRED POSITIVE: the SEQUENCED pin the refusals below each perturb by one clause ──
    let conforming = sequenced_pin(&points[0], &[(0, 1), (1, 2), (2, 3)]);
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![conforming.clone(), may_pin.clone()]
        )
        .is_ok(),
        "paired positive: the conforming allocation is ACCEPTED, so every refusal below is \
         attributable to its own clause rather than to an ingress refusing everything"
    );

    // 1 — `amounts` non-empty on a point whose kind is not `Targets` ⇒ the sequenced gate.
    let mut amounts_on_may = may_pin.clone();
    amounts_on_may.amounts = vec![AmountAssignment {
        choice_id: points[1].candidate_ids[0].clone(),
        amount: 6,
    }];
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![
                InteractionShortcutPin {
                    group: points[0].group,
                    choice_ids: vec![points[0].candidate_ids[0].clone()],
                    amounts: Vec::new(),
                },
                amounts_on_may
            ]
        )
        .is_err(),
        "a sequenced pin is admissible only on a `Targets` point"
    );

    // 3 — `amounts.len() != choice_ids.len()`.
    let mut short_amounts = conforming.clone();
    short_amounts.amounts.pop();
    short_amounts.amounts[1].amount = 5;
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![short_amounts, may_pin.clone()]
        )
        .is_err(),
        "one amount per announced subject, positionally"
    );

    // 4 — `amounts[i].choice_id != choice_ids[i]` at some `i`: the SAME multiset of ids and
    //     the SAME sum, differing only in the positional pairing.
    let mut transposed = conforming.clone();
    transposed.amounts.swap(0, 1);
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![transposed, may_pin.clone()]
        )
        .is_err(),
        "the positional compare: an allocation whose amounts are attached to the wrong \
         subjects is not the allocation its `choice_ids` name"
    );

    // 6 — `amounts` EMPTY with `choice_ids` longer than the point's `max` on a `Fixed` offer:
    //     a sequence with no declared lengths cannot partition a finite count. This leg is
    //     also what shows rows (1), (1b) and (1c) depend on the `amounts` field: the same
    //     multi-candidate pin without it is refused.
    let mut amount_free = conforming.clone();
    amount_free.amounts.clear();
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![amount_free, may_pin.clone()]
        )
        .is_err(),
        "an amount-free sequence cannot partition a declared count"
    );

    // ── A `Targets` point whose published `max` is NOT 1: BOTH sequenced entries refused ──
    let (wide_runner, _) = stage_sequenced_offer(
        "p4-coherence-wide",
        IterationCount::Fixed(6),
        6,
        vec![victims_point(1, 2)],
    );
    let wide_view = priority_view(wide_runner.state());
    let wide_points = shortcut_points(&wide_view);
    assert_eq!(
        wide_points[0].max, 2,
        "reach-guard: this offer's Targets point is multi-position, which is the shape A3 \
         declines to represent"
    );
    assert!(
        submit_pins(
            wide_runner.state(),
            &wide_view,
            fixed,
            vec![sequenced_pin(&wide_points[0], &[(0, 2), (1, 4)])]
        )
        .is_err(),
        "`amounts` non-empty on a point whose max is not 1 ⇒ refused"
    );
    assert!(
        submit_pins(
            wide_runner.state(),
            &wide_view,
            fixed,
            vec![InteractionShortcutPin {
                group: wide_points[0].group,
                choice_ids: wide_points[0].candidate_ids.clone(),
                amounts: Vec::new(),
            }]
        )
        .is_err(),
        "`choice_ids` longer than max on a point whose max is not 1 ⇒ refused by the same gate"
    );
    assert!(
        submit_pins(
            wide_runner.state(),
            &wide_view,
            fixed,
            vec![InteractionShortcutPin {
                group: wide_points[0].group,
                choice_ids: wide_points[0].candidate_ids[..2].to_vec(),
                amounts: Vec::new(),
            }]
        )
        .is_ok(),
        "paired positive on the SAME offer: a flat pin filling both positions is accepted, so \
         the two refusals above are the sequenced gate and not a broken board"
    );
}

/// **Row (4)** — HOSTILE ROWS ON THE ALLOCATION ITSELF, each with its refusal point named.
///
/// The middle-seat hexproof-after-latch leg lives on the real 4p dump, in
/// `fantastic_four_bounded_loop::p4_row_2_a_later_segment_that_went_illegal_refuses_the_whole_declaration`,
/// where a live board can actually grant it.
#[test]
fn p4_row_4_hostile_allocations_are_refused_each_at_its_own_guard() {
    let (runner, _) = stage_sequenced_offer(
        "p4-hostile-allocations",
        IterationCount::Fixed(6),
        6,
        vec![victims_point(1, 1)],
    );
    let view = priority_view(runner.state());
    let points = shortcut_points(&view);
    let fixed = InteractionShortcutDecision::Fixed { iterations: 6 };

    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![sequenced_pin(&points[0], &[(0, 1), (1, 2), (2, 3)])]
        )
        .is_ok(),
        "paired positive: the conforming allocation is accepted"
    );

    // Sum != the declared count ⇒ `start != *declared`.
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![sequenced_pin(&points[0], &[(0, 1), (1, 2), (2, 2)])]
        )
        .is_err(),
        "a composition of the declared count sums to it"
    );

    // A `choice_id` outside the point's `candidate_ids` ⇒ the `'k'` resolution, `UnknownChoice`.
    let mut unknown = sequenced_pin(&points[0], &[(0, 1), (1, 2), (2, 3)]);
    unknown.choice_ids[2] = InteractionChoiceId("not-an-offered-candidate".to_string());
    unknown.amounts[2].choice_id = unknown.choice_ids[2].clone();
    assert_eq!(
        submit_pins(runner.state(), &view, fixed, vec![unknown])
            .expect_err("an unoffered candidate is refused")
            .code,
        InteractionReasonCode::UnknownChoice,
        "the sequence resolves against the POINT's own candidate indices, exactly as the flat \
         path already does"
    );

    // A DUPLICATE subject ⇒ `Ranking::new`'s `DuplicateSubject`. Load-bearing for the whole
    // class, not for this fixture: `loop_shortcut_projection`'s `Targets` arm hard-codes
    // `unique: false` into every point it mints, so `point.unique` can never refuse one.
    assert!(
        !points[0].unique,
        "reach-guard: the published point does NOT carry the uniqueness flag, so the refusal \
         below is `Ranking`'s type invariant and not the point's own guard"
    );
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![sequenced_pin(&points[0], &[(0, 1), (1, 2), (0, 3)])]
        )
        .is_err(),
        "a duplicate subject is refused at `Ranking::new`. On the FIXED path this is this \
         ingress's deliberate foreclosure of NON-CONTIGUOUS allocations rather than a rule \
         consequence — the engine itself accepts two disjoint segments naming one seat"
    );

    // A ZERO-amount segment ⇒ a composition's parts are positive.
    assert!(
        submit_pins(
            runner.state(),
            &view,
            fixed,
            vec![sequenced_pin(&points[0], &[(0, 0), (1, 2), (2, 4)])]
        )
        .is_err(),
        "a zero is not a part of the declared count, and would collide two segment starts"
    );
}

/// **Row (5) — the human ingress no longer mints a sequenced until-lethal pin, and the FIXED
/// one still does.**
///
/// CR 732.2c: an accepted proposal's choices are all taken, so an until-lethal declaration may
/// announce only the ONE subject its drive resolves at every repetition. Under a FIXED count the
/// extra ids are a partition across ITERATIONS and every one of them becomes its own segment's
/// head, so that mode is untouched.
///
/// # Discrimination
///
/// Restore the until-lethal sequenced route ⇒ the first leg accepts. Refuse EVERY until-lethal
/// `Targets` submission rather than only the sequenced one — the shape a guard written on the
/// count alone would have — ⇒ the second leg fails. Refuse every sequenced pin ⇒ the FIXED legs
/// fail. The guard's other two conjuncts (the point's KIND, and its published `max`) keep their
/// own ends in
/// `p4_row_3_the_sequenced_pin_coherence_relation_refuses_each_incoherent_shape`, so a rewrite
/// dropping either while adding the count one passes every leg here and fails that row.
#[test]
fn p4_row_5_an_until_lethal_declaration_announces_the_one_subject_its_drive_resolves() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, PinnedDecision, Ranking, TargetPin, TargetSchedule,
    };

    let (runner, slots) = stage_sequenced_offer(
        "p4-announce-one",
        IterationCount::UntilLethal,
        6,
        vec![victims_point(1, 1)],
    );
    let view = priority_view(runner.state());
    let points = shortcut_points(&view);
    assert_eq!(
        (points[0].candidate_ids.len(), points[0].max),
        (3, 1),
        "reach-guard: three candidates on a single-position point, so a submission CAN exceed \
         the published max and a one-id submission can name a candidate other than the head"
    );

    // ── REFUSE: more ids than the point's published `max`, on an until-lethal offer. ──
    let over_max = submit_pins(
        runner.state(),
        &view,
        InteractionShortcutDecision::AcceptSuggested,
        vec![InteractionShortcutPin {
            group: points[0].group,
            choice_ids: points[0].candidate_ids.clone(),
            amounts: Vec::new(),
        }],
    );
    assert_eq!(
        over_max.err().map(|e| e.code),
        Some(InteractionReasonCode::ConstraintUnsatisfied),
        "CR 732.2c: there is no count to partition, so every id past the head names an \
         announcement no repetition makes"
    );

    // ── ACCEPT: a SINGLE id, and NOT the published head — a decoder that kept
    //    `candidate_ids[0]` would satisfy a first-candidate leg and fails this one. ──
    let single = submit_pins(
        runner.state(),
        &view,
        InteractionShortcutDecision::AcceptSuggested,
        vec![InteractionShortcutPin {
            group: points[0].group,
            choice_ids: vec![points[0].candidate_ids[2].clone()],
            amounts: Vec::new(),
        }],
    )
    .expect("a one-subject until-lethal declaration is accepted to the end of the human ingress");
    let GameAction::DeclareShortcut {
        template: Some(template),
        count,
    } = single
    else {
        panic!("a shortcut acceptance carrying pins materializes a template");
    };
    assert_eq!(count, IterationCount::UntilLethal);
    assert_eq!(
        template.decisions,
        vec![PinnedDecision::Targets {
            slot: slots[0].clone(),
            targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                Ranking::one(AnnouncementSubject::Seat(PlayerId(3)))
            ))],
        }],
        "the pin names EXACTLY the one subject submitted, through the ordinary flat decode arm"
    );

    // ── PAIRED POSITIVE: the FIXED sequenced submission still materializes its `Piecewise`,
    //    with the segment starts its amounts name. ──
    let (fixed_runner, fixed_slots) = stage_sequenced_offer(
        "p4-announce-one-fixed",
        IterationCount::Fixed(6),
        6,
        vec![victims_point(1, 1)],
    );
    let fixed_view = priority_view(fixed_runner.state());
    let fixed_points = shortcut_points(&fixed_view);
    let fixed = InteractionShortcutDecision::Fixed { iterations: 6 };
    let partitioned = submit_pins(
        fixed_runner.state(),
        &fixed_view,
        fixed,
        vec![sequenced_pin(&fixed_points[0], &[(0, 1), (1, 2), (2, 3)])],
    )
    .expect("the FIXED sequenced route is untouched");
    let GameAction::DeclareShortcut {
        template: Some(fixed_template),
        ..
    } = partitioned
    else {
        panic!("a shortcut acceptance carrying pins materializes a template");
    };
    let step = |seat: PlayerId| Ranking::one(AnnouncementSubject::Seat(seat));
    assert_eq!(
        fixed_template.decisions,
        vec![PinnedDecision::Targets {
            slot: fixed_slots[0].clone(),
            targets: vec![TargetPin::Scheduled(TargetSchedule::Piecewise(vec![
                (0, step(P1)),
                (1, step(PlayerId(2))),
                (3, step(PlayerId(3))),
            ]))],
        }],
        "a declared count IS partitionable, and its parts are the running prefix sums"
    );

    // ── A DUPLICATE subject is refused at `Ranking::new`, asserted THROUGH THE WIRE PATH — on
    //    the FIXED path, where a multi-subject sequence is still legal and the leg still has a
    //    conforming twin. `point.unique` is false on every `Targets` point this projection
    //    mints, so the type's own invariant is the only thing that can refuse it. ──
    let repeated = sequenced_pin(&fixed_points[0], &[(0, 1), (1, 2), (0, 3)]);
    assert_eq!(
        (repeated.choice_ids[0].clone(), repeated.amounts.len()),
        (repeated.choice_ids[2].clone(), 3),
        "reach-guard: this pin really does name one candidate twice, with a full amount list \
         summing to the declared count — so nothing else can refuse it"
    );
    assert!(
        submit_pins(fixed_runner.state(), &fixed_view, fixed, vec![repeated]).is_err(),
        "a repeated entry is the same declaration twice, not an ordering"
    );
    assert!(
        submit_pins(
            fixed_runner.state(),
            &fixed_view,
            fixed,
            vec![sequenced_pin(&fixed_points[0], &[(0, 1), (1, 2), (2, 3)])]
        )
        .is_ok(),
        "PAIRED POSITIVE: the same partition without the repeat is accepted, so the refusal \
         above is the duplicate and not a broken board"
    );
}

/// **Row (6), engine side** — the inbound wire charge for `amounts`, on the SAME cumulative
/// budget the `choice_ids` legs already charge.
///
/// Cumulative overflow across pins is the only nested branch in the validator and exactly what
/// a naive per-field guard misses. The server-core sibling is
/// `server_core::interaction_payload_guard::rejects_an_oversized_nested_shortcut_amount_budget`;
/// revert the charge and BOTH accept.
#[test]
fn p4_row_6_the_inbound_wire_charge_bounds_amounts_cumulatively() {
    let per_pin = MAX_INTERACTION_LIST_LEN / 2;
    let assignment = AmountAssignment {
        choice_id: InteractionChoiceId("a".to_string()),
        amount: 1,
    };
    let pins: Vec<InteractionShortcutPin> = (0..3)
        .map(|group| InteractionShortcutPin {
            group,
            choice_ids: vec![InteractionChoiceId("a".to_string())],
            amounts: vec![assignment.clone(); per_pin],
        })
        .collect();
    for pin in &pins {
        assert!(
            pin.amounts.len() <= MAX_INTERACTION_LIST_LEN,
            "each pin's allocation list is INDIVIDUALLY legal, so the refusal below is the \
             cumulative ceiling and not a per-field one"
        );
    }
    let submission = |pins: Vec<InteractionShortcutPin>| InteractionSubmission {
        interaction_id: engine::types::interaction::InteractionId("i-1".to_string()),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::AcceptSuggested,
            pins,
        },
    };
    assert!(
        engine::game::interaction::bound_interaction_submission(&submission(pins)).is_err(),
        "the sum of the per-pin `amounts` lists crosses the cumulative ceiling"
    );
    assert!(
        engine::game::interaction::bound_interaction_submission(&submission(vec![
            InteractionShortcutPin {
                group: 0,
                choice_ids: vec![InteractionChoiceId("a".to_string())],
                amounts: vec![assignment; per_pin],
            }
        ]))
        .is_ok(),
        "paired positive: one pin carrying the same per-pin list is ACCEPTED, so the refusal \
         above is the SUM and not the list length"
    );
}

/// **Row (8)** — `#[serde(default)]` ADDITIVITY, PROVEN rather than asserted.
///
/// A submission serialized WITHOUT `amounts` deserializes to an empty vec and drives the
/// pre-phase-4 behaviour unchanged. Remove the attribute ⇒ the parse errors and this row
/// fails. The `skip_serializing_if` half is the stronger property: a pin carrying no amounts
/// is BYTE-IDENTICAL to the pre-field wire shape, which is what keeps this field additive for
/// the protocol version already in the tree.
#[test]
fn p4_row_8_the_amounts_field_is_additive_on_the_wire() {
    let legacy = r#"{"group":0,"choiceIds":["i-1.0.0.k0"]}"#;
    let parsed: InteractionShortcutPin =
        serde_json::from_str(legacy).expect("a pre-field payload still parses");
    assert_eq!(parsed.group, 0);
    assert!(
        parsed.amounts.is_empty(),
        "a payload with no `amounts` key decodes to the empty allocation, which is the shape \
         every flat pin already had"
    );
    assert_eq!(
        serde_json::to_string(&parsed).expect("a pin serializes"),
        legacy,
        "`skip_serializing_if` keeps a pin carrying no amounts byte-identical to the \
         pre-field wire shape"
    );

    // The paired positive: an amount-BEARING payload parses and carries them, so the row is
    // not passing because both shapes are rejected.
    let bearing = r#"{"group":0,"choiceIds":["i-1.0.0.k0"],"amounts":[{"choiceId":"i-1.0.0.k0","amount":3}]}"#;
    let carried: InteractionShortcutPin =
        serde_json::from_str(bearing).expect("an amount-bearing payload parses");
    assert_eq!(
        carried.amounts,
        vec![AmountAssignment {
            choice_id: InteractionChoiceId("i-1.0.0.k0".to_string()),
            amount: 3,
        }]
    );
    assert_eq!(
        serde_json::to_string(&carried).expect("a pin serializes"),
        bearing,
        "the amount-bearing shape round-trips on the same wire key"
    );

    // And a legacy-shaped submission still DRIVES the pre-phase-4 behaviour end to end.
    let (runner, _) = stage_sequenced_offer(
        "p4-serde-additivity",
        IterationCount::Fixed(6),
        6,
        vec![victims_point(1, 1)],
    );
    let view = priority_view(runner.state());
    let points = shortcut_points(&view);
    let wire = serde_json::to_string(&InteractionSubmission {
        interaction_id: view.opportunities[0].interaction_id.clone(),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::Fixed { iterations: 6 },
            pins: vec![InteractionShortcutPin {
                group: points[0].group,
                choice_ids: vec![points[0].candidate_ids[0].clone()],
                amounts: Vec::new(),
            }],
        },
    })
    .expect("a submission serializes");
    assert!(
        !wire.contains("amounts"),
        "the omitted key is what makes the shape additive; wire was {wire}"
    );
    let round_tripped: InteractionSubmission =
        serde_json::from_str(&wire).expect("the omitted-key wire shape parses back");
    assert!(
        resolve_interaction_response(runner.state(), P0, &round_tripped).is_ok(),
        "a legacy-shaped submission still decodes and drives"
    );
}

/// **Row (9)** — PROGRESS STAYS INSIDE ITS OWN WINDOW.
///
/// `InteractionProgress.selected` counts POSITIONS ANSWERED. Charge the sequence length
/// instead of the position count and a sequenced pin publishes `selected > maximum`.
///
/// # Paired positive
///
/// The SAME offer answered with a FLAT pin publishes the identical progress, which is what
/// proves the `.min(point.max)` is the identity on the unchanged path.
#[test]
fn p4_row_9_a_sequenced_pin_publishes_progress_inside_its_own_window() {
    let (runner, _) = stage_sequenced_offer(
        "p4-progress-window",
        IterationCount::Fixed(6),
        6,
        vec![victims_point(1, 1)],
    );
    let view = priority_view(runner.state());
    let points = shortcut_points(&view);

    let preview = |label: &str, pin: InteractionShortcutPin| {
        preview_interaction(
            runner.state(),
            P0,
            &InteractionPreviewRequest {
                request_id: PreviewRequestId(label.to_string()),
                interaction_id: view.opportunities[0].interaction_id.clone(),
                response: InteractionResponse::Shortcut {
                    decision: InteractionShortcutDecision::Fixed { iterations: 6 },
                    pins: vec![pin],
                },
            },
        )
    };

    let sequenced = preview(
        "p4-progress-sequenced",
        sequenced_pin(&points[0], &[(0, 1), (1, 2), (2, 3)]),
    );
    assert_eq!(
        sequenced.status,
        InteractionPreviewStatus::Confirmable,
        "reach-guard: the sequenced pin is accepted, so the progress below is the one this \
         offer publishes for it"
    );
    assert!(
        sequenced
            .progress
            .maximum
            .is_some_and(|maximum| sequenced.progress.selected <= maximum),
        "a sequenced pin answers its point's POSITIONS, not one per subject in the sequence. \
         got {:?}",
        sequenced.progress
    );

    let flat = preview(
        "p4-progress-flat",
        InteractionShortcutPin {
            group: points[0].group,
            choice_ids: vec![points[0].candidate_ids[0].clone()],
            amounts: Vec::new(),
        },
    );
    assert_eq!(
        flat.status,
        InteractionPreviewStatus::Confirmable,
        "reach-guard: the flat pin is accepted too"
    );
    assert_eq!(
        sequenced.progress, flat.progress,
        "the `.min(point.max)` is the IDENTITY on the unchanged path: three subjects at one \
         position publish the same progress one subject at that position does"
    );
}

// ── CR 732.2a: the round-trip preview of an AUTHORED allocation, driven on the tracked
//    4-player dump through the production `apply()` path. ────────────────────────────────────

/// One beat of the F4 drive policy, every beat crossing the public `apply()` boundary: at
/// priority always pass, aim every target choice at a CONSTANT seat (a board-stable cycle is
/// what the detector can certify), and take every optional-effect prompt.
fn f4_drive_one_beat(state: &mut GameState) {
    let who = state
        .waiting_for
        .acting_player()
        .unwrap_or_else(|| panic!("no acting player at {:?}", state.waiting_for));
    let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, who);
    let chosen = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        actions
            .iter()
            .find(|action| matches!(action, GameAction::PassPriority))
            .cloned()
    } else {
        actions
            .iter()
            .find(|action| {
                matches!(
                    action,
                    GameAction::ChooseTarget { target: Some(TargetRef::Player(seat)) }
                        if *seat == P1
                )
            })
            .or_else(|| {
                actions.iter().find(|action| {
                    matches!(action, GameAction::DecideOptionalEffect { accept: true })
                })
            })
            .cloned()
    };
    let action = chosen.unwrap_or_else(|| {
        panic!(
            "the F4 drive policy answers every beat it reaches; unhandled {:?}",
            state.waiting_for
        )
    });
    if let Err(error) = apply(state, who, action.clone()) {
        panic!("apply err ({action:?}): {error:?}");
    }
}

/// The tracked F4 dump at its CR 732.2a offer beat: restored through the production chokepoint
/// `PersistedGameState::into_game_state`, driven by the engine's own `apply()` until the offer
/// fires, and left with the interaction authority BOUND.
///
/// The offer beat is SEARCHED, never hardcoded. The proposer-only opportunity asserted below is
/// this helper's own liveness control: an unbound probe can read zero opportunities, and every
/// row built on this board would then be vacuous rather than negative.
pub(crate) fn f4_offer_board() -> (GameState, PlayerId) {
    use std::io::Read;

    let gz: &[u8] = include_bytes!("../fixtures/fantastic_four_bounded_loop_4p.json.gz");
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("the tracked fixture inflates to UTF-8 JSON");
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("the dump envelope parses as JSON");
    let mut state = serde_json::from_value::<engine::types::game_state::PersistedGameState>(
        envelope["gameState"].clone(),
    )
    .expect("the dump deserializes through the production decoder")
    .into_game_state()
    .expect("the persisted snapshot satisfies the checked restore contract");
    state.loop_detection = engine::types::game_state::LoopDetectionMode::Interactive;

    for _ in 0..400u32 {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        f4_drive_one_beat(&mut state);
    }
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for else {
        panic!(
            "the drive must reach the CR 732.2a bounded offer, got {:?}",
            state.waiting_for
        );
    };

    bind_interaction_authority(
        &mut state,
        InteractionSessionId("interaction-contract-f4-round-trip".to_string()),
    )
    .expect("the interaction authority binds over the live offer");

    assert_eq!(
        viewer_interaction(&state, proposer).opportunities.len(),
        1,
        "liveness control: the bound offer beat publishes the proposer's own opportunity. An \
         empty read means the authority never bound, and every row over this board would then \
         be reading a dead instrument rather than a negative"
    );
    let other = (0..u8::try_from(state.players.len()).expect("a board has few seats"))
        .map(PlayerId)
        .find(|seat| *seat != proposer)
        .expect("the tracked dump is multiplayer");
    assert!(
        viewer_interaction(&state, other).opportunities.is_empty(),
        "liveness control, second half: the offer beat is PROPOSER-ONLY, so a non-proposer \
         reading an opportunity means this projection is not the offer's"
    );

    (state, proposer)
}

/// The offer's own published count window and preview list, read off the projection under test.
fn f4_published(
    view: &engine::types::interaction::ViewerInteraction,
) -> (
    InteractionShortcutCountSpec,
    Vec<InteractionShortcutPreview>,
) {
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { count, preview, .. },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("the loop shortcut offer uses a shortcut schema");
    };
    (*count, preview.clone())
}

/// The announced `Targets` point the allocation is stated over — the FIRST one in published
/// order, which is the same rule the producer keys on.
fn f4_allocation_point(points: &[InteractionShortcutPoint]) -> InteractionShortcutPoint {
    points
        .iter()
        .find(|point| point.kind == InteractionShortcutPointKind::Targets)
        .expect("the F4 offer announces a Targets point")
        .clone()
}

/// Every pin this offer requires: the allocation over the announced `Targets` point through the
/// landed `sequenced_pin`, plus the first published candidate on every other answerable point.
///
/// Built from the offer's own published points, so a re-dump that renumbers or re-orders them
/// flows through without edit.
fn f4_pins(
    points: &[InteractionShortcutPoint],
    allocation: &[(usize, u32)],
) -> Vec<InteractionShortcutPin> {
    points
        .iter()
        .filter(|point| !point.read_only)
        .map(|point| {
            if point.kind == InteractionShortcutPointKind::Targets {
                sequenced_pin(point, allocation)
            } else {
                InteractionShortcutPin {
                    group: point.group,
                    choice_ids: vec![point.candidate_ids[0].clone()],
                    amounts: Vec::new(),
                }
            }
        })
        .collect()
}

fn f4_preview(
    state: &GameState,
    actor: PlayerId,
    interaction_id: &engine::types::interaction::InteractionId,
    count: u32,
    pins: Vec<InteractionShortcutPin>,
) -> engine::types::interaction::InteractionPreview {
    preview_interaction(state, actor, &f4_request(interaction_id, count, pins))
}

fn f4_request(
    interaction_id: &engine::types::interaction::InteractionId,
    count: u32,
    pins: Vec<InteractionShortcutPin>,
) -> InteractionPreviewRequest {
    InteractionPreviewRequest {
        request_id: PreviewRequestId("p7-round-trip".to_string()),
        interaction_id: interaction_id.clone(),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::Fixed { iterations: count },
            pins,
        },
    }
}

/// The allocation stated as `(candidate index, amount)` pairs, so a published element can be
/// resubmitted through the landed `sequenced_pin` without transcribing its ids.
fn indexed(point: &InteractionShortcutPoint, allocation: &[AmountAssignment]) -> Vec<(usize, u32)> {
    allocation
        .iter()
        .map(|assignment| {
            (
                point
                    .candidate_ids
                    .iter()
                    .position(|id| *id == assignment.choice_id)
                    .expect("a published allocation names the point's own published candidates"),
                assignment.amount,
            )
        })
        .collect()
}

/// **Row 1** — ONE producer, two call sites, and they agree: for EVERY element the offer
/// publishes, resubmitting that element's own allocation at its own count round-trips an
/// element BYTE-EQUAL to the published one.
///
/// # Discrimination
///
/// Re-derive the previewed entries at the new call site instead of calling the shared producer
/// and the two disagree on the first element compared.
#[test]
fn every_published_shortcut_element_round_trips_byte_equal() {
    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let point = f4_allocation_point(&points);
    let (count_spec, published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed { min, .. } = count_spec else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };

    // No reach-guard on filtered-vs-authoritative `waiting_for`: the two cannot diverge for a
    // `LoopShortcut` offer at either attach site. Both run `slot_for_submission` first, which
    // admits only the proposer's authorized submitter — the same predicate that gates the offer's
    // redaction in `filter_state_for_viewer` — and no other arm of that filter writes the variant.
    assert!(
        !published.is_empty(),
        "reach-guard: the offer must publish elements, else this row compares nothing"
    );

    for element in &published {
        assert!(
            !element.entries.is_empty(),
            "paired positive: every compared element must state magnitudes, else a byte-equality \
             between two empty elements satisfies this row; element {element:?}"
        );
        if element.count > min {
            assert!(
                element.allocation.len() > 1,
                "paired positive: above the window FLOOR an element spreads its count over more \
                 than one announced segment, which is what makes the allocation observable; \
                 element {element:?}"
            );
        }
        let preview = f4_preview(
            &state,
            proposer,
            &interaction_id,
            element.count,
            f4_pins(&points, &indexed(&point, &element.allocation)),
        );
        assert_eq!(
            preview.status,
            InteractionPreviewStatus::Confirmable,
            "an element's own published allocation is a declaration the ingress accepts"
        );
        assert_eq!(
            preview.shortcut_preview.as_ref(),
            Some(element),
            "CR 732.2a: the published element and the round-tripped one are minted by ONE \
             producer, so they cannot disagree at any published count"
        );
    }
}

/// **Row 2** — an AUTHORED, non-canonical distribution is previewed per DECLARED seat, and the
/// seat is resolved by `choice_id` rather than by position.
///
/// # Discrimination
///
/// Restore the positional `zip` in `VictimSplit::new` and two legs fall at once: the two
/// single-segment subsets return the SAME seat, and the reordered-unequal split returns the
/// in-order element's entries.
#[test]
fn an_authored_split_is_previewed_per_declared_seat() {
    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let point = f4_allocation_point(&points);
    let (count_spec, published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed { max, .. } = count_spec else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };
    assert!(
        point.candidate_ids.len() > 1 && max > 2,
        "reach-guard: an UNEQUAL split over MORE THAN ONE announced candidate is what makes a \
         per-seat attribution observable at all; candidates={} max={max}",
        point.candidate_ids.len()
    );

    let element = |allocation: &[(usize, u32)]| -> InteractionShortcutPreview {
        let preview = f4_preview(
            &state,
            proposer,
            &interaction_id,
            max,
            f4_pins(&points, allocation),
        );
        assert_eq!(
            preview.status,
            InteractionPreviewStatus::Confirmable,
            "an authored split summing to the declared count is a declaration the ingress \
             accepts: {allocation:?}"
        );
        let element = preview
            .shortcut_preview
            .expect("a confirmable authored declaration carries its previewed element");
        assert_eq!(
            element.count, max,
            "the element states the count it was declared at"
        );
        assert_eq!(
            indexed(&point, &element.allocation),
            allocation.to_vec(),
            "the element's allocation is exactly what was submitted"
        );
        assert!(
            !element.entries.is_empty(),
            "paired positive: an authored element must state magnitudes"
        );
        element
    };
    let life_seats = |element: &InteractionShortcutPreview| -> Vec<Option<u8>> {
        element
            .entries
            .iter()
            .filter(|entry| entry.family == InteractionShortcutPreviewFamily::Life)
            .map(|entry| entry.player)
            .collect()
    };
    // CR 119.3 governs the LIFE re-attribution and nothing else, so every other family is this
    // row's allocation-invariant control.
    let invariant = |element: &InteractionShortcutPreview| -> Vec<InteractionShortcutPreviewEntry> {
        element
            .entries
            .iter()
            .filter(|entry| entry.family != InteractionShortcutPreviewFamily::Life)
            .copied()
            .collect()
    };

    let canonical = published
        .iter()
        .find(|element| element.count == max)
        .expect("the window's own ceiling is always published")
        .clone();
    assert!(
        invariant(&canonical)
            .iter()
            .any(|entry| entry.player.is_some()),
        "reach-guard: the control needs a SEAT-KEYED entry no allocation may move, else \
         'identical across shapes' is a claim about whole-game entries only; got {:?}",
        canonical.entries
    );

    let unequal = element(&[(0, max - 1), (1, 1)]);
    let reordered = element(&[(1, max - 1), (0, 1)]);
    let first_only = element(&[(0, max)]);
    let second_only = element(&[(1, max)]);

    assert_ne!(
        reordered.entries, unequal.entries,
        "CR 601.2c: the allocation names its seats by announced choice, so a REORDERED \
         declaration carrying the same amount sequence attributes them differently. A positional \
         pairing would have produced the in-order element's entries here"
    );
    assert_eq!(
        life_seats(&first_only).len(),
        1,
        "a single-segment declaration charges a single seat"
    );
    assert_ne!(
        life_seats(&first_only),
        life_seats(&second_only),
        "CR 601.2c: a subset naming the SECOND announced candidate charges the SECOND seat. A \
         positional pairing returns the first seat for both"
    );
    for element in [&unequal, &reordered] {
        assert_eq!(
            life_seats(element).len(),
            element.allocation.len(),
            "paired positive: the element names as many charged seats as the declaration has \
             segments; a raw count-times-delta fold returns one"
        );
    }
    for (name, element) in [
        ("unequal", &unequal),
        ("reordered", &reordered),
        ("first-only", &first_only),
        ("second-only", &second_only),
    ] {
        assert_eq!(
            invariant(element),
            invariant(&canonical),
            "control: the entries CR 119.3 does not re-attribute are identical across the \
             canonical and every authored shape; {name} moved one"
        );
    }
}

/// **Row 3** — the two confirmable shapes the rows above never state: a pin that partitions
/// NOTHING, and a count taken from the offer's own suggestion rather than from the request.
///
/// A `Targets` pin carrying empty `amounts` names WHO without partitioning anything — the shape
/// the count-only arm submits. CR 732.2a leaves that declaration stated with no magnitude rather
/// than substituting the canonical order for an allocation nobody authored, and it is a legal
/// answer throughout: the absence is the producer's, not a refusal's.
///
/// # Discrimination
///
/// Mint an element for the empty pin — `canonical_allocation(&basis.ids, count)` is the natural
/// wrong answer — and leg 1's absence flips. Bind the `AcceptSuggested` count to anything but the
/// window's own suggestion and leg 2's count equality fails.
#[test]
fn an_unpartitioned_pin_states_no_magnitude_and_accept_suggested_states_the_offered_count() {
    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let point = f4_allocation_point(&points);
    let (count_spec, _published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed {
        min,
        max,
        suggested,
    } = count_spec
    else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };
    assert!(
        point.min <= 1 && point.max == 1 && point.candidate_ids.len() > 1,
        "reach-guard: ONE choice id must satisfy the announced point unpartitioned — otherwise \
         leg 1 is refused for its ARITY and never reaches the producer — and a second candidate \
         is what makes leg 2's split observable; min={} max={} candidates={}",
        point.min,
        point.max,
        point.candidate_ids.len()
    );

    // ── LEG 1's PAIRED POSITIVE: the same declaration WITH its partition stated.
    let partitioned = f4_preview(
        &state,
        proposer,
        &interaction_id,
        max,
        f4_pins(&points, &[(0, max)]),
    );
    assert_eq!(partitioned.status, InteractionPreviewStatus::Confirmable);
    assert!(
        partitioned
            .shortcut_preview
            .is_some_and(|element| !element.entries.is_empty()),
        "paired positive: the partitioned sibling states its magnitudes, so leg 1's absence is \
         about the empty `amounts` and not about this board"
    );

    // ── LEG 1: the same pin, its partition cleared.
    let unpartitioned: Vec<InteractionShortcutPin> = f4_pins(&points, &[(0, max)])
        .into_iter()
        .map(|mut pin| {
            if pin.group == point.group {
                pin.amounts.clear();
            }
            pin
        })
        .collect();
    let preview = f4_preview(&state, proposer, &interaction_id, max, unpartitioned);
    assert_eq!(
        preview.status,
        InteractionPreviewStatus::Confirmable,
        "an unpartitioned declaration is a legal answer, not a refusal"
    );
    assert!(
        preview.shortcut_preview.is_none(),
        "CR 732.2a: a pin stating no split states no magnitude — an element here is one the \
         engine invented from the canonical order"
    );

    // ── LEG 2: the count the OFFER suggests, which the request never names.
    assert!(
        suggested > 1 && suggested >= min,
        "reach-guard: the suggestion must sit inside the window and above its floor, else the \
         two-segment split below is refused; min={min} max={max} suggested={suggested}"
    );
    let accepted = preview_interaction(
        &state,
        proposer,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("accept-suggested".to_string()),
            interaction_id: interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::AcceptSuggested,
                pins: f4_pins(&points, &[(0, suggested - 1), (1, 1)]),
            },
        },
    );
    assert_eq!(accepted.status, InteractionPreviewStatus::Confirmable);
    let element = accepted
        .shortcut_preview
        .expect("accepting the suggestion is confirmable, so it carries its previewed element");
    assert_eq!(
        element.count, suggested,
        "CR 732.2a: accepting the suggestion states the count the OFFER published"
    );
    assert!(
        !element.entries.is_empty() && element.allocation.len() > 1,
        "paired positive: the accepted element states magnitudes over more than one segment, so \
         the count equality above is not read off an empty element"
    );
}

/// Re-bind the interaction authority over an EDITED offer board and hand back its proposer. The
/// edit moves the published schema, so the offer's ids are re-minted under a fresh session rather
/// than carried over.
fn f4_rebound(mut state: GameState, session: &str) -> (GameState, PlayerId) {
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for else {
        panic!(
            "the edited board must still sit at the CR 732.2a offer, got {:?}",
            state.waiting_for
        );
    };
    bind_interaction_authority(&mut state, InteractionSessionId(session.to_string()))
        .expect("the interaction authority binds over the edited offer");
    assert_eq!(
        viewer_interaction(&state, proposer).opportunities.len(),
        1,
        "liveness control: the edited board still publishes the proposer's own opportunity, so a \
         refusal below is the ingress's answer and not a dead projection"
    );
    (state, proposer)
}

/// The F4 offer board with its announced `Targets` point made OPTIONAL (`min_targets == 0`),
/// carried through `PersistedGameState::into_game_state` so the bound under test is one the
/// production restore contract admits.
fn f4_optional_announcement_board() -> (GameState, PlayerId) {
    let (mut state, _) = f4_offer_board();
    {
        let WaitingFor::LoopShortcut { schema, .. } = &mut state.waiting_for else {
            panic!("the F4 board sits at the CR 732.2a offer");
        };
        let point = schema
            .points
            .iter_mut()
            .find(|point| matches!(point.kind, DecisionPointKind::Targets { .. }))
            .expect("the F4 offer announces a Targets point");
        let DecisionPointKind::Targets { min_targets, .. } = &mut point.kind else {
            unreachable!("found under that same predicate")
        };
        *min_targets = 0;
    }
    let restored = engine::types::game_state::PersistedGameState::capture(state)
        .into_game_state()
        .expect("the edited snapshot satisfies the checked restore contract");
    f4_rebound(restored, "interaction-contract-f4-optional")
}

/// The F4 offer board publishing a SECOND announced-target point — the schema's own `Targets`
/// point duplicated under a distinct `DecisionSlot` index, which is what makes both publish.
fn f4_second_announcement_board() -> (GameState, PlayerId) {
    let (mut state, _) = f4_offer_board();
    {
        let WaitingFor::LoopShortcut { schema, .. } = &mut state.waiting_for else {
            panic!("the F4 board sits at the CR 732.2a offer");
        };
        let mut duplicate = schema
            .points
            .iter()
            .find(|point| matches!(point.kind, DecisionPointKind::Targets { .. }))
            .expect("the F4 offer announces a Targets point")
            .clone();
        duplicate.slot.index = duplicate
            .slot
            .index
            .checked_add(1)
            .expect("the announced slot's sub-index leaves room for a sibling");
        schema.points.push(duplicate);
    }
    f4_rebound(state, "interaction-contract-f4-two-announcements")
}

/// The announced `Targets` schema point's slot and the seats its candidates name, in published
/// order — the two halves the submitted announcement is compared against.
fn f4_announced_slot(state: &GameState) -> (DecisionSlot, Vec<PlayerId>) {
    let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
        panic!("the board sits at the CR 732.2a offer");
    };
    let point = schema
        .points
        .iter()
        .find(|point| matches!(point.kind, DecisionPointKind::Targets { .. }))
        .expect("the F4 offer announces a Targets point");
    let DecisionPointKind::Targets { legal_targets, .. } = &point.kind else {
        unreachable!("found under that same predicate")
    };
    (
        point.slot.clone(),
        legal_targets
            .iter()
            .map(|target| match target {
                TargetRef::Player(seat) => *seat,
                TargetRef::Object(id) => panic!("the F4 offer announces seats, got object {id:?}"),
            })
            .collect(),
    )
}

/// **Row 3b** — a pin naming NOTHING is answered, and accepted, with the offer's own canonical
/// split at a count the published sample omits. The complement of the unpartitioned-pin row
/// above: that one pins the shape naming WHO without a magnitude, this one the shape naming
/// neither.
///
/// # Discrimination
///
/// Each leg names its own failing change:
/// * leg 1 — without the completion the request is `Rejected { ConstraintUnsatisfied }` with no
///   element on the answering entry point and `Err` on the other;
/// * leg 2 — consult the completion at the preview entry points instead of at the ingress and
///   this leg is `Err` while leg 1 passes, which is the divergence itself;
/// * leg 3 — mint anything but `canonical_allocation` over the domain's ids and the equality
///   against the offer's own published element fails;
/// * leg 4 — drop the `min == 1` conjunct and an element appears where an empty announcement is
///   already the declaration;
/// * leg 5 — drop the sole-point conjunct and the answered-second member turns `Confirmable` and
///   submits, while the both-empty member stays refused either way.
#[test]
fn a_nothing_naming_pin_is_completed_with_the_offers_canonical_split() {
    use engine::analysis::decision_template::PinnedDecision;

    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let point = f4_allocation_point(&points);
    let (count_spec, published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed { min, max, .. } = count_spec else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };
    assert!(
        (point.min, point.max) == (1, 1) && point.candidate_ids.len() > 1,
        "reach-guard: the completion fires only on a MANDATORY single-subject announced point, \
         and a second candidate is what makes its split observable; min={} max={} candidates={}",
        point.min,
        point.max,
        point.candidate_ids.len()
    );
    // `f4_pins(.., &[])` builds the nothing-naming pin over the announced point — no ids and no
    // amounts — while every other answerable point is answered from its own candidate list.
    let nothing_named = f4_pins(&points, &[]);
    assert!(
        nothing_named.iter().any(|pin| pin.group == point.group
            && pin.choice_ids.is_empty()
            && pin.amounts.is_empty()),
        "reach-guard: the request under test must NAME NOTHING over the announced point, else \
         every leg below is about some other pin shape"
    );

    // ── LEG 1: the first in-window count the offer's bounded sample published no element for.
    let unsampled = (min..=max)
        .find(|count| !published.iter().any(|element| element.count == *count))
        .expect(
            "reach-guard: the payload cap must omit a count inside the offer's own window, else \
             this row is about a fully sampled window and asserts nothing",
        );
    let answered = f4_preview(
        &state,
        proposer,
        &interaction_id,
        unsampled,
        nothing_named.clone(),
    );
    assert_eq!(
        answered.status,
        InteractionPreviewStatus::Confirmable,
        "CR 732.2a: the count is the proposer's to specify, and a pin naming nothing defers to \
         the offer's own split — a payload cap on which counts PUBLISH an element cannot narrow \
         which may be declared"
    );
    let element = answered
        .shortcut_preview
        .clone()
        .expect("a confirmable completed declaration carries its previewed element");
    assert_eq!(
        element.count, unsampled,
        "the element states the count the request declared"
    );
    assert!(
        !element.entries.is_empty(),
        "paired positive: the completed element states magnitudes, so the equalities below are \
         not read off an empty element"
    );
    assert!(
        !element.allocation.is_empty() && element.allocation.iter().all(|part| part.amount > 0),
        "CR 601.2c: every announced segment takes at least one repetition; {:?}",
        element.allocation
    );
    assert_eq!(
        element
            .allocation
            .iter()
            .map(|part| part.amount)
            .sum::<u32>(),
        unsampled,
        "the split is a partition OF the declared count"
    );
    let rejecting = preview_interaction_with_rejection(
        &state,
        proposer,
        &f4_request(&interaction_id, unsampled, nothing_named.clone()),
    )
    .expect("the completed declaration answers on the rejection-typed entry point too");
    assert_eq!(
        rejecting.shortcut_preview.as_ref(),
        Some(&element),
        "both preview entry points answer one request with one element"
    );

    // ── LEG 2: the same request SUBMITS, carrying that allocation as a sequenced announcement.
    let mut submitted = state.clone();
    let applied = submit_interaction(
        &mut submitted,
        proposer,
        InteractionSubmission {
            interaction_id: interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::Fixed {
                    iterations: unsampled,
                },
                pins: nothing_named.clone(),
            },
        },
    )
    .expect("what previews confirmable submits: the completion sits at the chokepoint both share");
    let GameAction::DeclareShortcut {
        count,
        template: Some(template),
    } = &applied.action
    else {
        panic!(
            "a declared shortcut submits a template, got {:?}",
            applied.action
        );
    };
    assert_eq!(
        *count,
        IterationCount::Fixed(unsampled),
        "the accepted action drives the count the request declared"
    );
    let (announced_slot, announced_seats) = f4_announced_slot(&state);
    let segments = indexed(&point, &element.allocation);
    let starts: Vec<u32> = segments
        .iter()
        .scan(0u32, |start, (_, amount)| {
            let at = *start;
            *start += amount;
            Some(at)
        })
        .collect();
    let seats: Vec<PlayerId> = segments
        .iter()
        .map(|(candidate, _)| announced_seats[*candidate])
        .collect();
    assert!(
        template.decisions.contains(&piecewise_pin(
            announced_slot,
            &starts,
            &seat_subjects(&seats)
        )),
        "CR 601.2c: the accepted declaration announces the completed split per repetition; \
         got {:?}",
        template.decisions
    );

    // ── LEG 3: the SAMPLED control — the completion's answer is the offer's own published one.
    let sampled = published
        .iter()
        .find(|candidate| {
            candidate.allocation.len() > 1
                && candidate
                    .allocation
                    .iter()
                    .any(|part| part.amount != candidate.allocation[0].amount)
        })
        .expect(
            "reach-guard: a published NON-UNIFORM multi-segment element is what keeps an even \
             split from satisfying the equality below",
        );
    let control = f4_preview(
        &state,
        proposer,
        &interaction_id,
        sampled.count,
        nothing_named.clone(),
    );
    assert_eq!(control.status, InteractionPreviewStatus::Confirmable);
    assert_eq!(
        control.shortcut_preview.as_ref(),
        Some(sampled),
        "CR 732.2a: at a count the offer DID publish, the completion states that published \
         element — allocation and entries alike"
    );

    // ── LEG 4: the OPTIONAL end of the class, where the empty pin is ALREADY a declaration.
    let (optional_state, optional_proposer) = f4_optional_announcement_board();
    let optional_view = viewer_interaction(&optional_state, optional_proposer);
    let optional_id = optional_view.opportunities[0].interaction_id.clone();
    let optional_points = shortcut_points(&optional_view);
    let optional_point = f4_allocation_point(&optional_points);
    assert_eq!(
        (optional_point.min, optional_point.max),
        (0, 1),
        "reach-guard: the restored board must publish the OPTIONAL bound, else this leg is the \
         mandatory one over again"
    );
    let optional_pins = f4_pins(&optional_points, &[]);
    let optional_preview = f4_preview(
        &optional_state,
        optional_proposer,
        &optional_id,
        unsampled,
        optional_pins.clone(),
    );
    assert_eq!(
        optional_preview.status,
        InteractionPreviewStatus::Confirmable,
        "an OPTIONAL announced point's empty pin is a legal answer, exactly as it is today"
    );
    assert!(
        optional_preview.shortcut_preview.is_none(),
        "CR 601.2c: at `min == 0` the empty pin already MEANS 'announce no target here', so \
         completing it would replace one stated declaration with a different one"
    );
    let mut optional_submitted = optional_state.clone();
    let optional_applied = submit_interaction(
        &mut optional_submitted,
        optional_proposer,
        InteractionSubmission {
            interaction_id: optional_id,
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::Fixed {
                    iterations: unsampled,
                },
                pins: optional_pins,
            },
        },
    )
    .expect("the optional announcement submits, as it does today");
    let GameAction::DeclareShortcut {
        template: Some(optional_template),
        ..
    } = &optional_applied.action
    else {
        panic!(
            "a declared shortcut submits a template, got {:?}",
            optional_applied.action
        );
    };
    assert!(
        optional_template
            .decisions
            .contains(&PinnedDecision::Targets {
                slot: f4_announced_slot(&optional_state).0,
                targets: Vec::new(),
            }),
        "the EMPTY announcement is what submits; got {:?}",
        optional_template.decisions
    );

    // ── LEG 5: a SECOND announced-target point leaves the offer's one published split an
    //    incomplete answer, so nothing is completed on either member.
    let (spliced_state, spliced_proposer) = f4_second_announcement_board();
    let spliced_view = viewer_interaction(&spliced_state, spliced_proposer);
    let spliced_id = spliced_view.opportunities[0].interaction_id.clone();
    let spliced_points = shortcut_points(&spliced_view);
    let announced: Vec<&InteractionShortcutPoint> = spliced_points
        .iter()
        .filter(|candidate| candidate.kind == InteractionShortcutPointKind::Targets)
        .collect();
    assert_eq!(
        announced.len(),
        2,
        "reach-guard: the spliced board must publish TWO announced-target points, else this leg \
         is the single-point board over again"
    );
    let second_group = announced[1].group;
    let answered_second: Vec<InteractionShortcutPin> = spliced_points
        .iter()
        .filter(|candidate| !candidate.read_only)
        .map(|candidate| match candidate.kind {
            InteractionShortcutPointKind::Targets if candidate.group == second_group => {
                sequenced_pin(candidate, &[(0, unsampled)])
            }
            InteractionShortcutPointKind::Targets => sequenced_pin(candidate, &[]),
            _ => InteractionShortcutPin {
                group: candidate.group,
                choice_ids: vec![candidate.candidate_ids[0].clone()],
                amounts: Vec::new(),
            },
        })
        .collect();
    for (name, pins) in [
        ("answered-second", answered_second),
        ("both-empty", f4_pins(&spliced_points, &[])),
    ] {
        let refused = f4_preview(
            &spliced_state,
            spliced_proposer,
            &spliced_id,
            unsampled,
            pins.clone(),
        );
        assert_eq!(
            refused.status,
            InteractionPreviewStatus::Rejected {
                reason: InteractionReasonCode::ConstraintUnsatisfied
            },
            "CR 601.2c: with a second announced-target point published the offer's one split is \
             not a complete answer, so the {name} member keeps today's refusal"
        );
        assert!(
            refused.shortcut_preview.is_none(),
            "a refused declaration states no magnitude; {name} rendered one"
        );
        assert!(
            preview_interaction_with_rejection(
                &spliced_state,
                spliced_proposer,
                &f4_request(&spliced_id, unsampled, pins.clone()),
            )
            .is_err(),
            "the rejection-typed entry point refuses the {name} member too"
        );
        let mut spliced_submitted = spliced_state.clone();
        assert!(
            submit_interaction(
                &mut spliced_submitted,
                spliced_proposer,
                InteractionSubmission {
                    interaction_id: spliced_id.clone(),
                    response: InteractionResponse::Shortcut {
                        decision: InteractionShortcutDecision::Fixed {
                            iterations: unsampled,
                        },
                        pins,
                    },
                },
            )
            .is_err(),
            "the submit path refuses the {name} member too"
        );
    }

    // ── HOSTILE SIBLINGS, all three at the SAME unsampled count leg 1 completes.
    // A pin naming ONE choice id with empty amounts states WHO without partitioning anything —
    // still confirmable, and still carrying no element, because the completion fires only on a
    // pin naming nothing.
    let one_named: Vec<InteractionShortcutPin> = nothing_named
        .iter()
        .cloned()
        .map(|mut pin| {
            if pin.group == point.group {
                pin.choice_ids = vec![point.candidate_ids[0].clone()];
            }
            pin
        })
        .collect();
    let unpartitioned = f4_preview(&state, proposer, &interaction_id, unsampled, one_named);
    assert_eq!(
        unpartitioned.status,
        InteractionPreviewStatus::Confirmable,
        "an unpartitioned declaration stays a legal answer at an unsampled count"
    );
    assert!(
        unpartitioned.shortcut_preview.is_none(),
        "CR 732.2a: a pin NAMING a subject states no split, so the completion leaves it alone"
    );
    // Dropping another point's pin entirely: the completion substitutes the announced group and
    // nothing else, so a declaration another point leaves unanswered stays refused.
    let missing_other = nothing_named
        .iter()
        .filter(|pin| pin.group == point.group)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        missing_other.len() < nothing_named.len(),
        "reach-guard: the offer must publish a second answerable point for this sibling to drop"
    );
    let incomplete = f4_preview(&state, proposer, &interaction_id, unsampled, missing_other);
    assert_eq!(
        incomplete.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ConstraintUnsatisfied
        },
        "the completion is group-local: it cannot carry a declaration another point leaves \
         unanswered"
    );
    assert!(incomplete.shortcut_preview.is_none());
    // The window is `materialize_loop_shortcut_response`'s single authority, and the completion
    // does not restate it: an out-of-window count is refused after the substitution as before it.
    let out_of_window = f4_preview(&state, proposer, &interaction_id, max + 1, nothing_named);
    assert_eq!(
        out_of_window.status,
        InteractionPreviewStatus::Rejected {
            reason: InteractionReasonCode::ConstraintUnsatisfied
        },
        "CR 732.2a: a count outside the offer's own window is refused, completion or not"
    );
    assert!(out_of_window.shortcut_preview.is_none());
}

/// **Row 4** — a declaration the ingress REFUSES carries no magnitude, each refusal asserted by
/// the reason the ingress actually emits for it.
///
/// # Discrimination
///
/// Attach the payload above the simulation match — i.e. on the rejected closure too — and every
/// refusal leg renders a magnitude.
#[test]
fn a_refused_shortcut_declaration_carries_no_previewed_magnitude() {
    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let point = f4_allocation_point(&points);
    let (count_spec, _published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed { max, .. } = count_spec else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };
    assert!(
        point.candidate_ids.len() > 1 && max > 2,
        "reach-guard: the duplicate-id and subset shapes need more than one announced candidate"
    );

    // ── MANDATORY PAIRED POSITIVE: the same declaration made legal answers WITH the payload, so
    //    no leg below can be satisfied by an upstream short-circuit that refuses everything.
    let legal = f4_preview(
        &state,
        proposer,
        &interaction_id,
        max,
        f4_pins(&points, &[(0, max - 1), (1, 1)]),
    );
    assert_eq!(legal.status, InteractionPreviewStatus::Confirmable);
    assert!(
        legal
            .shortcut_preview
            .is_some_and(|element| !element.entries.is_empty()),
        "paired positive: the legal sibling of every refusal below carries a non-empty element"
    );

    let unknown_id = InteractionChoiceId("k-not-published".to_string());
    let refusals: Vec<(&str, Vec<InteractionShortcutPin>, InteractionReasonCode)> = vec![
        (
            "sum below the declared count",
            f4_pins(&points, &[(0, max - 1)]),
            InteractionReasonCode::ConstraintUnsatisfied,
        ),
        (
            "a zero segment",
            f4_pins(&points, &[(0, max), (1, 0)]),
            InteractionReasonCode::ConstraintUnsatisfied,
        ),
        (
            "a duplicate choice id",
            f4_pins(&points, &[(0, max - 1), (0, 1)]),
            InteractionReasonCode::ConstraintUnsatisfied,
        ),
        (
            "a choice id the point does not publish",
            points
                .iter()
                .filter(|p| !p.read_only)
                .map(|p| {
                    if p.kind == InteractionShortcutPointKind::Targets {
                        InteractionShortcutPin {
                            group: p.group,
                            choice_ids: vec![unknown_id.clone()],
                            amounts: vec![AmountAssignment {
                                choice_id: unknown_id.clone(),
                                amount: max,
                            }],
                        }
                    } else {
                        InteractionShortcutPin {
                            group: p.group,
                            choice_ids: vec![p.candidate_ids[0].clone()],
                            amounts: Vec::new(),
                        }
                    }
                })
                .collect(),
            InteractionReasonCode::UnknownChoice,
        ),
    ];

    for (name, pins, reason) in refusals {
        let preview = f4_preview(&state, proposer, &interaction_id, max, pins);
        assert_eq!(
            preview.status,
            InteractionPreviewStatus::Rejected { reason },
            "{name} is refused by the reason the ingress emits for it"
        );
        assert!(
            preview.shortcut_preview.is_none(),
            "CR 732.2a: a declaration the engine refused states no magnitude; {name} rendered one"
        );
    }
}

/// **Row 5** — the preview COMMITS nothing, paired against the submission that does.
///
/// # Discrimination
///
/// The paired positive is written on what a `DeclareShortcut` actually changes — the pending
/// interaction and the whole serialized state — never on a life delta: the declaration declares,
/// and the magnitudes land at the accept beat.
#[test]
fn previewing_a_shortcut_declaration_commits_nothing() {
    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let point = f4_allocation_point(&points);
    let (count_spec, published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed { max, .. } = count_spec else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };
    let ceiling = published
        .iter()
        .find(|element| element.count == max)
        .expect("the window's own ceiling is always published")
        .clone();
    let pins = f4_pins(&points, &indexed(&point, &ceiling.allocation));

    let before = serde_json::to_value(&state).expect("the authoritative state serializes");
    let confirmable = f4_preview(&state, proposer, &interaction_id, max, pins.clone());
    assert_eq!(confirmable.status, InteractionPreviewStatus::Confirmable);
    assert_eq!(
        serde_json::to_value(&state).expect("the authoritative state serializes"),
        before,
        "CR 732.1b: the sequence is never performed, so a preview at the published ceiling \
         leaves the authoritative state byte-identical"
    );

    let refused = f4_preview(
        &state,
        proposer,
        &interaction_id,
        max,
        f4_pins(&points, &[(0, max - 1)]),
    );
    assert!(matches!(
        refused.status,
        InteractionPreviewStatus::Rejected { .. }
    ));
    assert_eq!(
        serde_json::to_value(&state).expect("the authoritative state serializes"),
        before,
        "previewing a REFUSED declaration is inert too"
    );

    // ── PAIRED POSITIVE: the same payload through the mutating path DOES move the board, so
    //    the byte-equality above is a property of previewing rather than of an inert payload.
    let mut submitted = state.clone();
    submit_interaction(
        &mut submitted,
        proposer,
        InteractionSubmission {
            interaction_id,
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::Fixed { iterations: max },
                pins,
            },
        },
    )
    .expect("the previewed declaration is submittable");
    assert!(
        !matches!(submitted.waiting_for, WaitingFor::LoopShortcut { .. }),
        "paired positive: submitting moves the pending interaction off the offer"
    );
    assert_ne!(
        serde_json::to_value(&submitted).expect("the submitted state serializes"),
        before,
        "paired positive: submitting changes the authoritative state"
    );
}

/// **Row 5b** — both preview entry points answer a CONFIRMABLE declaration with the SAME
/// element, from one request object handed to each.
///
/// # Discrimination
///
/// Attach the payload on one entry point only and the equality fails with one side `None`. The
/// paired positive is what keeps that from being an equality between two absences: both sides
/// must carry a non-empty `entries` and more than one allocation segment.
///
/// Refusal is deliberately NOT compared across the two: `preview_interaction_with_rejection`
/// `?`-returns `Err(ActionRejection)` and constructs no answer at all on that path, so there is
/// no second payload to compare. Refused-carries-nothing is asserted where it discriminates, on
/// the answering entry point.
#[test]
fn both_preview_entry_points_answer_with_the_same_shortcut_element() {
    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let (count_spec, _published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed { max, .. } = count_spec else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };
    let request = f4_request(
        &interaction_id,
        max,
        f4_pins(&points, &[(0, max - 1), (1, 1)]),
    );

    let answered = preview_interaction(&state, proposer, &request);
    let rejecting = preview_interaction_with_rejection(&state, proposer, &request)
        .expect("a confirmable declaration answers on the rejection-typed entry point too");
    assert_eq!(answered.status, InteractionPreviewStatus::Confirmable);
    assert_eq!(rejecting.status, InteractionPreviewStatus::Confirmable);

    let element = answered
        .shortcut_preview
        .as_ref()
        .expect("paired positive: the answering entry point carries an element");
    assert!(
        !element.entries.is_empty() && element.allocation.len() > 1,
        "paired positive: an equality between two ABSENT payloads is exactly the failure this \
         row exists to replace, so both sides must carry a stated, multi-segment element"
    );
    assert_eq!(
        answered.shortcut_preview, rejecting.shortcut_preview,
        "the two entry points answer one request with one element"
    );
}

/// **Row 6** — the new payload is ADDITIVE on the wire, asserted on both status arms.
///
/// # Discrimination
///
/// Drop `skip_serializing_if` and the omission leg fails. The absent-key leg's failing change is
/// removing the `Option` wrapper, NOT dropping `#[serde(default)]` — a missing `Option` field
/// decodes to `None` either way, so no leg here claims that attribute is load-bearing.
#[test]
fn the_shortcut_preview_payload_is_additive_on_the_wire() {
    let (state, proposer) = f4_offer_board();
    let view = viewer_interaction(&state, proposer);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let points = shortcut_points(&view);
    let (count_spec, _published) = f4_published(&view);
    let InteractionShortcutCountSpec::Fixed { max, .. } = count_spec else {
        panic!("the F4 offer publishes a Fixed count window, got {count_spec:?}");
    };

    let populated = f4_preview(
        &state,
        proposer,
        &interaction_id,
        max,
        f4_pins(&points, &[(0, max - 1), (1, 1)]),
    );
    let refused = f4_preview(
        &state,
        proposer,
        &interaction_id,
        max,
        f4_pins(&points, &[(0, max - 1)]),
    );
    assert!(matches!(
        refused.status,
        InteractionPreviewStatus::Rejected { .. }
    ));
    let mut confirmable_without = populated.clone();
    confirmable_without.shortcut_preview = None;

    let populated_json = serde_json::to_value(&populated).expect("the preview serializes");
    // ── POSITIVE CONTROL: a NON-EMPTY payload IS emitted, else both absence legs below are
    //    satisfied by a serializer that writes no key under any circumstances.
    assert!(
        populated_json
            .pointer("/shortcutPreview/entries")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|entries| !entries.is_empty()),
        "positive control: a populated payload emits a non-empty `/shortcutPreview/entries`"
    );
    assert_eq!(
        serde_json::from_value::<engine::types::interaction::InteractionPreview>(
            populated_json.clone()
        )
        .expect("a populated payload reads"),
        populated,
        "a populated payload round-trips unchanged"
    );

    for (arm, preview) in [
        ("confirmable", &confirmable_without),
        ("rejected", &refused),
    ] {
        let json = serde_json::to_value(preview).expect("the preview serializes");
        assert!(
            json.pointer("/shortcutPreview").is_none(),
            "an absent payload emits no `shortcutPreview` key on the {arm} arm"
        );
        assert_eq!(
            &serde_json::from_value::<engine::types::interaction::InteractionPreview>(json.clone())
                .expect("an absent payload reads"),
            preview,
            "JSON written before this field existed still deserializes on the {arm} arm"
        );

        let mut explicit_null = json;
        explicit_null["shortcutPreview"] = serde_json::Value::Null;
        assert_eq!(
            &serde_json::from_value::<engine::types::interaction::InteractionPreview>(
                explicit_null
            )
            .expect("an explicit null payload reads"),
            preview,
            "an explicit `null` reads as absent on the {arm} arm — the spelling the generated \
             binding emits beside the optional one"
        );
    }
}

/// **Row 7** — an interaction whose response model is not `LoopShortcut` carries no payload.
///
/// # Discrimination
///
/// Make the response-side producer fall through to a default element instead of refusing a
/// non-`Shortcut` response and the absence leg fails.
#[test]
fn a_non_shortcut_preview_carries_no_shortcut_payload() {
    let mut state = GameState::new_two_player(42);
    bind(&mut state, "non-shortcut-preview");
    let view = priority_view(&state);
    let interaction_id = view.opportunities[0].interaction_id.clone();
    let InteractionAvailability::ProgressAvailable { witness } = view.availability else {
        panic!("priority must expose a real progress witness");
    };

    let preview = preview_interaction(
        &state,
        P0,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("p7-non-shortcut".to_string()),
            interaction_id,
            response: witness.response,
        },
    );
    // ── PAIRED POSITIVE: the preview path still ANSWERS this response model, so the absence
    //    below is that model's own answer rather than a path that had started refusing all.
    assert_eq!(preview.status, InteractionPreviewStatus::Confirmable);
    assert!(preview.progress.confirmable);
    assert!(matches!(
        preview.outcome,
        InteractionOutcomeCode::Advanced | InteractionOutcomeCode::Replaced
    ));
    assert!(
        preview.shortcut_preview.is_none(),
        "only a shortcut declaration states a shortcut magnitude"
    );
}

// ═════════════════════════════════════════════════════════════════════════════════════════
// PHASE 10 — a shortcut proposal states no announcement the drive makes (CR 732.2c).
// ═════════════════════════════════════════════════════════════════════════════════════════

/// A ranking over `seats`, in the order given.
fn seat_rank(seats: &[PlayerId]) -> engine::analysis::decision_template::Ranking {
    engine::analysis::decision_template::Ranking::new(seat_subjects(seats))
        .expect("distinct seats make a legal ranking")
}

/// The single-position `Constant` schedule naming `seats`.
fn constant_of(seats: &[PlayerId]) -> engine::analysis::decision_template::TargetPin {
    use engine::analysis::decision_template::{TargetPin, TargetSchedule};
    TargetPin::Scheduled(TargetSchedule::Constant(seat_rank(seats)))
}

/// A `Piecewise` schedule from `(start, seats)` pairs, in declared order.
fn piecewise_of(steps: &[(u32, &[PlayerId])]) -> engine::analysis::decision_template::TargetPin {
    use engine::analysis::decision_template::{TargetPin, TargetSchedule};
    TargetPin::Scheduled(TargetSchedule::Piecewise(
        steps
            .iter()
            .map(|(start, seats)| (*start, seat_rank(seats)))
            .collect(),
    ))
}

/// A `RoundRobin` schedule over `steps`, in declared order.
fn round_robin_of(steps: &[&[PlayerId]]) -> engine::analysis::decision_template::TargetPin {
    use engine::analysis::decision_template::{TargetPin, TargetSchedule};
    TargetPin::Scheduled(TargetSchedule::RoundRobin(
        steps.iter().map(|seats| seat_rank(seats)).collect(),
    ))
}

/// One `Targets` pin over `slot`, as the whole declaration a proposer sends.
fn targets_template(
    slot: &DecisionSlot,
    count: IterationCount,
    targets: Vec<engine::analysis::decision_template::TargetPin>,
) -> engine::analysis::decision_template::DecisionTemplate {
    use engine::analysis::decision_template::{
        DecisionGroupKey, DecisionKind, DecisionTemplate, PinnedDecision, ReplayMode,
    };
    DecisionTemplate {
        // The offer's proposer. Bound here rather than left free because the CR 603.5 `owner`
        // firewall sits AHEAD of `declaration_conforms` and lands on the same handback, so a
        // foreign owner would make every leg below measure that firewall instead.
        owner: P0,
        decisions: vec![PinnedDecision::Targets {
            slot: slot.clone(),
            targets,
        }],
        replay: ReplayMode::Scheduled {
            count: count.clone(),
        },
        key: DecisionGroupKey::from_sources(
            std::slice::from_ref(&slot.source),
            DecisionKind::LoopChoice,
        ),
    }
}

/// The verdict the PRODUCTION declare ingress reaches for `targets` declared over one
/// `victims_point(min, max)`: `WaitingFor::Priority` is the manual-play handback every refusal
/// arm lands on, `WaitingFor::RespondToShortcut` is the APNAP window an acceptance opens.
/// `act` returns `Ok` either way, so the verdict is read here and nowhere else.
///
/// Each call stages its OWN offer: a declaration consumes it, so one runner cannot serve a
/// second leg.
///
/// ⚠ `max_iterations` is a parameter and not a constant because
/// `handle_declare_shortcut` refuses `UntilLethal` outright on a NARROWED bound, before any
/// pin is read. An until-lethal leg staged that way lands on `Priority` for a reason with
/// nothing to do with the pin, and every leg then agrees.
fn declare_targets_verdict(
    label: &str,
    count: IterationCount,
    max_iterations: u32,
    positions: (u32, u32),
    targets: Vec<engine::analysis::decision_template::TargetPin>,
) -> WaitingFor {
    let (mut runner, slots) = stage_sequenced_offer(
        label,
        count.clone(),
        max_iterations,
        vec![victims_point(positions.0, positions.1)],
    );
    let template = targets_template(&slots[0], count.clone(), targets);
    runner
        .act(GameAction::DeclareShortcut {
            count,
            template: Some(template),
        })
        .expect("the declare ingress answers a staged offer rather than erroring");
    runner.state().waiting_for.clone()
}

fn refused(verdict: &WaitingFor) -> bool {
    matches!(verdict, WaitingFor::Priority { .. })
}

fn accepted(verdict: &WaitingFor) -> bool {
    matches!(verdict, WaitingFor::RespondToShortcut { .. })
}

/// **Row 1** — the gap, closed on its strongest member, at the production declare ingress.
///
/// A `Constant` naming its head plus a subject the offer never published is ACCEPTED at BASE:
/// index 0 resolves the head, which IS a published legal target, so nothing but the new
/// reachability clause can refuse it.
///
/// # Discrimination
///
/// Every hostile ranking here names EXACTLY TWO subjects. A head test loosened by one
/// (`nth(2).is_none()`, "at most two") is verdict-identical to the shipped clause on a
/// three-entry ranking and would admit these, so the size is the discrimination and not a
/// convenience.
///
/// The third leg is a CONTROL, not a positive: it is refused at BASE and at the tip alike, by
/// the pre-existing value-legality check both times, and it differs from the paired positive in
/// exactly the head's publication status — which is what proves the board really withholds that
/// subject.
#[test]
fn p10_row_1_a_declaration_naming_an_unread_announcement_is_refused_at_the_declare_ingress() {
    let unbounded = ShortcutDecisionSchema::default().max_iterations;

    let hostile = declare_targets_verdict(
        "p10-row1-hostile",
        IterationCount::UntilLethal,
        unbounded,
        (1, 1),
        vec![constant_of(&[P1, P0])],
    );
    assert!(
        refused(&hostile),
        "CR 732.2c: a proposal may not certify an announcement no index resolves. got {hostile:?}"
    );

    let head_only = declare_targets_verdict(
        "p10-row1-positive",
        IterationCount::UntilLethal,
        unbounded,
        (1, 1),
        vec![constant_of(&[P1])],
    );
    assert!(
        accepted(&head_only),
        "PAIRED POSITIVE: the SAME declaration with its tail deleted is accepted on the same \
         board through the same call, so the refusal above is the new clause and not a broken \
         board. got {head_only:?}"
    );

    let unpublished_head = declare_targets_verdict(
        "p10-row1-control",
        IterationCount::UntilLethal,
        unbounded,
        (1, 1),
        vec![constant_of(&[P0])],
    );
    assert!(
        refused(&unpublished_head),
        "CONTROL: a one-entry ranking states its head, so the new clause is silent on it — this \
         is the pre-existing CR 608.2b value-legality refusal, and it is what shows the board \
         really withholds this subject. got {unpublished_head:?}"
    );
}

/// **Row 2** — the class, at both ends, on one board, with both axes pinned.
///
/// Declared count `Fixed(1)` throughout, so the validated range is 1 and every fattened step
/// past index 0 is one the count does not reach. That placement is the discrimination: a
/// conjunct written inside `validate_pins`' per-index loop never sees an unreached step's
/// ranking and accepts every refusal leg here, while the count axis — refusing a step this
/// count does not reach — refuses every acceptance leg.
///
/// # The quantifier positions each leg walks
///
/// The fattened step sits at the FIRST, an INTERIOR and the LAST position of a schedule, and a
/// colliding pair of starts sits at the first, an interior and the last sorted window, so a walk
/// restricted to any combination of extremal positions accepts a leg the shipped clause refuses.
#[test]
fn p10_row_2_every_schedule_arm_refuses_a_step_naming_more_than_its_head() {
    // One expression per leg, differing from its paired positive in exactly one axis.
    let verdict = |label: &str, positions: (u32, u32), pin| {
        declare_targets_verdict(label, IterationCount::Fixed(1), 6, positions, vec![pin])
    };
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);

    // ── The `Piecewise` arm. Every leg's paired positive deletes exactly the fattened tail. ──
    for (label, fat, thin) in [
        (
            "piecewise-last",
            piecewise_of(&[(0, &[P1]), (5, &[P1, P0])]),
            piecewise_of(&[(0, &[P1]), (5, &[P1])]),
        ),
        (
            "piecewise-first",
            piecewise_of(&[(0, &[P1, P0]), (5, &[P1])]),
            piecewise_of(&[(0, &[P1]), (5, &[P1])]),
        ),
        (
            "piecewise-interior",
            piecewise_of(&[(0, &[P1]), (5, &[P1, P0]), (9, &[P1])]),
            piecewise_of(&[(0, &[P1]), (5, &[P1]), (9, &[P1])]),
        ),
        (
            "round-robin-last",
            round_robin_of(&[&[P1], &[P1, P0]]),
            round_robin_of(&[&[P1], &[P1]]),
        ),
        (
            "round-robin-first",
            round_robin_of(&[&[P1, P0], &[P1]]),
            round_robin_of(&[&[P1], &[P1]]),
        ),
        (
            "round-robin-interior",
            round_robin_of(&[&[P1], &[P1, P0], &[P1]]),
            round_robin_of(&[&[P1], &[P1], &[P1]]),
        ),
    ] {
        assert!(
            refused(&verdict(label, (1, 1), fat)),
            "{label}: a step naming more than its head is refused wherever it sits"
        );
        assert!(
            accepted(&verdict(label, (1, 1), thin)),
            "{label}: PAIRED POSITIVE — one-entry steps, the later step still unreached by this \
             count, accepted. A count-axis conjunct refuses this, and a clause narrowed to what \
             the respond-side projection can state refuses the rotation half"
        );
    }

    // ── The distinct-starts clause. A segment sharing a start with a LATER-DECLARED one is
    //    selected at no index at all, so it declares a subject no drive reads — while both
    //    legs' index 0 resolves a PUBLISHED subject, so no value check can answer them apart.
    for (label, collided, distinct) in [
        (
            "starts-adjacent",
            piecewise_of(&[(0, &[P1]), (0, &[p2])]),
            // The first segment's start RAISED above the second's: a descending pair, whose
            // index 0 resolves the same seat the collided leg's does. An ordering check refuses
            // it although the drive reads both its segments.
            piecewise_of(&[(2, &[P1]), (0, &[p2])]),
        ),
        (
            "starts-non-adjacent",
            piecewise_of(&[(0, &[P1]), (2, &[p2]), (0, &[p3])]),
            // Starts 0, 2, 1 — pairwise distinct and non-monotone. A clause comparing
            // DECLARED-ORDER neighbours reads every pair distinct in the leg above too.
            piecewise_of(&[(0, &[P1]), (2, &[p2]), (1, &[p3])]),
        ),
        (
            "starts-interior-window",
            piecewise_of(&[(0, &[P1]), (2, &[p2]), (2, &[p3]), (7, &[P1])]),
            piecewise_of(&[(0, &[P1]), (2, &[p2]), (3, &[p3]), (7, &[P1])]),
        ),
    ] {
        assert!(
            refused(&verdict(label, (1, 1), collided)),
            "{label}: a segment shadowed by a later one sharing its start is announced at no \
             index, at no count, under no range"
        );
        assert!(
            accepted(&verdict(label, (1, 1), distinct)),
            "{label}: PAIRED POSITIVE — pairwise-distinct starts, nothing else changed"
        );
    }
}

/// **Row 2b** — the clause is applied to EVERY pin of a multi-position declaration.
///
/// Both legs stage a point whose published positions the declaration fills exactly, and the
/// fattened pin sits at a position that is neither the first nor the last, so the clause
/// hoisted out of the pin loop onto any combination of extremal pins accepts a board the
/// shipped clause refuses.
#[test]
fn p10_row_2b_the_clause_reads_every_declared_pin_position() {
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let verdict = |label: &str, positions: (u32, u32), targets| {
        declare_targets_verdict(label, IterationCount::Fixed(1), 6, positions, targets)
    };

    let two_fat = verdict(
        "p10-row2b-two-fat",
        (2, 2),
        vec![constant_of(&[P1]), constant_of(&[p2, p3])],
    );
    assert!(
        refused(&two_fat),
        "the SECOND of two declared pins names more than its head. got {two_fat:?}"
    );
    let two_thin = verdict(
        "p10-row2b-two-thin",
        (2, 2),
        vec![constant_of(&[P1]), constant_of(&[p2])],
    );
    assert!(
        accepted(&two_thin),
        "PAIRED POSITIVE: the same two pins with the second's tail deleted. got {two_thin:?}"
    );

    let three_fat = verdict(
        "p10-row2b-three-fat",
        (3, 3),
        vec![
            constant_of(&[P1]),
            constant_of(&[p2, P0]),
            constant_of(&[p3]),
        ],
    );
    assert!(
        refused(&three_fat),
        "the INTERIOR of three declared pins names more than its head. got {three_fat:?}"
    );
    let three_thin = verdict(
        "p10-row2b-three-thin",
        (3, 3),
        vec![constant_of(&[P1]), constant_of(&[p2]), constant_of(&[p3])],
    );
    assert!(
        accepted(&three_thin),
        "PAIRED POSITIVE: the same three pins with the interior one's tail deleted. \
         got {three_thin:?}"
    );
}

/// **Row 7** — the rule is DECLARE-time, and a legacy declaration is not re-refused.
///
/// One value, two authorities. The `Constant` names its head plus a second subject the point
/// DID publish, so no value-legality check can refuse it and the new clause is the only thing
/// that can.
#[test]
fn p10_row_7_a_restored_multi_entry_ranking_still_loads_and_still_drives_head_only() {
    use engine::analysis::decision_template::{
        ConcreteDecision, ConcreteTarget, IterationIndex, PinnedDecision,
    };

    let p2 = PlayerId(2);

    // ── LOAD half: the wire ingress `Ranking`'s `#[serde(try_from)]` shim runs. ──
    let (runner, slots) = stage_sequenced_offer(
        "p10-row7-load",
        IterationCount::Fixed(3),
        6,
        vec![victims_point(1, 1)],
    );
    let mut carrying = runner.state().clone();
    let declared = targets_template(
        &slots[0],
        IterationCount::Fixed(3),
        vec![constant_of(&[P1, p2])],
    );
    carrying.waiting_for = WaitingFor::RespondToShortcut {
        player: P1,
        remaining_players: Vec::new(),
        proposal: engine::analysis::loop_check::ShortcutProposal {
            proposer: P0,
            predicted_winner: Some(P0),
            count: IterationCount::Fixed(3),
            unbounded: Vec::new(),
            win_kind: engine::analysis::loop_check::WinKind::Advantage,
            template: Some(declared),
            per_cycle: None,
        },
    };
    let wire = serde_json::to_string(&carrying).expect("serialize the pending proposal");
    let restored: GameState = serde_json::from_str(&wire).expect("a legacy declaration LOADS");
    let WaitingFor::RespondToShortcut { proposal, .. } = &restored.waiting_for else {
        panic!("the restored state still carries its pending proposal");
    };
    let template = proposal
        .template
        .as_ref()
        .expect("the restored proposal still carries its declaration");
    assert_eq!(
        template.decisions,
        vec![PinnedDecision::Targets {
            slot: slots[0].clone(),
            targets: vec![constant_of(&[P1, p2])],
        }],
        "reach-guard: the restored declaration still carries BOTH subjects — a round trip that \
         dropped the tail would satisfy the head-only assertions below vacuously"
    );
    for i in 0..3 as IterationIndex {
        let resolved = engine::analysis::decision_template::resolve(template, i, &restored)
            .expect("the restored declaration still drives");
        assert_eq!(
            resolved,
            vec![ConcreteDecision::Targets {
                slot: slots[0].clone(),
                targets: vec![ConcreteTarget::Player(P1)],
            }],
            "CR 732.2a: every index of the drive's range resolves the HEAD, at index {i}"
        );
    }

    // ── DECLARE half: the identical value offered to the declare ingress at the tip. ──
    let declared_now = declare_targets_verdict(
        "p10-row7-declare",
        IterationCount::Fixed(1),
        6,
        (1, 1),
        vec![constant_of(&[P1, p2])],
    );
    assert!(
        refused(&declared_now),
        "the same value a save may still carry is refused at DECLARE. got {declared_now:?}"
    );
    let truncated = declare_targets_verdict(
        "p10-row7-positive",
        IterationCount::Fixed(1),
        6,
        (1, 1),
        vec![constant_of(&[P1])],
    );
    assert!(
        accepted(&truncated),
        "SAME-CALL POSITIVE: that declaration truncated to its head. It is what tells this \
         refusal from the others this ingress can produce. got {truncated:?}"
    );
}
