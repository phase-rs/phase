//! Created-token placement parity: the two representations of "create a token
//! with counters" must read the same to every tactical policy.
//!
//! The engine writes the same Oracle meaning in two shapes:
//!
//! * **declarative** — `Effect::Token { enter_with_counters: [(type, count)] }`
//!   ("The token enters with X +1/+1 counters on it", CR 614.1c + CR 122.6a);
//! * **imperative** — `Effect::Token { enter_with_counters: [] }` followed in
//!   the same chain by `Effect::PutCounter { target: LastCreated }` ("create a
//!   0/0 token, then put X +1/+1 counters on it", CR 608.2c + CR 122.6).
//!
//! This module owns the shared shape builders every per-site test uses, and the
//! full-registry parity rows: each row builds a card shape in both
//! representations and asserts that every `(PolicyId, verdict)` the shared
//! registry emits is equal between them. A policy outside the four readers this
//! phase changed that disagrees is a sweep miss, not something to fix here, so
//! that assertion runs first and names the policy.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, FilterProp,
    PlayerScope, PtValue, QuantityExpr, QuantityRef, SharedQuality, SubAbilityLink, TargetFilter,
    TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::cast_facts::cast_facts_for_action;
use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::plus_one_counters::PlusOneCountersFeature;
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{PolicyId, PolicyRegistry, PolicyVerdict};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);

// ─── Shared shape builders ──────────────────────────────────────────────────

/// The announced X (`QuantityRef::Variable { name: "X" }`).
pub(crate) fn x_quantity() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::Variable {
            name: "X".to_string(),
        },
    }
}

/// A green-blue Fractal creature token of the given size.
pub(crate) fn token_effect(
    power: i32,
    toughness: i32,
    enter_with_counters: Vec<(CounterType, QuantityExpr)>,
) -> Effect {
    Effect::Token {
        name: "Fractal".to_string(),
        power: PtValue::Fixed(power),
        toughness: PtValue::Fixed(toughness),
        types: vec!["Creature".to_string(), "Fractal".to_string()],
        colors: vec![ManaColor::Green, ManaColor::Blue],
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters,
    }
}

/// `PutCounter { target: LastCreated }`: counters on the object(s) the chain
/// last created.
pub(crate) fn last_created_placement(counter_type: CounterType, count: QuantityExpr) -> Effect {
    Effect::PutCounter {
        counter_type,
        count,
        target: TargetFilter::LastCreated,
    }
}

/// `effects` as one unconditional chain: the first is the root effect of a
/// `kind` ability, each later one a `Spell`-kind `sub_ability` joined by `link`.
pub(crate) fn effect_chain(
    kind: AbilityKind,
    link: SubAbilityLink,
    effects: Vec<Effect>,
) -> AbilityDefinition {
    let mut effects = effects.into_iter().rev();
    let last = effects.next().expect("a chain has at least one effect");
    let mut node = AbilityDefinition::new(AbilityKind::Spell, last);
    for effect in effects {
        let mut parent = AbilityDefinition::new(AbilityKind::Spell, effect);
        node.sub_link = link;
        parent.sub_ability = Some(Box::new(node));
        node = parent;
    }
    node.kind = kind;
    node
}

/// Declarative shape: a token of `size` that enters with `count` counters.
pub(crate) fn declarative_token_with_counters(
    kind: AbilityKind,
    size: (i32, i32),
    counter_type: CounterType,
    count: QuantityExpr,
) -> AbilityDefinition {
    AbilityDefinition::new(
        kind,
        token_effect(size.0, size.1, vec![(counter_type, count)]),
    )
}

/// Imperative shape: a token of `size` with no entry counters, then `count`
/// counters put on it by a `link`-joined `PutCounter { LastCreated }` sub.
/// `ContinuationStep` is the shape the imperative class lowers to;
/// `SequentialSibling` is the shape route cards (Kianne, Match the Odds) parse to.
pub(crate) fn imperative_token_then_placement(
    kind: AbilityKind,
    link: SubAbilityLink,
    size: (i32, i32),
    counter_type: CounterType,
    count: QuantityExpr,
) -> AbilityDefinition {
    effect_chain(
        kind,
        link,
        vec![
            token_effect(size.0, size.1, vec![]),
            last_created_placement(counter_type, count),
        ],
    )
}

