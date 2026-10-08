//! CR 115.7 — `phase_ai::search::fallback_action` must answer a parked
//! `RetargetChoice` with a submission the reducer accepts.
//!
//! The fallback is the AI's last resort when scoring produces nothing, so a
//! fallback that proposes an illegal submission is a freeze by a second route.
//! Both rows drive `fallback_action` itself — the seam these rows exist to
//! carry. That is an ENTRY POINT, not an isolation: `fallback_action` ends in
//! `gate(action)`, which filters through `contract.contains_action`, and the
//! contract builds its candidates from the same engine enumerator that reaches
//! `retarget_actions` (`ai_support::context`'s `AiDecisionContract::issue` ->
//! `candidate_actions_for_semantic_owner_with_probe`). Its `issued` helper reads
//! `contract.candidates` even more directly. So these rows assert a
//! CO-DEPENDENCE property — the fallback answers the prompt only when the
//! generator and the fallback agree — and reverting EITHER side alone yields
//! `None` and fails the `is_some()` guard. They do not isolate the fallback from
//! the generator, and must not be cited as if they did.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    mana_multi_role, ControllerRef, Effect, ManaProduction, ManaTargetRole, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastingVariant, RetargetScope, RetargetSlotAddress, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::config::{create_config, AiDifficulty, Platform};

const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

/// `fallback_action` gates its result on the decision contract, which is exact
/// set membership against the contract's own candidates — so the contract MUST
/// be issued for the prompt's own player, or the row degrades into asserting
/// nothing.
fn fallback_for_prompt(runner: &GameRunner) -> Option<GameAction> {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let contract = engine::ai_support::AiDecisionContract::issue(runner.state(), P0);
    phase_ai::search::fallback_action(runner.state(), &config, &contract)
}

/// Row 2b — CR 115.7a: the fallback must propose ANOTHER legal target. At base
/// it returns `current_targets`, which `apply_retarget` rejects whenever the
/// current target has dropped out of the pool.
#[test]
fn fallback_retarget_action_is_legal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let alternative = scenario.add_creature(P0, "Goblin", 1, 1).id();
    scenario.add_creature(P0, "Bystander", 1, 1);
    let victim = scenario.add_creature(P1, "Bear", 2, 2).id();

    let mut runner = scenario.build();

    // The same fixture shape as the engine-side row: a single-target spell on
    // the stack whose current target is deliberately absent from the pool.
    let parsed = parse_oracle_text(
        LIGHTNING_BOLT_ORACLE,
        "Lightning Bolt",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let bolt_id = create_object(
        runner.state_mut(),
        CardId(77),
        P1,
        "Lightning Bolt".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&bolt_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    let bolt_ability = ResolvedAbility::new(
        parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(victim)],
        bolt_id,
        P1,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: bolt_id,
        source_id: bolt_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: Some(Box::new(bolt_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // phase-rs/phase#8355 round-8 review finding H2 (second pass):
    // production-shaped `slots`/`slot_pools`, not the outer-empty compat
    // shorthand this row used before (paired with a board-state Shroud
    // keyword to keep `victim` excluded from re-derivation under H3). That
    // pairing was the bug: an outer-empty `slot_pools` re-derives per-
    // position pools from the LIVE board (H3), so the row's exclusion of
    // `victim` depended on the keyword surviving unrelated board setup — and
    // without it, re-derivation reads `victim` back as a genuine, likely
    // FIRST, "any target" candidate. Measured (`Keyword::Shroud` removed,
    // shape otherwise unchanged): the generator proposes five candidates
    // including `victim`, the reducer accepts it, and `fallback_action`
    // proposes it too — the discriminating assertion below never fires
    // because the scenario the row claims to test ("current target dropped
    // out of the pool") no longer held. A production `Single` prompt is
    // never built with an empty `slot_pools` (INVARIANT SC) — pin the
    // fixture to that shape so the exclusion is guaranteed BY THE PAYLOAD,
    // not by a keyword surviving whatever re-derivation happens to read off
    // the board.
    let slot_pool = vec![TargetRef::Object(alternative)];

    // A directly parked prompt: no pending cast, so `fallback_action`'s
    // cancel-cast escape cannot fire before the `RetargetChoice` arm.
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: vec![TargetRef::Object(victim)],
        slots: vec![RetargetSlotAddress {
            path: vec![],
            slot: 0,
        }],
        slot_pools: vec![slot_pool.clone()],
        legal_new_targets: slot_pool,
    };

    let action = fallback_for_prompt(&runner);

    // Positive reach-guard, both halves: `Some` alone would also be satisfied by
    // a `CancelCast` returned from an escape that ran before the arm.
    assert!(action.is_some(), "reach guard: the fallback must answer");
    assert!(
        matches!(action, Some(GameAction::RetargetSpell { .. })),
        "reach guard: the RetargetChoice arm must be the one that answered, got {action:?}"
    );

    // Discriminating: the action must propose a target OTHER than victim
    // (CR 115.7a: "another legal target") — at base the action is
    // `RetargetSpell { [victim] }`, which the reducer rejects with "chosen
    // target not in legal alternatives". `victim` is not a member of the
    // declared `slot_pool`, so this asserts the CLAIM ("not the excluded
    // current target") against the exact pool both `fallback_action` and
    // `apply_retarget` read, rather than pinning one arbitrary enumeration-
    // order winner.
    assert_ne!(
        action,
        Some(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Object(victim)],
        }),
        "CR 115.7a: the fallback must propose ANOTHER legal target, not the \
         shrouded current target, got {action:?}"
    );
    runner
        .act(action.unwrap())
        .expect("the fallback's action must be accepted by the reducer");
}

