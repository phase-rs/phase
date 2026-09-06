//! CR 616.1e/f: a shock land played while a "lands enter untapped" source is on
//! the battlefield must offer the ordering choice, so the player can have the
//! untap effect apply LAST and win.
//!
//! The shock-land class ("As this land enters, you may pay 2 life. If you don't,
//! it enters tapped.") parses as `execute: None` with the enters-tapped write
//! living in `ReplacementMode::MayCost`'s `decline` branch. `candidate_materiality`
//! only walked `execute`, so the candidate classified `Disjoint`, no
//! `enter_tapped` collision with Spelunking was detected, and declining the
//! payment applied the tap unopposed — the land entered tapped with no ordering
//! prompt, contradicting Spelunking's ruling that the player chooses.
//!
//! Goes RED if the decline-branch arm in `candidate_materiality` is reverted.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHOCK_LAND: &str =
    "({T}: Add {R} or {G}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";

/// Plays a shock land under Spelunking, declining the life payment. `untap_last`
/// picks the ordering: when the untap is applied last it must win (CR 616.1f).
/// Returns `(entered_tapped, prompt_rounds, life_paid)`.
fn play_shockland_declining(untap_last: bool) -> (bool, usize, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Spelunking", "Lands you control enter untapped.");
    let mut builder = scenario.add_land_to_hand(P0, "Stomping Ground");
    builder.from_oracle_text(SHOCK_LAND);
    let land_id = builder.id();

    let mut runner = scenario.build();
    let starting_life = runner.state().players[0].life;
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let mut rounds = 0;
    while let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for {
        // Decline the "Pay 2 life" branch; in an ordering round, put the shock
        // land's tap first (so Spelunking's untap applies last) or vice versa.
        let labels: Vec<String> = candidates.iter().map(|c| c.description.clone()).collect();
        // Identify the ordering candidates by SOURCE, not by label text: the
        // shock land's ordering label is its full Oracle text, not "Enters
        // tapped" (its tap lives in the decline branch, so
        // `replacement_choice_label` falls back to the description).
        let untap_idx = candidates
            .iter()
            .position(|c| c.source_name == "Spelunking");
        let tap_idx = candidates
            .iter()
            .position(|c| c.source_name == "Stomping Ground");
        let pick = if let Some(i) = labels.iter().position(|d| d == "Decline") {
            i
        } else {
            // CR 616.1f: the effect applied LAST wins, so to make the untap win
            // we select the TAP first.
            let (first, other) = if untap_last {
                (tap_idx, untap_idx)
            } else {
                (untap_idx, tap_idx)
            };
            first.or(other).unwrap_or(0)
        };
        runner
            .act(GameAction::ChooseReplacement { index: pick })
            .expect("replacement choice should succeed");
        rounds += 1;
        assert!(rounds <= 6, "replacement prompt failed to terminate");
    }

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield, "the land entered play");
    (
        obj.tapped,
        rounds,
        starting_life - runner.state().players[0].life,
    )
}

#[test]
fn declining_shockland_under_spelunking_can_enter_untapped() {
    let (tapped, rounds, life_paid) = play_shockland_declining(true);

    // CR 616.1: declining the payment must NOT end the decision — the shock
    // land's tap and Spelunking's untap both write `enter_tapped`, so the
    // player is owed the ordering choice.
    assert!(
        rounds >= 2,
        "CR 616.1e: expected a second prompt to order the shock land's tap \
         against Spelunking's untap, got {rounds} round(s)"
    );

    // CR 616.1f: the untap was applied last, so it wins.
    assert!(
        !tapped,
        "CR 616.1f: with Spelunking's untap applied last the land must enter untapped"
    );

    // The payment was declined, so no life was paid.
    assert_eq!(life_paid, 0, "declining the shock payment costs no life");
}

#[test]
fn ordering_the_shockland_tap_last_still_enters_tapped() {
    // The mirror ordering stays reachable — CR 616.1e genuinely offers both
    // outcomes, so this must NOT be "Spelunking always wins".
    let (tapped, _rounds, _life_paid) = play_shockland_declining(false);
    assert!(
        tapped,
        "CR 616.1f: with the shock land's tap applied last the land enters tapped"
    );
}