/// Which of the two shapes a fixture is built in.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Representation {
    Declarative,
    Imperative(SubAbilityLink),
}

/// The token-with-counters ability in `representation`.
pub(crate) fn token_with_counters(
    representation: Representation,
    kind: AbilityKind,
    size: (i32, i32),
    counter_type: CounterType,
    count: QuantityExpr,
) -> AbilityDefinition {
    match representation {
        Representation::Declarative => {
            declarative_token_with_counters(kind, size, counter_type, count)
        }
        Representation::Imperative(link) => {
            imperative_token_then_placement(kind, link, size, counter_type, count)
        }
    }
}

/// A "when this enters" trigger (CR 603.6a) running `execute`.
pub(crate) fn self_etb_trigger(execute: AbilityDefinition) -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .destination(Zone::Battlefield)
        .execute(execute)
}

/// `{X}` as a mana cost.
pub(crate) fn x_mana_cost() -> AbilityCost {
    AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        },
    }
}

// ─── Full-registry parity (D1.6) ────────────────────────────────────────────

/// A `PolicyVerdict` reduced to comparable data. `PolicyVerdict` derives no
/// `PartialEq`, so the parity rows compare this projection. Deltas compare with
/// `==`, so `-0.0` equals `+0.0`.
#[derive(Debug, PartialEq)]
enum ProjectedVerdict {
    Reject {
        kind: &'static str,
        facts: Vec<(&'static str, i64)>,
    },
    Score {
        delta: f64,
        kind: &'static str,
        facts: Vec<(&'static str, i64)>,
    },
}

impl From<PolicyVerdict> for ProjectedVerdict {
    fn from(verdict: PolicyVerdict) -> Self {
        match verdict {
            PolicyVerdict::Reject { reason } => Self::Reject {
                kind: reason.kind,
                facts: reason.facts,
            },
            PolicyVerdict::Score { delta, reason } => Self::Score {
                delta,
                kind: reason.kind,
                facts: reason.facts,
            },
        }
    }
}

/// The policies this phase changed: a difference anywhere else stops the phase.
const CHANGED_READERS: [PolicyId; 4] = [
    PolicyId::PlusOneCountersTactical,
    PolicyId::AntiSelfHarm,
    PolicyId::RedundancyAvoidance,
    PolicyId::XCastGate,
];

#[derive(Debug, Clone, Copy)]
enum Board {
    Empty,
    OwnCreature,
}

/// One parity row: builds its card into `state` in `representation` and
/// returns the source and the candidate action.
struct ParityRow {
    name: &'static str,
    build: fn(&mut GameState, Representation) -> (ObjectId, GameAction),
}

fn fresh_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.active_player = AI;
    state.priority_player = AI;
    state.waiting_for = WaitingFor::Priority { player: AI };
    state
}

fn add_object(
    state: &mut GameState,
    name: &str,
    zone: Zone,
    core_types: Vec<CoreType>,
    mana_cost: ManaCost,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, name.to_string(), zone);
    let object = state.objects.get_mut(&id).expect("just created");
    object.card_types.core_types = core_types;
    object.mana_cost = mana_cost;
    if object.card_types.core_types.contains(&CoreType::Creature) {
        object.power = Some(2);
        object.toughness = Some(2);
    }
    id
}

fn set_abilities(state: &mut GameState, id: ObjectId, abilities: Vec<AbilityDefinition>) {
    *Arc::make_mut(&mut state.objects.get_mut(&id).expect("source").abilities) = abilities;
}

fn cast(state: &GameState, object_id: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id,
        card_id: state.objects[&object_id].card_id,
        targets: Vec::new(),
        payment_mode: CastPaymentMode::Auto,
    }
}