/// Row 2f — CR 115.7a, at the `phase-ai` seam: a flat pool member legal only for
/// another slot must never be proposed, because `apply_retarget` re-checks each
/// changed submission against its own slot filter.
///
/// SCOPE OF THE CLAIM — read before citing this row. It pins SLOT LEGALITY at the
/// `phase-ai` fallback seam: that the fallback filters the flat pool through the
/// same authority the reducer applies. It does NOT claim the submission it
/// accepts is CR-115.7a / CR-115.7b-legal, and must not be cited as if it did.
///
/// This fixture is the same shape as `retarget_prompt_softlock.rs` row 2e:
/// `current_targets` has LENGTH 2 while the accepted submission has LENGTH 1,
/// because the reducer's `Single` arm hard-requires exactly one target.
/// (phase-rs/phase#8355 round-8 review LOW finding, correcting this note: with
/// `slots` populated — as this fixture does below — `apply_retarget`'s write is
/// PER-ADDRESS, not the compat branch's blanket `ability.targets = new_targets`.
/// Applying `[P1]` writes ONLY slot 0; `ability.targets` stays LENGTH 2 and slot
/// 1 keeps its prior value untouched. It is the outer-empty `slots` compat
/// branch alone that would truncate — not this fixture's write, and not any
/// LIVE production `Single` prompt, which is never built with an empty `slots`
/// under Invariant SC.) CR 115.7b's remedy (one slot changes, every other
/// declared target stays in place) is exactly what this per-address write does;
/// only CR 115.7a's all-or-none remedy would make a single-slot write WRONG for
/// a multi-target entry, and `RetargetScope::Single` is produced by both oracle
/// templates, so a correctness fix must dispatch on the template — row 2e's
/// SCOPE note carries the full two-template analysis, including why no printed
/// card reaches this arm under the CR-115.7a template. The length-1 acceptance
/// asserted below is recorded as OBSERVED CURRENT BEHAVIOUR rather than a
/// blanket endorsement, because `RetargetScope::Single` itself cannot tell the
/// two templates apart at this seam:
///
///   DEFERRED(out-of-run): interactive Single-scope retarget cannot honor CR
///   115.7a's all-or-none remedy for a synthetic multi-target entry reached
///   through the "change the target of" template — upstream cause filter.rs
///   FilterProp::HasSingleTarget is permissive with no resolution-time
///   validation; fix needs filter.rs + interaction.rs, both outside phase 1's
///   frozen scope.
#[test]
fn fallback_multi_role_retarget_action_is_slot_legal() {
    let mut runner = GameScenario::new().build();

    let role = ManaTargetRole::Both {
        // Slot 0 (recipient, surfaced first): only an opponent of P0, i.e. P1.
        recipient: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        // Slot 1 (count source): any player.
        count_source: TargetFilter::Player,
    };
    let source = create_object(
        runner.state_mut(),
        CardId(901),
        P0,
        "Multi-Role Mana Source".to_string(),
        Zone::Battlefield,
    );
    let entry_id = create_object(
        runner.state_mut(),
        CardId(901),
        P0,
        "Multi-Role Mana Ability".to_string(),
        Zone::Stack,
    );

    // Slot 0 holds P1, not P0 — so proposing `[P0]` would be a genuine change
    // into a slot it is illegal for, rather than an exempt non-change.
    let current_targets = vec![TargetRef::Player(P1), TargetRef::Player(P0)];
    let ability = ResolvedAbility::new(
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(role),
        },
        current_targets.clone(),
        source,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: entry_id,
        source_id: source,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ability),
        },
    });

    // Both structural reach-guards. (phase-rs/phase#8355 round-8 review LOW
    // finding: corrects a justification naming `retarget_actions`' `is_none_or`
    // — no such call exists in that function.) Without the first,
    // `retarget_actions`' own `entry.ability()` lookup fails and it returns NO
    // candidates, so the contract `fallback_action` consults offers nothing for
    // this prompt and the row's own `action.is_some()` reach-guard fails for the
    // wrong reason. Without the second, `chain_retarget_slots` no longer
    // produces the per-role-pair bindings this fixture's `slots` address, so
    // `retarget_slots_aligned` rejects the mismatch and `retarget_actions`
    // again returns nothing. Either degradation makes this row vacuous.
    assert!(
        runner.state().stack[0].ability().is_some(),
        "reach guard: stack index 0 must carry the ability under test"
    );
    assert!(
        mana_multi_role(&runner.state().stack[0].ability().unwrap().effect).is_some(),
        "reach guard: the node must be inside the per-slot admitted class"
    );

    // CR 115.7a / phase-rs/phase#8355 round 8 (H1): a production `Single`
    // prompt is never built with an empty `slot_pools` — every exposed
    // position gets its pool from `change_targets::slot_pool`. Populate the
    // same per-slot pools a real prompt would store, matching
    // `retarget_prompt_softlock.rs`'s `multi_role_mana_single_retarget_
    // candidates_are_slot_legal` fixture: slot 0 (recipient, opponent-only)
    // admits only P1; slot 1 (count source, any player) admits both.
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets,
        slots: vec![
            RetargetSlotAddress {
                path: vec![],
                slot: 0,
            },
            RetargetSlotAddress {
                path: vec![],
                slot: 1,
            },
        ],
        slot_pools: vec![
            vec![TargetRef::Player(P1)],
            vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        ],
        legal_new_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
    };

    let action = fallback_for_prompt(&runner);

    assert!(action.is_some(), "reach guard: the fallback must answer");
    assert!(
        matches!(action, Some(GameAction::RetargetSpell { .. })),
        "reach guard: the RetargetChoice arm must be the one that answered, got {action:?}"
    );

    // Discriminating: `[P0]` is in the flat pool but legal only for slot 1, so
    // it must never be the proposal; only the slot-0-legal `[P1]` may be.
    assert_eq!(
        action,
        Some(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Player(P1)],
        }),
        "CR 115.7a: the fallback must not propose a pool member legal only for \
         another slot"
    );

    // Discriminating: at base the action is `[P1, P0]` (length 2), which the
    // `Single` arm rejects outright.
    //
    // "Accepted by the reducer" is the WHOLE claim here — acceptance, not
    // rules-correctness. (phase-rs/phase#8355 round-8 review LOW finding,
    // correcting this note: with `slots` populated, as this fixture's payload
    // above does, `apply_retarget`'s write is PER-ADDRESS and does NOT
    // truncate — slot 1 keeps its prior value untouched; only the outer-empty
    // `slots` compat branch overwrites the whole vector.) This length-1
    // submission is CR-115.7b-correct; it is only a possible CR-115.7a
    // all-or-none concern under the unreachable-by-printed-card "change the
    // target of" template. See this row's SCOPE note and the
    // DEFERRED(out-of-run) entry it carries; this row deliberately asserts
    // only that the submission is accepted, and never asserts the resulting
    // `ability.targets`, because that template-dependent legality question is
    // exactly what `RetargetScope::Single` cannot answer at this seam.
    runner.act(action.unwrap()).expect(
        "the fallback's action must be ACCEPTED by the reducer — acceptance only, not a \
         claim of CR-115.7a / CR-115.7b legality; see this row's SCOPE note",
    );
}