/// CR 614.1c + CR 616.1e: the decline branch's self tap can sit on a CHAINED
/// `sub_ability` rather than at the root ("...enters with a +1/+1 counter and
/// enters tapped"). `candidate_materiality` must walk the decline chain the way
/// the applier does, or the tap is classified `Disjoint` and the ordering
/// prompt is suppressed exactly as it was for the root-level shock land.
///
/// The lead link is a SelfRef `PutCounter` — an event-modifier effect — because
/// `EventModifiers::event_modifiers_for_ability` walks only a contiguous prefix
/// of event modifiers. A non-modifier lead (e.g. `Draw`) stops the applier's
/// walk, so a tap behind it is never applied and must NOT be classified as an
/// `enter_tapped` write (see `chained_enter_tapped_commute_class`).
///
/// Goes RED if the decline walk is reduced to a root-only `decline.effect` check.
/// Drives the chained-decline scenario, declining the payment. `untap_last`
/// picks the ordering; returns `(entered_tapped, prompt_rounds)`.
fn play_chained_tapland_declining(untap_last: bool) -> (bool, usize) {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, EffectScope, QuantityExpr,
        ReplacementDefinition, ReplacementMode, TapStateChange, TargetFilter,
    };
    use engine::types::counter::CounterType;
    use engine::types::replacements::ReplacementEvent;

    // Decline branch: enter with a +1/+1 counter, THEN enter tapped. The lead
    // link is itself an event modifier, so the applier's walk reaches the tap.
    let chained_decline = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Spelunking", "Lands you control enter untapped.");
    let mut builder = scenario.add_land_to_hand(P0, "Chained Tapland");
    builder.with_replacement_definition(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                },
                decline: Some(Box::new(chained_decline)),
            })
            .description(
                "As ~ enters, you may pay 2 life. If you don't, it enters tapped.".to_string(),
            ),
    );
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let mut rounds = 0;
    while let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for {
        let labels: Vec<String> = candidates.iter().map(|c| c.description.clone()).collect();
        let pick = if let Some(i) = labels.iter().position(|d| d == "Decline") {
            i
        } else {
            // CR 616.1f: the effect applied LAST wins, so selecting the TAP
            // first makes the untap win, and vice versa.
            let tap = candidates
                .iter()
                .position(|c| c.source_name == "Chained Tapland");
            let untap = candidates
                .iter()
                .position(|c| c.source_name == "Spelunking");
            let (first, other) = if untap_last {
                (tap, untap)
            } else {
                (untap, tap)
            };
            first.or(other).unwrap_or(0)
        };
        runner
            .act(GameAction::ChooseReplacement { index: pick })
            .expect("replacement choice should succeed");
        rounds += 1;
        assert!(rounds <= 6, "replacement prompt failed to terminate");
    }

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield, "the land entered play");
    (obj.tapped, rounds)
}

/// CR 616.1e: the chained decline tap must surface the ordering prompt.
///
/// Goes RED if the decline walk is reduced to a root-only `decline.effect` check.
#[test]
fn chained_decline_tap_still_collides_with_an_untap_source() {
    let (tapped, rounds) = play_chained_tapland_declining(true);
    assert!(
        rounds >= 2,
        "CR 616.1e: a chained decline tap must still surface the ordering prompt, got {rounds} round(s)"
    );
    assert!(
        !tapped,
        "CR 616.1f: with the untap applied last the land must enter untapped"
    );
}

/// The MIRROR of the case above, and the half that makes it discriminating.
///
/// `!tapped` alone is equally consistent with "the chained tap was applied and
/// then correctly overwritten" and with "the chained tap was never applied at
/// all" — it passes in both the working and the broken world. This test pins
/// the opposite ordering: applying the chained tap LAST must leave the land
/// TAPPED, which is only true if the classifier and the applier agree that the
/// chained tap really writes `enter_tapped`. A degenerate ordering (or a
/// classifier/applier disagreement) fails one of the pair.
#[test]
fn ordering_the_chained_decline_tap_last_still_enters_tapped() {
    let (tapped, rounds) = play_chained_tapland_declining(false);
    assert!(
        rounds >= 2,
        "CR 616.1e: the ordering prompt must still be offered, got {rounds} round(s)"
    );
    assert!(
        tapped,
        "CR 616.1f: with the chained tap applied last the land must enter tapped —          if this fails while its mirror passes, the chained tap is never applied          and the classifier disagrees with the applier"
    );
}