fn mana(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Fractal Summoning, {X}{G/U}{G/U} sorcery: "Create a 0/0 green and blue
/// Fractal creature token. Put X +1/+1 counters on it."
fn fractal_summoning(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let id = add_object(
        state,
        "Fractal Summoning",
        Zone::Hand,
        vec![CoreType::Sorcery],
        mana(
            vec![
                ManaCostShard::X,
                ManaCostShard::GreenBlue,
                ManaCostShard::GreenBlue,
            ],
            0,
        ),
    );
    let ability = token_with_counters(
        repr,
        AbilityKind::Spell,
        (0, 0),
        CounterType::Plus1Plus1,
        x_quantity(),
    );
    set_abilities(state, id, vec![ability]);
    (id, cast(state, id))
}

/// A self-ETB creature spell whose ETB creates the token (Manifestation Sage:
/// "...Put X +1/+1 counters on it, where X is the number of cards in your hand").
fn manifestation_sage(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let id = add_object(
        state,
        "Manifestation Sage",
        Zone::Hand,
        vec![CoreType::Creature],
        mana(vec![ManaCostShard::GreenBlue; 4], 0),
    );
    let execute = token_with_counters(
        repr,
        AbilityKind::Spell,
        (0, 0),
        CounterType::Plus1Plus1,
        QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
        },
    );
    state
        .objects
        .get_mut(&id)
        .expect("source")
        .trigger_definitions
        .push(self_etb_trigger(execute));
    (id, cast(state, id))
}

/// Additive Evolution's ETB trigger on an enchantment spell: "...create a 0/0
/// green and blue Fractal creature token. Put three +1/+1 counters on it."
fn additive_evolution(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let id = add_object(
        state,
        "Additive Evolution",
        Zone::Hand,
        vec![CoreType::Enchantment],
        mana(vec![ManaCostShard::Green, ManaCostShard::Green], 3),
    );
    let execute = token_with_counters(
        repr,
        AbilityKind::Spell,
        (0, 0),
        CounterType::Plus1Plus1,
        QuantityExpr::Fixed { value: 3 },
    );
    state
        .objects
        .get_mut(&id)
        .expect("source")
        .trigger_definitions
        .push(self_etb_trigger(execute));
    (id, cast(state, id))
}

/// Berta, Wise Extrapolator's "{X}, {T}: Create a 0/0 green and blue Fractal
/// creature token and put X +1/+1 counters on it."
fn berta_ability(repr: Representation) -> AbilityDefinition {
    let mut ability = token_with_counters(
        repr,
        AbilityKind::Activated,
        (0, 0),
        CounterType::Plus1Plus1,
        x_quantity(),
    );
    ability.cost = Some(AbilityCost::Composite {
        costs: vec![x_mana_cost(), AbilityCost::Tap],
    });
    ability
}

fn berta_activate(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let id = add_object(
        state,
        "Berta, Wise Extrapolator",
        Zone::Battlefield,
        vec![CoreType::Creature],
        mana(vec![ManaCostShard::Green, ManaCostShard::Blue], 2),
    );
    set_abilities(state, id, vec![berta_ability(repr)]);
    (
        id,
        GameAction::ActivateAbility {
            source_id: id,
            ability_index: 0,
        },
    )
}

/// Casting a permanent whose activated ability is the created-token ability:
/// `PolicyContext::effects()` reads every ability of a cast object.
fn berta_cast(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let id = add_object(
        state,
        "Berta, Wise Extrapolator",
        Zone::Hand,
        vec![CoreType::Creature],
        mana(vec![ManaCostShard::Green, ManaCostShard::Blue], 2),
    );
    set_abilities(state, id, vec![berta_ability(repr)]);
    (id, cast(state, id))
}

/// Match the Odds, {2}{G} sorcery: "Create a 1/1 white Ally creature token. Put
/// a +1/+1 counter on it for each creature your opponents control." (route card;
/// its imperative twin is the parsed `SequentialSibling` shape).
fn match_the_odds(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let id = add_object(
        state,
        "Match the Odds",
        Zone::Hand,
        vec![CoreType::Sorcery],
        mana(vec![ManaCostShard::Green], 2),
    );
    let ability = token_with_counters(
        repr,
        AbilityKind::Spell,
        (1, 1),
        CounterType::Plus1Plus1,
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::Opponent),
                ),
            },
        },
    );
    set_abilities(state, id, vec![ability]);
    (id, cast(state, id))
}