/// A third player, so slot 0's "an opponent of P0" filter admits MORE THAN ONE
/// player. In a two-player game that filter admits exactly P1, so the only
/// slot-0-legal candidate is necessarily the current target — which survives via
/// the unchanged-position exemption rather than through the admit path.
const P2: PlayerId = PlayerId(2);

/// Builds the same two-slot mana node row 2f uses, but at an explicit player
/// count and with caller-chosen targets, so a row can put a genuine slot-0
/// CHANGE in the pool. Slot 0 (recipient) takes an opponent of P0; slot 1
/// (count source) takes any player.
fn park_multi_role_retarget(
    player_count: u8,
    current_targets: Vec<TargetRef>,
    legal_new_targets: Vec<TargetRef>,
) -> GameRunner {
    let mut runner = GameScenario::new_n_player(player_count, 42).build();

    let role = ManaTargetRole::Both {
        recipient: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        count_source: TargetFilter::Player,
    };
    let source = create_object(
        runner.state_mut(),
        CardId(902),
        P0,
        "Multi-Role Mana Source".to_string(),
        Zone::Battlefield,
    );
    let entry_id = create_object(
        runner.state_mut(),
        CardId(902),
        P0,
        "Multi-Role Mana Ability".to_string(),
        Zone::Stack,
    );
    let ability = ResolvedAbility::new(
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(role),
        },
        current_targets.clone(),
        source,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: entry_id,
        source_id: source,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ability),
        },
    });

    // The same two structural reach-guards row 2f carries (see its corrected
    // justification above, phase-rs/phase#8355 round-8 review LOW finding):
    // without the entry, `retarget_actions` returns NO candidates
    // (`entry.ability()` fails); without the per-slot admitted class,
    // `retarget_slots_aligned` rejects the resulting binding mismatch and
    // `retarget_actions` again returns nothing. Either way any row built here
    // would be vacuous.
    assert!(
        runner.state().stack[0].ability().is_some(),
        "reach guard: stack index 0 must carry the ability under test"
    );
    assert!(
        mana_multi_role(&runner.state().stack[0].ability().unwrap().effect).is_some(),
        "reach guard: the node must be inside the per-slot admitted class"
    );

    // CR 115.7a / phase-rs/phase#8355 round 8 (H1): a production `Single`
    // prompt is never built with an empty `slot_pools` — every exposed
    // position gets its pool from `change_targets::slot_pool`. Derive slot
    // 0's real pool from the caller's flat candidate list by the SAME filter
    // the role declares (`ControllerRef::Opponent`): P0 is never its own
    // opponent, so a caller-supplied candidate equal to P0 is honestly
    // ABSENT from slot 0's stored pool rather than merely rejected after
    // being offered. Slot 1 (count source, any player) has no such
    // restriction, so its pool is the caller's flat list unfiltered — and is
    // never consulted by a `Single` submission anyway (`apply_retarget`'s
    // `Single` arm only ever writes/admits position 0).
    let slot0_pool: Vec<TargetRef> = legal_new_targets
        .iter()
        .filter(|t| **t != TargetRef::Player(P0))
        .cloned()
        .collect();
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets,
        slots: vec![
            RetargetSlotAddress {
                path: vec![],
                slot: 0,
            },
            RetargetSlotAddress {
                path: vec![],
                slot: 1,
            },
        ],
        slot_pools: vec![slot0_pool, legal_new_targets.clone()],
        legal_new_targets,
    };
    runner
}

