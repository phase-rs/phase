use std::sync::Arc;

use engine::analysis::decision_template::{
    DecisionPoint, DecisionPointKind, DecisionSlot, IterationCount, ShortcutDecisionSchema,
};
use engine::game::engine::apply;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, preview_interaction,
    resolve_interaction_response, submit_interaction,
};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, Chooser, ChosenAttribute,
    CounterCostSelection, Effect, ManaContribution, ManaProduction, QuantityExpr, ResolvedAbility,
    SacrificeCost, TargetFilter, TargetRef, TypedFilter, ZoneOwner,
};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card::CardFace;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AlternativeCastKeyword, AutoPassMode, CastPaymentMode, GameState, MulliganBottomEntry,
    MulliganDecisionEntry, MulliganDecisionPhase, OpeningHandBottomReason, PendingTriggerSummary,
    PlayerDeckPool, TurnBoundary, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    AmountAssignment, InteractionActionCode, InteractionAvailability, InteractionChoiceId,
    InteractionManaAbilityActivationScope, InteractionManaColor, InteractionManaRestriction,
    InteractionOpportunityResponse, InteractionOutcomeCode, InteractionPresentationSurface,
    InteractionPreviewRequest, InteractionPreviewStatus, InteractionReasonCode,
    InteractionResponse, InteractionResponseSpec, InteractionRoleCode, InteractionSessionId,
    InteractionShortcutCountSpec, InteractionShortcutDecision, InteractionShortcutPin,
    InteractionShortcutPointKind, InteractionShortcutPreview, InteractionShortcutPreviewEntry,
    InteractionShortcutPreviewFamily, InteractionShortcutResponseCode, InteractionSubmission,
    PreviewRequestId, MAX_INTERACTION_LIST_LEN,
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
                chooser: Chooser::Opponent,
                up_to: false,
                selection: CardSelectionMode::Chosen,
                constraint: None,
            },
            vec![],
            source,
            P0,
        )),
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
/// measured period on the certificate.
fn preview_offer(
    iteration_count: IterationCount,
    max_iterations: u32,
    per_cycle: Option<engine::analysis::resource::ResourceVector>,
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
                victim_slot: Vec::new(),
            }),
        },
        schema: ShortcutDecisionSchema {
            iteration_count,
            max_iterations,
            ..Default::default()
        },
        // `points` is empty here (`..Default::default()`), and an empty schema never publishes a
        // declaration — the same invariant row D4 asserts against `build_bounded_declaration`. So
        // `None` is what the engine itself would stage, not merely what makes the literal compile.
        // These rows exercise the PREVIEW projection, which reads the certificate and schema; a
        // declaration here would stage a state the producer cannot emit.
        declaration: None,
    };
    bind(&mut state, "loop-preview");
    state
}

fn shortcut_preview_of(state: &GameState) -> Option<InteractionShortcutPreview> {
    let view = priority_view(state);
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { preview, .. },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("loop shortcut uses a shortcut schema");
    };
    preview.clone()
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

    let at = |n: u32| {
        shortcut_preview_of(&preview_offer(
            IterationCount::Fixed(n),
            n,
            Some(preview_period_delta()),
        ))
        .expect("a bounded offer with a measured period states a preview")
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
        shortcut_preview_of(&preview_offer(
            IterationCount::Fixed(4),
            4,
            Some(preview_period_delta()),
        ))
        .is_some(),
        "control: both authorities present must publish a preview, else every arm below \
         passes for an unrelated reason"
    );

    // ── No measured period: every mint except the bounded one carries `per_cycle: None`,
    //    as does every save written before that field existed.
    assert_eq!(
        shortcut_preview_of(&preview_offer(IterationCount::Fixed(4), 4, None)),
        None,
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
        None,
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
        None,
        "a period that nets to nothing on every family publishes no preview at all"
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
/// The likeliest instance of that first gap is closed by TYPE rather than by text (fix round 3,
/// G4): the cheapest way to reach a `GameState` from the preview is to widen
/// `loop_shortcut_projection` to accept one, which contains none of the banned strings and
/// lives in a span this row does not read. Its parameter list is pinned below, so the
/// projection can see the waiting-for state and nothing else — and neither can anything it
/// calls. What remains uncovered is a clone reached through some OTHER existing binding, which
/// no signature can rule out.
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
        attach.contains("loop_shortcut_projection(") && attach.contains("projection.preview"),
        "reach-guard: the extracted arm must be the one that projects the offer AND publishes \
         the preview onto the spec, else the ban is being applied to the wrong arm"
    );
    assert!(
        attach.contains("filtered_state"),
        "constructibility, second half: the arm must actually USE one of those `&GameState` \
         bindings, so a clone is writable at the exact point the reviewer's probe inserted one"
    );

    // ── TYPE-LEVEL PIN: the ban below is TEXTUAL, so its cheapest evasion is to widen
    //    `loop_shortcut_projection` to take a `&GameState` and clone it THERE — a third span
    //    this row does not read, and one that would contain none of the three banned strings.
    //    The projection's parameter list closes that route by TYPE rather than by text: with
    //    only a `&WaitingFor` in scope, no callee it reaches can be handed a `GameState`
    //    either, so "the preview computation cannot see game state" stops being a search
    //    result and becomes a fact about the signature.
    let projection_signature = extract(&text, "\nfn loop_shortcut_projection(", ") -> ");
    let projection_params = projection_signature
        .strip_prefix("\nfn loop_shortcut_projection(")
        .expect("`extract` re-emits its own marker, so the prefix is always present")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        projection_params.trim_end_matches(','),
        "waiting_for: &WaitingFor",
        "type-level pin: the shortcut preview is computed from the WAITING-FOR state alone. \
         Adding a parameter here — a `&GameState`, or anything reaching one — reopens the \
         clone-apply route through a span the textual ban below never reads"
    );

    for (span_name, body) in [
        ("shortcut_preview_entries", &arithmetic),
        ("opportunity_for_slot's LoopShortcut arm", &attach),
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

/// CR 100.2a + CR 100.4a + CR 100.5: `deck_size` is a *minimum* and non-Commander
/// decks have no maximum, so the between-games schema must publish the interval
/// the engine will accept — `[minimum, whole pool]` — not one exact size. A
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
