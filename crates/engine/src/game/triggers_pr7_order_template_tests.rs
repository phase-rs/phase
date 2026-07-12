//! PR-7 Part B2 — building-block tests for the trigger-ordering resolver's two
//! tiers (`apply_trigger_order_template`) plus the `SetTriggerOrderTemplate{Save}`
//! GameAction route through `apply()`.
//!
//! CR 603.3b: a simultaneous batch is ordered ONCE; every subsequent parked-tail
//! re-drain is coverage-only. These tests pin the two-tier split:
//!   * ephemeral (`ThisObject`) = COVERAGE-ONLY (never permutes) — T2, T-dup.
//!   * persistent (`AllCopies`) = PERMUTE-ONCE + register ephemeral marker — T7, T4.
use super::*;
use crate::analysis::decision_template::{
    DecisionGroupKey, DecisionKind, DecisionTemplate, PinnedDecision, ReplayMode,
};
use crate::types::game_state::{GameState, TriggerOrderGroup, YieldTarget};
use crate::types::identifiers::CardId;

/// Minimal injected trigger context. `apply_trigger_order_template` reads only
/// `source_id`, `source_incarnation` and `source_card_id`; the effect is inert
/// (a distinct `count` only matters where a test drives `begin_trigger_ordering`).
fn mk_ctx(
    source_id: u64,
    incarnation: Option<u64>,
    card_id: Option<u64>,
    count: i32,
) -> DeferredTrigger {
    let mut ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(source_id),
        PlayerId(0),
    );
    ability.source_incarnation = incarnation;
    ability.source_card_id = card_id.map(CardId);
    PendingTriggerContext {
        pending: PendingTrigger {
            source_id: ObjectId(source_id),
            controller: PlayerId(0),
            condition: None,
            ability,
            timestamp: 0,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: Vec::new(),
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        },
        trigger_events: Vec::new(),
        dispatch_origin: PendingTriggerDispatchOrigin::Normal,
    }
}

fn group(triggers: Vec<DeferredTrigger>) -> TriggerOrderGroup {
    TriggerOrderGroup {
        controller: PlayerId(0),
        triggers,
        ordered: false,
    }
}

fn this_obj(source_id: u64, incarnation: Option<u64>) -> YieldTarget {
    YieldTarget::ThisObject {
        source_id: ObjectId(source_id),
        incarnation,
        trigger_description: None,
    }
}

fn all_copies(card_id: u64) -> YieldTarget {
    YieldTarget::AllCopies {
        card_id: CardId(card_id),
        trigger_description: None,
    }
}

/// Source-ids of a group's triggers, in current order — the observable a permute
/// would change and coverage-only leaves intact.
fn source_ids(g: &TriggerOrderGroup) -> Vec<u64> {
    g.triggers.iter().map(|c| c.pending.source_id.0).collect()
}

/// T2 (gate building-block, coverage-only). The false↔true flip between (a) no
/// template and (b) a covering ephemeral template IS the fix; (b)/(c) also pin that
/// the group order is left byte-identical (a permuting impl would reorder it).
#[test]
fn apply_ephemeral_template_is_coverage_only() {
    let mut state = GameState::new_two_player(7);

    // Order-dependent group [src1, src2] in the player's chosen order.
    let mut g = group(vec![
        mk_ctx(1, Some(0), None, 1),
        mk_ctx(2, Some(0), None, 2),
    ]);

    // (a) No template ⇒ false, group NOT ordered (would fall through to prompt).
    assert!(
        !apply_trigger_order_template(&mut state, &mut g),
        "no covering template ⇒ apply returns false"
    );

    // (b) Register a covering ephemeral (ThisObject) template for {src1, src2}.
    let tmpl = build_ephemeral_order_template(PlayerId(0), &g.triggers);
    state.set_trigger_order_template(tmpl);
    assert!(
        apply_trigger_order_template(&mut state, &mut g),
        "a covering ephemeral template ⇒ apply returns true"
    );
    assert_eq!(
        source_ids(&g),
        vec![1, 2],
        "COVERAGE-ONLY: the already-chosen order is left byte-identical (a permuting impl fails here)"
    );

    // (c) The shrunken tail (drop the head) is still a sub-multiset ⇒ still covered.
    let mut tail = group(vec![mk_ctx(2, Some(0), None, 2)]);
    assert!(
        apply_trigger_order_template(&mut state, &mut tail),
        "a shrinking suffix ⊆ the full batch is still covered (sub-multiset)"
    );
    assert_eq!(source_ids(&tail), vec![2], "tail order unchanged");
}

