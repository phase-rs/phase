use engine::analysis::decision_template::{
    DecisionPoint, DecisionPointKind, DecisionSlot, IterationCount, ShortcutDecisionSchema,
};
use engine::game::engine::apply;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, preview_interaction, submit_interaction,
};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, Chooser, CounterCostSelection,
    Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypedFilter, ZoneOwner,
};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AlternativeCastKeyword, AutoPassMode, CastPaymentMode, GameState, MulliganBottomEntry,
    MulliganDecisionEntry, MulliganDecisionPhase, OpeningHandBottomReason, PendingTriggerSummary,
    TurnBoundary, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::interaction::{
    InteractionActionCode, InteractionAvailability, InteractionChoiceId,
    InteractionOpportunityResponse, InteractionOutcomeCode, InteractionPresentationSurface,
    InteractionPreviewRequest, InteractionPreviewStatus, InteractionReasonCode,
    InteractionResponse, InteractionResponseSpec, InteractionRoleCode, InteractionSessionId,
    InteractionShortcutDecision, InteractionShortcutPin, InteractionShortcutPointKind,
    InteractionShortcutResponseCode, InteractionSubmission, PreviewRequestId,
    MAX_INTERACTION_LIST_LEN,
};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
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
                        code: InteractionActionCode::CastSpell
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
                        code: InteractionActionCode::PlayLand
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
                    code: InteractionActionCode::ChooseZoneOpponentChooser
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
fn preference_action_rotates_capability_when_internal_auto_pass_advances() {
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
    .expect("the preference update triggers the configured auto-pass loop");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    assert!(state
        .active_interaction_slots
        .iter()
        .all(|slot| slot.interaction_id != initial));
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
        },
        schema: engine::analysis::decision_template::ShortcutDecisionSchema {
            iteration_count: engine::analysis::decision_template::IterationCount::Fixed(2),
            points: Vec::new(),
            convoke_tappable_count: 0,
        },
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
        },
        schema: ShortcutDecisionSchema {
            iteration_count: IterationCount::Fixed(2),
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