/// Row 2g — the ADMIT half of the per-slot authority, which row 2f cannot show.
///
/// Row 2f proves the filter REJECTS a pool member legal only for another slot.
/// It cannot prove the filter ADMITS a legal CHANGE, because its surviving
/// candidate equals the current slot-0 target and therefore passes through
/// `retarget_slot_violation`'s unchanged-position exemption. A generator that
/// dropped every changed proposal would still pass row 2f.
///
/// This row removes that escape by construction: the current slot-0 target (P1)
/// is deliberately ABSENT from the pool, so no exempt non-change exists and the
/// only candidate that can survive is a genuine, slot-0-legal CHANGE.
#[test]
fn fallback_multi_role_retarget_admits_a_legal_slot_change() {
    // Slot 0 currently holds P1. Pool offers P0 (illegal for slot 0 — P0 is not
    // its own opponent) and P2 (legal for slot 0, and a real change).
    let runner = park_multi_role_retarget(
        3,
        vec![TargetRef::Player(P1), TargetRef::Player(P0)],
        vec![TargetRef::Player(P0), TargetRef::Player(P2)],
    );

    // Drop-guard: the pool must genuinely exclude the current slot-0 target, or
    // the exemption is back and this row degenerates into row 2f.
    let WaitingFor::RetargetChoice {
        current_targets,
        legal_new_targets,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("fixture must park a RetargetChoice");
    };
    assert!(
        !legal_new_targets.contains(&current_targets[0]),
        "drop-guard: the current slot-0 target must be ABSENT from the pool, or the \
         surviving candidate would be an exempt non-change; got {legal_new_targets:?}"
    );

    let action = fallback_for_prompt(&runner);

    assert!(action.is_some(), "reach guard: the fallback must answer");

    // Discriminating, ADMIT side: the surviving proposal is the slot-0-legal
    // CHANGE. Under a generator that dropped changed proposals this is `None`;
    // under one that ignored slot legality it would be `[P0]`.
    assert_eq!(
        action,
        Some(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Player(P2)],
        }),
        "CR 115.7a: the fallback must propose the slot-0-legal CHANGE, and must not \
         propose the pool member legal only for the count-source slot"
    );
}