/// Kianne, Dean of Substance's "{4}{G}: Create a 0/0 green and blue Fractal
/// creature token. Put a +1/+1 counter on it for each different mana value
/// among nonland cards you own in exile with study counters on them." (route).
fn kianne_activate(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let id = add_object(
        state,
        "Kianne, Dean of Substance",
        Zone::Battlefield,
        vec![CoreType::Creature],
        mana(vec![ManaCostShard::Green], 1),
    );
    let studied = TypedFilter::card()
        .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
        .properties(vec![
            FilterProp::Owned {
                controller: ControllerRef::You,
            },
            FilterProp::InZone { zone: Zone::Exile },
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Generic("study".to_string())),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 1 },
            },
        ]);
    let mut ability = token_with_counters(
        repr,
        AbilityKind::Activated,
        (0, 0),
        CounterType::Plus1Plus1,
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountDistinct {
                filter: TargetFilter::Typed(studied),
                qualities: vec![SharedQuality::ManaValue],
            },
        },
    );
    ability.cost = Some(AbilityCost::Mana {
        cost: mana(vec![ManaCostShard::Green], 4),
    });
    set_abilities(state, id, vec![ability]);
    (
        id,
        GameAction::ActivateAbility {
            source_id: id,
            ability_index: 0,
        },
    )
}

/// Total entry counters the source's token effects carry, across its abilities
/// and trigger executes: non-zero only in the declarative shape.
fn entry_counter_entries(state: &GameState, source: ObjectId) -> usize {
    let object = &state.objects[&source];
    let trigger_executes = object
        .trigger_definitions
        .iter_unchecked()
        .filter_map(|entry| entry.definition.execute.as_deref());
    object
        .abilities
        .iter()
        .chain(trigger_executes)
        .flat_map(crate::ability_chain::collect_chain_effects)
        .map(|effect| match effect {
            Effect::Token {
                enter_with_counters,
                ..
            } => enter_with_counters.len(),
            _ => 0,
        })
        .sum()
}

fn registry_projection(
    state: &GameState,
    action: GameAction,
    commitment: f32,
) -> Vec<(PolicyId, ProjectedVerdict)> {
    let class = match action {
        GameAction::CastSpell { .. } => TacticalClass::Spell,
        _ => TacticalClass::Ability,
    };
    let candidate = CandidateAction {
        action,
        metadata: ActionMetadata::for_actor(Some(AI), class),
    };
    let decision = AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates: vec![candidate.clone()],
    };
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session.features.insert(
        AI,
        DeckFeatures {
            plus_one_counters: PlusOneCountersFeature {
                commitment,
                ..PlusOneCountersFeature::default()
            },
            ..DeckFeatures::default()
        },
    );
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;
    let ctx = PolicyContext {
        state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        // Production builds the cast facts from the candidate action.
        cast_facts: cast_facts_for_action(state, &candidate.action, AI),
        search_depth: SearchDepth::Root,
    };
    PolicyRegistry::shared()
        .verdicts(&ctx)
        .into_iter()
        .map(|(id, verdict)| (id, verdict.into()))
        .collect()
}

/// Every verdict `id` emitted, in emission order.
fn verdicts_of(verdicts: &[(PolicyId, ProjectedVerdict)], id: PolicyId) -> Vec<&ProjectedVerdict> {
    verdicts
        .iter()
        .filter(|(policy, _)| *policy == id)
        .map(|(_, verdict)| verdict)
        .collect()
}

fn build_twin(
    row: &ParityRow,
    repr: Representation,
    board: Board,
) -> (GameState, ObjectId, GameAction) {
    let mut state = fresh_state();
    if let Board::OwnCreature = board {
        add_object(
            &mut state,
            "Grizzly Bears",
            Zone::Battlefield,
            vec![CoreType::Creature],
            mana(vec![ManaCostShard::Green], 1),
        );
    }
    let (source, action) = (row.build)(&mut state, repr);
    (state, source, action)
}

