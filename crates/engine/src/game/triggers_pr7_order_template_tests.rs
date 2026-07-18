//! PR-7 Part B2 — building-block tests for the trigger-ordering resolver's two
//! tiers (`apply_trigger_order_template`) plus live `OrderTriggers` persistence.
//!
//! CR 603.3b: a simultaneous batch is ordered ONCE; every subsequent parked-tail
//! re-drain is coverage-only. These tests pin the two-tier split:
//!   * ephemeral (`ThisObject`) = COVERAGE-ONLY (never permutes) — T2, T-dup.
//!   * persistent (`AllCopies`) = PERMUTE-ONCE + register ephemeral marker — T7, T4.
use super::*;
use crate::analysis::decision_template::{
    DecisionGroupKey, DecisionKind, DecisionTemplate, PinnedDecision, ReplayMode,
};
use crate::types::actions::GameAction;
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

fn mk_named_ctx(
    source_id: u64,
    incarnation: Option<u64>,
    card_id: u64,
    description: &str,
    count: i32,
) -> DeferredTrigger {
    let mut context = mk_ctx(source_id, incarnation, Some(card_id), count);
    context.pending.description = Some(description.to_string());
    context
}

fn group(triggers: Vec<DeferredTrigger>) -> TriggerOrderGroup {
    TriggerOrderGroup {
        controller: PlayerId(0),
        triggers,
        ordered: false,
    }
}