/// Row 2h — the `None` contract this layer deliberately introduces.
///
/// `fallback_action`'s retarget arm returns `None` when the engine's enumeration
/// is empty, and that refusal is deliberate: under `Single` scope an empty
/// enumeration means every pool member fails the per-slot check, and
/// `apply_retarget`'s `Single` arm would reject any submission built from that
/// pool. Returning a knowingly-rejected action instead would launder an engine
/// gap into an AI retry loop.
///
/// Nothing pinned that contract, so a future change could silently restore a
/// rejected submission and no test would notice. This row pins it.
#[test]
fn fallback_multi_role_retarget_yields_none_when_no_pool_member_is_slot_legal() {
    // Slot 0 holds P1 and the pool offers only P0, which is illegal for slot 0.
    // P1 is absent, so there is no exempt non-change to fall back on either.
    let runner = park_multi_role_retarget(
        3,
        vec![TargetRef::Player(P1), TargetRef::Player(P0)],
        vec![TargetRef::Player(P0)],
    );

    // Positive control for a negative assertion: the prompt really is parked and
    // its pool really is non-empty, so a `None` below means "every candidate was
    // filtered out", never "there was nothing to filter" or "no prompt existed".
    let WaitingFor::RetargetChoice {
        legal_new_targets, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("positive control: the fixture must park a RetargetChoice");
    };
    assert!(
        !legal_new_targets.is_empty(),
        "positive control: the pool must be NON-empty, or `None` proves nothing about \
         the per-slot filter"
    );

    assert_eq!(
        fallback_for_prompt(&runner),
        None,
        "the retarget arm must refuse rather than submit an action `apply_retarget` \
         would reject; see the DEFERRED(out-of-run) note in `phase-ai/src/search.rs`"
    );
}