fn assert_row_parity(row: &ParityRow, link: SubAbilityLink) {
    for commitment in [0.0_f32, 0.9] {
        for board in [Board::Empty, Board::OwnCreature] {
            let label = format!("{} (c = {commitment}, {board:?} board)", row.name);
            let (decl_state, decl_source, decl_action) =
                build_twin(row, Representation::Declarative, board);
            let (imp_state, imp_source, imp_action) =
                build_twin(row, Representation::Imperative(link), board);

            // Reach guard: the twins really are the two shapes.
            assert!(
                entry_counter_entries(&decl_state, decl_source) > 0,
                "{label}: the declarative twin must carry entry counters"
            );
            assert_eq!(
                entry_counter_entries(&imp_state, imp_source),
                0,
                "{label}: the imperative twin must carry no entry counters"
            );

            let decl = registry_projection(&decl_state, decl_action, commitment);
            let imp = registry_projection(&imp_state, imp_action, commitment);

            // Reach guard: the changed readers are scored on both twins.
            let mut required = vec![
                PolicyId::AntiSelfHarm,
                PolicyId::RedundancyAvoidance,
                PolicyId::XCastGate,
            ];
            if commitment > 0.0 {
                required.push(PolicyId::PlusOneCountersTactical);
            }
            for id in &required {
                for (shape, verdicts) in [("declarative", &decl), ("imperative", &imp)] {
                    assert!(
                        verdicts.iter().any(|(policy, _)| policy == id),
                        "{label}: {id:?} must score the {shape} twin"
                    );
                }
            }

            let mut ids: Vec<PolicyId> = Vec::new();
            for (id, _) in decl.iter().chain(imp.iter()) {
                if !ids.contains(id) {
                    ids.push(*id);
                }
            }
            let differing: Vec<PolicyId> = ids
                .into_iter()
                .filter(|id| verdicts_of(&decl, *id) != verdicts_of(&imp, *id))
                .collect();
            let outside: Vec<PolicyId> = differing
                .iter()
                .copied()
                .filter(|id| !CHANGED_READERS.contains(id))
                .collect();
            // D1.6 stop rule: a policy outside the changed readers that reads
            // the two shapes differently is a sweep miss.
            assert!(
                outside.is_empty(),
                "{label}: STOP RULE (D1.6) — policies outside the changed readers differ: \
                 {outside:?}\ndeclarative: {decl:?}\nimperative: {imp:?}"
            );
            assert!(
                differing.is_empty(),
                "{label}: the two representations must score identically; differing: \
                 {differing:?}\ndeclarative: {decl:?}\nimperative: {imp:?}"
            );
        }
    }
}

/// The moved class, built in the shape it lowers to (`ContinuationStep`).
#[test]
fn moved_class_rows_score_identically_in_both_representations() {
    let rows = [
        ParityRow {
            name: "Fractal Summoning cast",
            build: fractal_summoning,
        },
        ParityRow {
            name: "Manifestation Sage cast",
            build: manifestation_sage,
        },
        ParityRow {
            name: "Additive Evolution cast",
            build: additive_evolution,
        },
        ParityRow {
            name: "Berta activate",
            build: berta_activate,
        },
        ParityRow {
            name: "Berta cast",
            build: berta_cast,
        },
    ];
    for row in &rows {
        assert_row_parity(row, SubAbilityLink::ContinuationStep);
    }
}

/// Route cards, whose imperative twin is the `SequentialSibling` shape they
/// parse to today.
#[test]
fn route_rows_score_identically_in_both_representations() {
    let rows = [
        ParityRow {
            name: "Match the Odds cast",
            build: match_the_odds,
        },
        ParityRow {
            name: "Kianne activate",
            build: kianne_activate,
        },
    ];
    for row in &rows {
        assert_row_parity(row, SubAbilityLink::SequentialSibling);
    }
}

// ─── Production-parse seam (S1) ─────────────────────────────────────────────

/// The engine's imperative lowering: a `ContinuationStep`, `Spell`-kind
/// `PutCounter { target: LastCreated }` (CR 608.2c + CR 122.1).
fn is_created_token_placement(def: &AbilityDefinition) -> bool {
    matches!(
        &*def.effect,
        Effect::PutCounter {
            target: TargetFilter::LastCreated,
            ..
        }
    ) && def.sub_link == SubAbilityLink::ContinuationStep
        && def.kind == AbilityKind::Spell
}

/// Every `PutCounter { target: LastCreated }` in `def`, across its sub, else and
/// mode branches.
fn last_created_placements(def: &AbilityDefinition) -> usize {
    let own = usize::from(matches!(
        &*def.effect,
        Effect::PutCounter {
            target: TargetFilter::LastCreated,
            ..
        }
    ));
    own + [def.sub_ability.as_deref(), def.else_ability.as_deref()]
        .into_iter()
        .flatten()
        .chain(def.mode_abilities.iter())
        .map(last_created_placements)
        .sum::<usize>()
}