/// CR 616.1 restore migration: `ReplacementChoice::kind` is `#[serde(default)]`,
/// so a save written before the field existed deserializes EVERY parked prompt
/// as `Order` — including an optional "you may" accept/decline. The frontend
/// keys its presentation off `kind`, so such a restore would render a sortable
/// ordering list for a yes/no decision.
///
/// Simulates a legacy save by stripping `kind` from the serialized JSON, then
/// asserts the restore re-derives it from the live pending replacement.
///
/// Goes RED if `migrate_restored_replacement_choice_kind` is removed from the
/// shared load chokepoint.
#[test]
fn legacy_save_restores_an_optional_prompt_as_optional_not_ordering() {
    use engine::game::scenario::GameRunner;
    use engine::types::game_state::{PersistedGameState, ReplacementChoiceKind};

    // Park a shock land's optional pay-2-life prompt (no untap source, so this
    // is a pure OptionalBranch decision).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_land_to_hand(P0, "Stomping Ground");
    builder.from_oracle_text(SHOCK_LAND);
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let WaitingFor::ReplacementChoice { kind, .. } = &runner.state().waiting_for else {
        panic!("expected the shock land to park an optional replacement prompt");
    };
    assert_eq!(
        *kind,
        ReplacementChoiceKind::OptionalBranch,
        "reach guard: a live park must classify the shock payment as OptionalBranch"
    );

    // Round-trip with `kind` stripped, exactly as a pre-field save would decode.
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the paused state serializes");
    let legacy = saved.replace(r#""kind":{"type":"OptionalBranch"},"#, "");
    assert_ne!(
        legacy, saved,
        "reach guard: the `kind` field must actually be stripped, or this test is vacuous"
    );

    let restored: PersistedGameState =
        serde_json::from_str(&legacy).expect("the legacy state deserializes");
    let runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );

    let WaitingFor::ReplacementChoice { kind, .. } = runner.state().waiting_for else {
        panic!("the restored state must still carry the parked replacement prompt");
    };
    assert_eq!(
        kind,
        ReplacementChoiceKind::OptionalBranch,
        "CR 616.1: a legacy save's optional prompt must restore as OptionalBranch, \
         not as the serde default `Order` — otherwise the client renders a \
         sortable ordering list for a yes/no decision"
    );
}

