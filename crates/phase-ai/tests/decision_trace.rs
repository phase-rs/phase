//! Phase D scenario test — decision-trace aggregator emits structured
//! per-policy breakdowns for the chosen tactical action.
//!
//! The plan (step 22) explicitly permits a targeted test: "construct the
//! `AiContext` + `PolicyRegistry` directly, call `verdicts()` on a chosen
//! candidate, sort + format as the aggregator would, and assert the output
//! shape contains the expected `kind` string." Driving `choose_action` end
//! to end is unreliable for this purpose because the AI routinely prefers
//! `PassPriority` over a fetchland activation when no landfall payoff is on
//! the battlefield (the landfall-timing policy *penalises* the fetch), so
//! the chosen action would be `PassPriority` rather than the fetchland we
//! want to observe.
//!
//! The tests below pin the chosen candidate to the fetchland and invoke the
//! production aggregator (`search::emit_trace_for_candidate`) directly, so
//! we are exercising the exact formatter and registry call that runs inside
//! `choose_action` — no reimplementation in the test.

use std::sync::{Arc, Mutex};

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::config::AiConfig;
use phase_ai::context::AiContext;
use phase_ai::features::{DeckFeatures, LandfallFeature};
use phase_ai::search::emit_trace_for_candidate;
use phase_ai::session::AiSession;
use tracing::subscriber::with_default;
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

/// A `tracing_subscriber::Layer` that captures events on the
/// `phase_ai::decision_trace` target into a shared buffer.
#[derive(Default, Clone)]
struct CaptureLayer {
    entries: Arc<Mutex<Vec<String>>>,
}

impl CaptureLayer {
    fn new() -> Self {
        Self::default()
    }
}

struct StringVisitor<'a>(&'a mut String);

impl<'a> tracing::field::Visit for StringVisitor<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let _ = write!(self.0, " {}={:?}", field.name(), value);
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "phase_ai::decision_trace" {
            return;
        }
        let mut line = String::new();
        let mut visitor = StringVisitor(&mut line);
        event.record(&mut visitor);
        self.entries.lock().unwrap().push(line);
    }
}

const AI: PlayerId = PlayerId(0);

/// CR 701.21 + CR 305.4 + CR 701.23: fetch-shaped activated ability — pays a
/// sacrifice cost (CR 701.21), searches the library (CR 701.23), and puts a
/// land onto the battlefield (CR 305.4).
fn make_fetch_ability() -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::land()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
        },
    );
    ability.cost = Some(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            },
        ],
    });
    ability.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter::land()),
            owner_library: false,
            enter_transformed: false,
            under_your_control: true,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
    )));
    ability
}

fn landfall_features(payoff_on_board_name: Option<&str>) -> DeckFeatures {
    DeckFeatures {
        landfall: LandfallFeature {
            payoff_count: 3,
            enabler_count: 4,
            // Above the 0.1 activation floor — policy opts in.
            commitment: 0.9,
            payoff_names: match payoff_on_board_name {
                Some(name) => vec![name.to_string()],
                // A name that will never match any object on the battlefield.
                None => vec!["Uncraftable Payoff".to_string()],
            },
        },
        ..Default::default()
    }
}

fn build_state_with_fetchland() -> (GameState, ObjectId) {
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::Priority { player: AI };
    let fetch = create_object(
        &mut state,
        CardId(1),
        AI,
        "Fetchland".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&fetch).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(make_fetch_ability());
    }
    (state, fetch)
}

fn build_context(features: DeckFeatures) -> (AiContext, AiConfig) {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session.features.insert(AI, features);
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;
    (context, config)
}

fn fetchland_candidate(fetch: ObjectId) -> CandidateAction {
    CandidateAction {
        action: GameAction::ActivateAbility {
            source_id: fetch,
            ability_index: 0,
        },
        metadata: ActionMetadata {
            actor: Some(AI),
            tactical_class: TacticalClass::Ability,
        },
    }
}

fn run_with_trace<F: FnOnce()>(f: F) -> Vec<String> {
    let layer = CaptureLayer::new();
    let captured = layer.entries.clone();
    let subscriber = tracing_subscriber::registry().with(
        layer.with_filter(
            tracing_subscriber::filter::Targets::new()
                .with_target("phase_ai::decision_trace", tracing::Level::DEBUG),
        ),
    );
    with_default(subscriber, f);
    let out = captured.lock().unwrap().clone();
    out
}

#[test]
fn decision_trace_emits_landfall_no_payoff_kind() {
    let (state, fetch) = build_state_with_fetchland();
    let (context, config) = build_context(landfall_features(None));
    let candidate = fetchland_candidate(fetch);
    let decision = AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates: vec![candidate.clone()],
    };

    let entries = run_with_trace(|| {
        emit_trace_for_candidate(&state, &decision, &candidate, AI, &config, &context);
    });

    assert_eq!(
        entries.len(),
        1,
        "expected one decision_trace entry, got: {entries:?}"
    );
    let line = &entries[0];
    assert!(
        line.contains("landfall_no_payoff_on_board"),
        "expected trace to include `landfall_no_payoff_on_board`, got: {line}"
    );
    assert!(
        line.contains("LandfallTiming"),
        "expected trace to include `LandfallTiming` policy id, got: {line}"
    );
    assert!(
        line.contains("tactical decision"),
        "expected trace to include event message, got: {line}"
    );
}

#[test]
fn decision_trace_gate_short_circuits_with_no_subscriber() {
    // No subscriber installed — `event_enabled!` must short-circuit cleanly
    // with no observable effect. Regression guard for the gated hot path.
    let (state, fetch) = build_state_with_fetchland();
    let (context, config) = build_context(landfall_features(None));
    let candidate = fetchland_candidate(fetch);
    let decision = AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates: vec![candidate.clone()],
    };
    emit_trace_for_candidate(&state, &decision, &candidate, AI, &config, &context);
}