/// The S1 seam assertion: the production parse is exactly the shape the
/// phase-1 predicate pairs — one `Token` with no entry counters whose DIRECT
/// sub is the placement, and no other `LastCreated` placement anywhere.
fn assert_production_seam(name: &str, def: &AbilityDefinition) {
    let mut paired_tokens = 0;
    let mut node = Some(def);
    while let Some(current) = node {
        if let Effect::Token {
            enter_with_counters,
            ..
        } = &*current.effect
        {
            assert!(
                enter_with_counters.is_empty(),
                "{name}: the production token carries no entry counters: {def:?}"
            );
            if current
                .sub_ability
                .as_deref()
                .is_some_and(is_created_token_placement)
            {
                paired_tokens += 1;
            }
        }
        node = current.sub_ability.as_deref();
    }
    assert_eq!(
        (paired_tokens, last_created_placements(def)),
        (1, 1),
        "{name}: one token directly followed by its placement, and no other \
         LastCreated placement: {def:?}"
    );
}

/// Inverse of the engine's imperative lowering, applied in place: the
/// placement's `(counter_type, count)` becomes the `Token`'s entry counters and
/// the placement's own sub moves up under the `Token`. Returns whether a
/// placement was folded.
fn fold_in_place(node: &mut AbilityDefinition) -> bool {
    let is_pair = matches!(&*node.effect, Effect::Token { .. })
        && node
            .sub_ability
            .as_deref()
            .is_some_and(is_created_token_placement);
    if !is_pair {
        return node.sub_ability.as_deref_mut().is_some_and(fold_in_place);
    }
    let placement = *node.sub_ability.take().expect("checked above");
    let Effect::PutCounter {
        counter_type,
        count,
        ..
    } = *placement.effect
    else {
        panic!("a created-token placement is a PutCounter");
    };
    if let Effect::Token {
        enter_with_counters,
        ..
    } = &mut *node.effect
    {
        enter_with_counters.push((counter_type, count));
    }
    node.sub_ability = placement.sub_ability;
    true
}

/// The declarative twin of a production parse.
fn fold_placement_into_entry_counters(def: &AbilityDefinition) -> AbilityDefinition {
    let mut folded = def.clone();
    assert!(
        fold_in_place(&mut folded),
        "the production parse holds a placement: {def:?}"
    );
    folded
}

/// The production `ability` in `repr`: the parse itself after the seam
/// assertion (it lowers only to `ContinuationStep`), or its declarative fold.
fn production_in(
    repr: Representation,
    name: &str,
    ability: AbilityDefinition,
) -> AbilityDefinition {
    match repr {
        Representation::Imperative(link) => {
            assert_eq!(
                link,
                SubAbilityLink::ContinuationStep,
                "{name}: the production parse is the ContinuationStep shape"
            );
            assert_production_seam(name, &ability);
            ability
        }
        Representation::Declarative => fold_placement_into_entry_counters(&ability),
    }
}

fn parse_card(
    oracle: &str,
    name: &str,
    keywords: &[&str],
    types: &[&str],
    subtypes: &[&str],
) -> ParsedAbilities {
    let owned = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    parse_oracle_text(
        oracle,
        name,
        &owned(keywords),
        &owned(types),
        &owned(subtypes),
    )
}

/// The one activated ability of a production parse.
fn activated_ability(parsed: ParsedAbilities) -> AbilityDefinition {
    let mut activated = parsed
        .abilities
        .into_iter()
        .filter(|ability| ability.kind == AbilityKind::Activated);
    let ability = activated.next().expect("an activated ability");
    assert!(activated.next().is_none(), "exactly one activated ability");
    ability
}

/// Fractal Summoning, verbatim Oracle (MTGJSON).
fn fractal_summoning_parsed(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let parsed = parse_card(
        "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it.",
        "Fractal Summoning",
        &[],
        &["Sorcery"],
        &["Lesson"],
    );
    let ability = parsed
        .abilities
        .into_iter()
        .next()
        .expect("the spell ability");
    let id = add_object(
        state,
        "Fractal Summoning",
        Zone::Hand,
        vec![CoreType::Sorcery],
        mana(
            vec![
                ManaCostShard::X,
                ManaCostShard::GreenBlue,
                ManaCostShard::GreenBlue,
            ],
            0,
        ),
    );
    let ability = production_in(repr, "Fractal Summoning", ability);
    set_abilities(state, id, vec![ability]);
    (id, cast(state, id))
}