fn all_copies(card_id: u64, description: &str) -> YieldTarget {
    YieldTarget::AllCopies {
        card_id: CardId(card_id),
        trigger_description: Some(description.to_string()),
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

/// A duplicate persistent identity is ambiguous and must not auto-order a future batch.
#[test]
fn duplicate_or_legacy_persistent_templates_never_auto_order() {
    const CARD_X: u64 = 100;
    const CARD_Y: u64 = 200;
    let mut state = GameState::new_two_player(7);

    // Legacy wildcard + duplicate X identities are intentionally rejected.
    let persistent = DecisionTemplate {
        owner: PlayerId(0),
        decisions: vec![
            PinnedDecision::Order {
                source: YieldTarget::AllCopies {
                    card_id: CardId(CARD_X),
                    trigger_description: None,
                },
                pos: 0,
            },
            PinnedDecision::Order {
                source: all_copies(CARD_Y, "Y trigger"),
                pos: 1,
            },
            PinnedDecision::Order {
                source: all_copies(CARD_X, "X trigger"),
                pos: 2,
            },
        ],
        replay: ReplayMode::Static,
        key: DecisionGroupKey::from_sources(
            &[
                YieldTarget::AllCopies {
                    card_id: CardId(CARD_X),
                    trigger_description: None,
                },
                all_copies(CARD_Y, "Y trigger"),
                all_copies(CARD_X, "X trigger"),
            ],
            DecisionKind::TriggerOrdering,
        ),
    };
    state.set_trigger_order_template(persistent);

    // Fresh full batch is not eligible for a legacy wildcard preference.
    let mut fresh = group(vec![
        mk_named_ctx(1, Some(0), CARD_X, "X trigger", 1),
        mk_named_ctx(2, Some(0), CARD_X, "X trigger", 2),
        mk_named_ctx(3, Some(0), CARD_Y, "Y trigger", 3),
    ]);
    assert!(!apply_trigger_order_template(&mut state, &mut fresh));
    assert_eq!(
        source_ids(&fresh),
        vec![1, 2, 3],
        "ambiguous or legacy templates leave the live group for the player to order"
    );
}

/// A nonidentity live order with distinct named identities is saved and replays.
#[test]
fn submitted_persistent_template_reapplies_in_saved_order() {
    const CARD_A: u64 = 100;
    const CARD_B: u64 = 200;

    let mut state = GameState::new_two_player(7);
    let submitted = vec![
        mk_named_ctx(2, Some(0), CARD_B, "B trigger", 2),
        mk_named_ctx(1, Some(0), CARD_A, "A trigger", 1),
    ];
    record_submitted_trigger_order(&mut state, PlayerId(0), &submitted, false);

    // Exactly one persistent template registered for P0.
    let persistent: Vec<&DecisionTemplate> = state
        .decision_templates
        .iter()
        .filter(|t| t.key.is_persistent())
        .collect();
    assert_eq!(persistent.len(), 1, "one persistent template saved");
    let tmpl = persistent[0];
    assert_eq!(tmpl.owner, PlayerId(0), "owner forced to actor");

    assert_eq!(
        tmpl.decisions,
        vec![
            PinnedDecision::Order {
                source: all_copies(CARD_B, "B trigger"),
                pos: 0,
            },
            PinnedDecision::Order {
                source: all_copies(CARD_A, "A trigger"),
                pos: 1,
            },
        ],
        "pins retain card identity and the nonempty trigger discriminator"
    );

    // A fresh batch of those two card identities, in placement order [A, B], auto-orders
    // to the saved order [B, A] via the gate's 3rd arm — and the matcher reads
    // `source_card_id`, which equals the Save-derived card_id by construction.
    let mut fresh = group(vec![
        mk_named_ctx(5, Some(0), CARD_A, "A trigger", 1),
        mk_named_ctx(6, Some(0), CARD_B, "B trigger", 2),
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
        mk_named_ctx(5, Some(0), CARD_A, "A trigger", 1),
        mk_named_ctx(6, Some(0), CARD_B, "B trigger", 2),
    ]);
    assert!(
        !apply_trigger_order_template(&mut state2, &mut fresh2),
        "no saved template ⇒ not covered ⇒ would prompt"
    );
}

#[test]
fn only_nonidentity_distinct_named_orders_persist() {
    const CARD_A: u64 = 100;
    const CARD_B: u64 = 200;
    let mut state = GameState::new_two_player(7);

    let distinct = vec![
        mk_named_ctx(1, Some(0), CARD_A, "A trigger", 1),
        mk_named_ctx(2, Some(0), CARD_B, "B trigger", 2),
    ];
    record_submitted_trigger_order(&mut state, PlayerId(0), &distinct, true);

    let missing_description = vec![
        mk_ctx(3, Some(0), Some(CARD_A), 1),
        mk_named_ctx(4, Some(0), CARD_B, "B trigger", 2),
    ];
    record_submitted_trigger_order(&mut state, PlayerId(0), &missing_description, false);

    let duplicate_identity = vec![
        mk_named_ctx(5, Some(0), CARD_A, "A trigger", 1),
        mk_named_ctx(6, Some(0), CARD_A, "A trigger", 2),
    ];
    record_submitted_trigger_order(&mut state, PlayerId(0), &duplicate_identity, false);

    assert!(
        state.decision_templates.is_empty(),
        "identity, missing-description, and duplicate identities never become persistent preferences"
    );
}

#[test]
fn persistent_order_template_rejects_unrepresentable_positions() {
    let max_representable: Vec<_> = (1..=u64::from(u8::MAX) + 1)
        .map(|id| mk_named_ctx(id, Some(0), id, &format!("trigger {id}"), 1))
        .collect();
    let mut max_state = GameState::new_two_player(7);
    record_submitted_trigger_order(&mut max_state, PlayerId(0), &max_representable, false);

    let max_template = max_state
        .decision_templates
        .iter()
        .find(|template| template.key.is_persistent())
        .expect("256 distinct identities fit the u8 order-position range");
    assert_eq!(max_template.decisions.len(), usize::from(u8::MAX) + 1);
    assert!(matches!(
        max_template.decisions.last(),
        Some(PinnedDecision::Order { pos: u8::MAX, .. })
    ));
    assert!(
        is_specific_persistent_order_template(max_template),
        "the maximum representable template remains eligible for replay"
    );

    let unrepresentable: Vec<_> = (1..=u64::from(u8::MAX) + 2)
        .map(|id| mk_named_ctx(id, Some(0), id, &format!("trigger {id}"), 1))
        .collect();
    let mut overflow_state = GameState::new_two_player(7);
    record_submitted_trigger_order(&mut overflow_state, PlayerId(0), &unrepresentable, false);

    assert!(
        overflow_state.decision_templates.is_empty(),
        "257 distinct identities cannot be saved without truncating a u8 position"
    );
}

#[test]
fn live_order_triggers_persists_only_nonidentity_named_order() {
    const CARD_A: u64 = 100;
    const CARD_B: u64 = 200;

    let mut state = GameState::new_two_player(7);
    let disposition = begin_trigger_ordering(
        &mut state,
        vec![
            mk_named_ctx(1, Some(0), CARD_A, "A trigger", 1),
            mk_named_ctx(2, Some(0), CARD_B, "B trigger", 2),
        ],
    );
    let TriggerOrderingDisposition::PromptForChoice(prompt) = disposition else {
        panic!("distinct named triggers must reach an OrderTriggers prompt");
    };
    state.waiting_for = *prompt;

    super::super::engine::apply_as_current(
        &mut state,
        GameAction::OrderTriggers { order: vec![1, 0] },
    )
    .expect("a valid nonidentity OrderTriggers submission succeeds");

    let persistent: Vec<_> = state
        .decision_templates
        .iter()
        .filter(|template| template.key.is_persistent())
        .collect();
    assert_eq!(
        persistent.len(),
        1,
        "nonidentity live order saves one preference"
    );
    assert_eq!(
        persistent[0].decisions,
        vec![
            PinnedDecision::Order {
                source: all_copies(CARD_B, "B trigger"),
                pos: 0,
            },
            PinnedDecision::Order {
                source: all_copies(CARD_A, "A trigger"),
                pos: 1,
            },
        ],
        "the saved preference preserves the submitted nonidentity order"
    );

    let mut identity_state = GameState::new_two_player(7);
    let disposition = begin_trigger_ordering(
        &mut identity_state,
        vec![
            mk_named_ctx(3, Some(0), CARD_A, "A trigger", 1),
            mk_named_ctx(4, Some(0), CARD_B, "B trigger", 2),
        ],
    );
    let TriggerOrderingDisposition::PromptForChoice(prompt) = disposition else {
        panic!("distinct named triggers must reach an OrderTriggers prompt");
    };
    identity_state.waiting_for = *prompt;

    super::super::engine::apply_as_current(
        &mut identity_state,
        GameAction::OrderTriggers { order: vec![0, 1] },
    )
    .expect("a valid identity OrderTriggers submission succeeds");

    assert!(
        identity_state
            .decision_templates
            .iter()
            .all(|template| !template.key.is_persistent()),
        "identity live order does not save a persistent preference"
    );
}