/// CR 616.1 restore migration, search-found shape. Companion to the optional
/// case above: a found-card destination prompt is a set of mutually exclusive
/// alternatives, NOT a sequence, so a legacy save must not restore it as
/// `Order` and render a sortable list.
///
/// Builds the parked prompt directly — a full search scenario is not needed to
/// exercise the classification seam the migration re-derives through.
///
/// Goes RED if `migrate_restored_replacement_choice_kind` is removed from the
/// shared load chokepoint.
#[test]
fn legacy_save_restores_a_search_found_prompt_as_search_found_not_ordering() {
    use engine::game::scenario::GameRunner;
    use engine::types::game_state::{
        PendingReplacement, PersistedGameState, ReplacementChoiceKind, ZoneDeliveryExileTracking,
    };
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::proposed_event::{
        BoundSearchFoundCandidate, BoundSearchFoundDisposition, ProposedEvent,
        SearchFoundDisposition,
    };
    use engine::types::ReplacementId;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Search Rider", "")
        .id();
    let mut runner = scenario.build();

    let rid = ReplacementId { source, index: 0 };
    let found = runner.state().objects[&source].id;
    let candidate = BoundSearchFoundCandidate {
        replacement_id: rid,
        disposition: BoundSearchFoundDisposition {
            destination: Zone::Exile,
            source: ObjectIncarnationRef {
                object_id: source,
                incarnation: runner.state().objects[&source].incarnation,
            },
            grant: None,
        },
        source_name: "Search Rider".to_string(),
        description: "Exile it instead".to_string(),
        is_optional: false,
    };
    let state = runner.state_mut();
    state.pending_replacement = Some(PendingReplacement {
        proposed: ProposedEvent::SearchFound {
            searcher: P0,
            library_owner: Some(P0),
            object_id: found,
            disposition: SearchFoundDisposition::Original,
            applied: Default::default(),
        },
        sacrifice_provenance: None,
        candidates: vec![rid],
        search_found_candidates: vec![candidate],
        depth: 0,
        is_optional: false,
        library_placement: None,
        exile_controller: None,
        exile_duration: None,
        exile_tracking: ZoneDeliveryExileTracking::None,
        excess_recipient: None,
        lifelink_bonus: 0,
        may_cost_paid: false,
        may_cost_remaining: None,
    });
    let parked = engine::game::replacement::replacement_choice_waiting_for(P0, runner.state());
    runner.state_mut().waiting_for = parked;

    let WaitingFor::ReplacementChoice { kind, .. } = runner.state().waiting_for else {
        panic!("expected a parked search-found replacement prompt");
    };
    assert_eq!(
        kind,
        ReplacementChoiceKind::SearchFoundDestination,
        "reach guard: a live park must classify this as SearchFoundDestination, \
         or the restore assertion below is vacuous"
    );

    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the paused state serializes");
    let legacy = saved.replace(r#""kind":{"type":"SearchFoundDestination"},"#, "");
    assert_ne!(
        legacy, saved,
        "reach guard: the `kind` field must actually be stripped, or this test is vacuous"
    );

    let restored: PersistedGameState =
        serde_json::from_str(&legacy).expect("the legacy state deserializes");
    let runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );

    let WaitingFor::ReplacementChoice { kind, .. } = runner.state().waiting_for else {
        panic!("the restored state must still carry the parked replacement prompt");
    };
    assert_eq!(
        kind,
        ReplacementChoiceKind::SearchFoundDestination,
        "CR 616.1: a legacy save's search-found prompt must restore as \
         SearchFoundDestination, not as the serde default `Order`"
    );
}

/// CR 616.1f: the ordering prompt's `last_applied_decides` flag must be TRUE for
/// a whole-field overwrite collision (the enters-tapped class), so the client may
/// name a concrete winning result.
///
/// The companion negative case lives in the engine unit tests: a compositional
/// collision (damage doubler vs adder) and the first-applied-wins EmptyManaPool
/// sentinel must both report FALSE, because naming a "winner" there would state
/// an outcome that does not exist (or the exact inverse).
///
/// Goes RED if the flag is hardcoded true or dropped from the payload.
#[test]
fn enter_tapped_ordering_prompt_reports_last_applied_decides() {
    use engine::types::game_state::ReplacementChoiceKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Spelunking", "Lands you control enter untapped.");
    let mut builder = scenario.add_land_to_hand(P0, "Stomping Ground");
    builder.from_oracle_text(SHOCK_LAND);
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    // With an untap source out, the tap-vs-untap ordering prompt is raised
    // alongside the shock land's own optional payment. Capture the flag from the
    // FIRST prompt whose kind is `Order` — that is the one the client renders as
    // a sortable list.
    let mut seen_order = None;
    let mut guard = 0;
    while let WaitingFor::ReplacementChoice {
        kind,
        last_applied_decides,
        ref candidates,
        ..
    } = runner.state().waiting_for
    {
        if kind == ReplacementChoiceKind::Order && seen_order.is_none() {
            seen_order = Some(last_applied_decides);
        }
        // Advance: decline a payment branch when offered, else take the first
        // ordering candidate.
        let pick = candidates
            .iter()
            .position(|c| c.description == "Decline")
            .unwrap_or(0);
        runner
            .act(GameAction::ChooseReplacement { index: pick })
            .expect("replacement choice should succeed");
        guard += 1;
        assert!(guard <= 6, "replacement prompt failed to terminate");
    }

    let last_applied_decides = seen_order
        .expect("reach guard: an Order prompt must occur, or the flag assertion is vacuous");
    assert!(
        last_applied_decides,
        "CR 616.1f: two single-target SetTapState writers each stamp the whole \
         `enter_tapped` field, so the last one applied decides the outcome"
    );
}