/// T-dup (MAJOR-1 regression, duplicate same-source). One source (srcX=10) fired two
/// order-dependent triggers P and Q; the chosen order is [P(10), R(20), Q(10)]. After
/// P dispatches and pauses, the parked tail is [R(20), Q(10)]. Coverage-only must leave
/// it [R, Q]; a reorder-by-pin-pos impl greedily maps Q(10)→pos0, R(20)→pos1 ⇒ [Q, R].
#[test]
fn apply_ephemeral_duplicate_source_tail_stays_in_order() {
    let mut state = GameState::new_two_player(7);

    // Full chosen batch [P(10), R(20), Q(10)] — register its ephemeral marker.
    let full = group(vec![
        mk_ctx(10, Some(0), None, 1), // P
        mk_ctx(20, Some(0), None, 2), // R
        mk_ctx(10, Some(0), None, 3), // Q (same source as P)
    ]);
    let tmpl = build_ephemeral_order_template(PlayerId(0), &full.triggers);
    state.set_trigger_order_template(tmpl);

    // The parked tail after P dispatched: [R(20), Q(10)].
    let mut tail = group(vec![
        mk_ctx(20, Some(0), None, 2), // R
        mk_ctx(10, Some(0), None, 3), // Q
    ]);
    assert!(
        apply_trigger_order_template(&mut state, &mut tail),
        "the tail is a sub-multiset of {{srcX, srcY, srcX}} ⇒ covered"
    );
    assert_eq!(
        source_ids(&tail),
        vec![20, 10],
        "COVERAGE-ONLY leaves [R, Q]; a reorder-by-pin-pos impl would invert to [Q, R] \
         (Q(srcX)→first srcX pin pos0)"
    );
}

/// T7 (Gap-B regression, persistent distinct-card parked tail). Saved persistent order
/// {X@0, Y@1, X@2} (X duplicate card, Y distinct). Fresh full batch [X1, X2, Y] permutes
/// ONCE to [X1, Y, X2] and registers a `ThisObject` ephemeral marker; the parked tail
/// [Y, X2] is then coverage-only (stays [Y, X2]). A direct-permute-on-tail impl would
/// re-grab the pin vacated by X1 and invert the DISTINCT pair to [X2, Y].
#[test]
fn apply_persistent_permutes_once_then_tail_is_coverage_only() {
    const CARD_X: u64 = 100;
    const CARD_Y: u64 = 200;
    let mut state = GameState::new_two_player(7);

    // Persistent AllCopies template: X@0, Y@1, X@2.
    let persistent = DecisionTemplate {
        owner: PlayerId(0),
        decisions: vec![
            PinnedDecision::Order {
                source: all_copies(CARD_X),
                pos: 0,
            },
            PinnedDecision::Order {
                source: all_copies(CARD_Y),
                pos: 1,
            },
            PinnedDecision::Order {
                source: all_copies(CARD_X),
                pos: 2,
            },
        ],
        replay: ReplayMode::Static,
        key: DecisionGroupKey::from_sources(
            &[all_copies(CARD_X), all_copies(CARD_Y), all_copies(CARD_X)],
            DecisionKind::TriggerOrdering,
        ),
    };
    state.set_trigger_order_template(persistent);

    // Fresh full batch in placement order [X1(src1), X2(src2), Y(src3)].
    let mut fresh = group(vec![
        mk_ctx(1, Some(0), Some(CARD_X), 1), // X1
        mk_ctx(2, Some(0), Some(CARD_X), 2), // X2
        mk_ctx(3, Some(0), Some(CARD_Y), 3), // Y
    ]);
    assert!(
        apply_trigger_order_template(&mut state, &mut fresh),
        "persistent template covers the fresh batch"
    );
    assert_eq!(
        source_ids(&fresh),
        vec![1, 3, 2],
        "PERMUTE-ONCE to [X1, Y, X2] (X1→pos0, Y→pos1, X2→pos2)"
    );
    // A ThisObject ephemeral marker for {src1, src2, src3} now exists.
    assert!(
        state.decision_templates.iter().any(|t| t.key.is_ephemeral()
            && t.key.covers(&[
                this_obj(1, Some(0)),
                this_obj(2, Some(0)),
                this_obj(3, Some(0))
            ])),
        "the persistent permute registers a covering ThisObject ephemeral marker (Gap-B)"
    );

    // Parked tail after X1 dispatched: [Y(src3), X2(src2)].
    let mut tail = group(vec![
        mk_ctx(3, Some(0), Some(CARD_Y), 3), // Y
        mk_ctx(2, Some(0), Some(CARD_X), 2), // X2
    ]);
    assert!(
        apply_trigger_order_template(&mut state, &mut tail),
        "the tail matches the ephemeral marker first (ephemeral-before-persistent consult)"
    );
    assert_eq!(
        source_ids(&tail),
        vec![3, 2],
        "COVERAGE-ONLY keeps the saved distinct order [Y, X2]; a direct-permute-on-tail impl \
         would invert to [X2, Y]"
    );
}