/// Manifestation Sage, verbatim Oracle (MTGJSON): the production self-ETB
/// trigger, with its execute in `repr`.
fn manifestation_sage_parsed(
    state: &mut GameState,
    repr: Representation,
) -> (ObjectId, GameAction) {
    let parsed = parse_card(
        "When this creature enters, create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of cards in your hand.",
        "Manifestation Sage",
        &[],
        &["Creature"],
        &["Human", "Wizard"],
    );
    let mut trigger = parsed.triggers.into_iter().next().expect("the ETB trigger");
    let execute = *trigger.execute.take().expect("the trigger body");
    trigger.execute = Some(Box::new(production_in(repr, "Manifestation Sage", execute)));
    let id = add_object(
        state,
        "Manifestation Sage",
        Zone::Hand,
        vec![CoreType::Creature],
        mana(vec![ManaCostShard::GreenBlue; 4], 0),
    );
    state
        .objects
        .get_mut(&id)
        .expect("source")
        .trigger_definitions
        .push(trigger);
    (id, cast(state, id))
}

/// Berta, Wise Extrapolator, verbatim Oracle (MTGJSON): the production "{X},
/// {T}: Create … and put X +1/+1 counters on it." ("and put" grammar).
fn berta_activate_parsed(state: &mut GameState, repr: Representation) -> (ObjectId, GameAction) {
    let parsed = parse_card(
        "Increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)\nWhenever one or more +1/+1 counters are put on Berta, add one mana of any color.\n{X}, {T}: Create a 0/0 green and blue Fractal creature token and put X +1/+1 counters on it.",
        "Berta, Wise Extrapolator",
        &["Increment"],
        &["Creature"],
        &["Frog", "Druid"],
    );
    let ability = production_in(repr, "Berta", activated_ability(parsed));
    let id = add_object(
        state,
        "Berta, Wise Extrapolator",
        Zone::Battlefield,
        vec![CoreType::Creature],
        mana(vec![ManaCostShard::Green, ManaCostShard::Blue], 2),
    );
    set_abilities(state, id, vec![ability]);
    (
        id,
        GameAction::ActivateAbility {
            source_id: id,
            ability_index: 0,
        },
    )
}

/// Sequence Engine, verbatim Oracle (MTGJSON): a non-creature activated source
/// with a declared target, whose token is nested under the exile.
fn sequence_engine_activate_parsed(
    state: &mut GameState,
    repr: Representation,
) -> (ObjectId, GameAction) {
    let parsed = parse_card(
        "{X}, {T}: Exile target creature card with mana value X from a graveyard. Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it. Activate only as a sorcery.",
        "Sequence Engine",
        &[],
        &["Artifact"],
        &[],
    );
    let ability = production_in(repr, "Sequence Engine", activated_ability(parsed));
    let id = add_object(
        state,
        "Sequence Engine",
        Zone::Battlefield,
        vec![CoreType::Artifact],
        mana(vec![ManaCostShard::Green], 2),
    );
    set_abilities(state, id, vec![ability]);
    (
        id,
        GameAction::ActivateAbility {
            source_id: id,
            ability_index: 0,
        },
    )
}

/// S1: the production parse of moved class members is the shape the phase-1
/// predicate pairs, and the full registry scores it identically to its
/// declarative fold.
#[test]
fn production_parse_rows_score_identically_in_both_representations() {
    let rows = [
        ParityRow {
            name: "Fractal Summoning cast (production parse)",
            build: fractal_summoning_parsed,
        },
        ParityRow {
            name: "Manifestation Sage cast (production parse)",
            build: manifestation_sage_parsed,
        },
        ParityRow {
            name: "Berta activate (production parse)",
            build: berta_activate_parsed,
        },
        ParityRow {
            name: "Sequence Engine activate (production parse)",
            build: sequence_engine_activate_parsed,
        },
    ];
    for row in &rows {
        assert_row_parity(row, SubAbilityLink::ContinuationStep);
    }
}