/// T4 (persistent Save route + finding #5 identity). Save through `apply()` builds a
/// persistent `AllCopies` template from the submitted order; the pin card-ids equal the
/// source objects' card-ids (the CANONICAL identity the matcher later reads as
/// `source_card_id`). A fresh batch of those two card identities is then auto-ordered to
/// the saved order by `apply_trigger_order_template` (the gate's 3rd arm).
#[test]
fn save_persistent_template_reapplies_in_saved_order() {
    use crate::types::actions::{GameAction, TriggerOrderTemplateOp};
    const CARD_A: u64 = 100;
    const CARD_B: u64 = 200;

    let mut state = GameState::new_two_player(7);
    // Two battlefield objects: id1 = card A, id2 = card B.
    let mut oa = crate::game::game_object::GameObject::new(
        ObjectId(1),
        CardId(CARD_A),
        PlayerId(0),
        "A".to_string(),
        crate::types::zones::Zone::Battlefield,
    );
    oa.incarnation = 0;
    let mut ob = crate::game::game_object::GameObject::new(
        ObjectId(2),
        CardId(CARD_B),
        PlayerId(0),
        "B".to_string(),
        crate::types::zones::Zone::Battlefield,
    );
    ob.incarnation = 0;
    state.objects.insert(ObjectId(1), oa);
    state.objects.insert(ObjectId(2), ob);

    // Save via apply(): sources = [id1, id2], order = [1, 0] ⇒ position0 = sources[1]
    // (id2, card B), position1 = sources[0] (id1, card A).
    super::super::engine::apply_as_current(
        &mut state,
        GameAction::SetTriggerOrderTemplate {
            op: TriggerOrderTemplateOp::Save {
                sources: vec![ObjectId(1), ObjectId(2)],
                order: vec![1, 0],
            },
        },
    )
    .expect("Save is an any-state preference action");

    // Exactly one persistent template registered for P0.
    let persistent: Vec<&DecisionTemplate> = state
        .decision_templates
        .iter()
        .filter(|t| t.key.is_persistent())
        .collect();
    assert_eq!(persistent.len(), 1, "one persistent template saved");
    let tmpl = persistent[0];
    assert_eq!(tmpl.owner, PlayerId(0), "owner forced to actor");

    // finding #5: the Save-derived pins read the objects' card_ids, at the submitted
    // positions.
    assert_eq!(
        tmpl.decisions,
        vec![
            PinnedDecision::Order {
                source: all_copies(CARD_B),
                pos: 0,
            },
            PinnedDecision::Order {
                source: all_copies(CARD_A),
                pos: 1,
            },
        ],
        "pins built from state.objects[source].card_id at the submitted order positions"
    );
    assert_eq!(
        state.objects[&ObjectId(2)].card_id,
        CardId(CARD_B),
        "the Save-derived pin card_id equals the source object's card_id (identity)"
    );

    // A fresh batch of those two card identities, in placement order [A, B], auto-orders
    // to the saved order [B, A] via the gate's 3rd arm — and the matcher reads
    // `source_card_id`, which equals the Save-derived card_id by construction.
    let mut fresh = group(vec![
        mk_ctx(5, Some(0), Some(CARD_A), 1), // fresh object, card A
        mk_ctx(6, Some(0), Some(CARD_B), 2), // fresh object, card B
    ]);
    assert!(
        apply_trigger_order_template(&mut state, &mut fresh),
        "the persistent template covers a fresh batch of the same card identities"
    );
    assert_eq!(
        source_ids(&fresh),
        vec![6, 5],
        "auto-ordered to the saved order [B(src6), A(src5)]"
    );

    // Discriminator: without the persistent template a fresh batch is not covered.
    let mut state2 = GameState::new_two_player(7);
    let mut fresh2 = group(vec![
        mk_ctx(5, Some(0), Some(CARD_A), 1),
        mk_ctx(6, Some(0), Some(CARD_B), 2),
    ]);
    assert!(
        !apply_trigger_order_template(&mut state2, &mut fresh2),
        "no saved template ⇒ not covered ⇒ would prompt"
    );
}
