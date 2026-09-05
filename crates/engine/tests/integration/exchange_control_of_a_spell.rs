//! Phase 2 of the Perplexing Chimera run — "exchange control of a spell"
//! (U8: `exchange_control.rs`'s zone gate widened from Battlefield-only to
//! Battlefield-or-Stack, CR 701.12a + CR 400.7a).
//!
//! Covers Verification Matrix rows V17, V18 (plan-r6 §Verification Matrix,
//! Stage-2 rows). This is the "definition of done" pair: V17 proves the class
//! (Sudden Substitution — declared targets, not a context ref) and V18 proves
//! the card (Perplexing Chimera, end to end through the real cast pipeline).

use engine::game::scenario::{CastCommit, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, ChainStep, WaitingFor};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim from `client/public/card-data.json`.
const SHIFTING_GRIFT_TEXT: &str = "Spree (Choose one or more additional costs.)\n+ {2} — \
    Exchange control of two target creatures.\n+ {1} — Exchange control of two target \
    artifacts.\n+ {1} — Exchange control of two target enchantments.";

/// Verbatim from `client/public/card-data.json`.
const KARONA_FALSE_GOD_AVATAR_TEXT: &str = "At the beginning of your upkeep, exchange control \
    of a permanent you control chosen at random and a permanent target opponent controls \
    chosen at random.";

/// Verbatim from `client/public/card-data.json`.
const MISTER_NEGATIVE_TEXT: &str = "Vigilance, lifelink\nDarkforce Inversion — When Mister \
    Negative enters, you may exchange life totals with target opponent. If you lost life this \
    way, draw that many cards.";

/// A generous colorless mana pool — enough to cover any combination of
/// Shifting Grift's Spree additional costs (max `{2}+{1}+{1} = {4}`, with the
/// printed `{U}{U}` zeroed by `.with_mana_cost(ManaCost::zero())` at each call
/// site) without needing specific colors.
fn generic_mana_pool() -> Vec<ManaUnit> {
    std::iter::repeat_with(|| {
        ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        )
    })
    .take(8)
    .collect()
}

const PERPLEXING_CHIMERA_TEXT: &str = "Whenever an opponent casts a spell, you may exchange \
    control of this creature and that spell. If you do, you may choose new targets for the \
    spell. (If the spell becomes a permanent, you control that permanent.)";

const SUDDEN_SUBSTITUTION_TEXT: &str = "Split second (As long as this spell is on the stack, \
    players can't cast spells or activate abilities that aren't mana abilities.)\nExchange \
    control of target noncreature spell and target creature. Then the spell's controller may \
    choose new targets for it.";

/// The unrestricted single-target pump clause. A TARGETED triggering spell is
/// required for the Chimera rows: `change_targets` takes its
/// `current_targets.is_empty()` no-op arm for a vanilla creature spell, which
/// makes the retarget prompt structurally unreachable and the assertion vacuous.
const PUMP_TEXT: &str = "Target creature gets +1/+1 until end of turn.";

/// Verbatim from `client/public/card-data.json`.
const GILDED_DRAKE_TEXT: &str = "Flying\nWhen this creature enters, exchange control of this \
    creature and up to one target creature an opponent controls. If you don't or can't make an \
    exchange, sacrifice this creature. This ability still resolves if its target becomes illegal.";

/// The generic "steal a creature for the turn" clause, used to stage the CR
/// 701.12b same-controller collision through a REAL continuous effect. Writing
/// `state.objects[drake].controller = P1` directly does not work: the layer
/// flush at the end of resolution reverts it before any assertion can read it.
const GAIN_CONTROL_TEXT: &str = "Gain control of target creature until end of turn.";

/// Pass priority until the current committed cast's next trigger raises its
/// own `OptionalEffectChoice`, or panic with a diagnosable message if the
/// stack empties first.
fn advance_to_optional_choice(commit: &mut CastCommit<'_>) {
    for _ in 0..40 {
        match commit.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                if commit.state().stack.is_empty() {
                    panic!("the stack emptied without ever raising an OptionalEffectChoice");
                }
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining to the prompt");
            }
            ref other => panic!("unexpected waiting state while draining to the prompt: {other:?}"),
        }
    }
    panic!("did not reach OptionalEffectChoice within 40 iterations");
}

// ---------------------------------------------------------------------------
// V17 — ExchangeControl accepts a stack subject (the class)
// ---------------------------------------------------------------------------

/// V17: Sudden Substitution — declared targets (not a context ref) — proves
/// the zone-gate widening (U8) in isolation from the context-ref machinery
/// (U4-U7): P1 casts a noncreature spell that draws cards; P0 casts Sudden
/// Substitution (verbatim Oracle text, Split Second) targeting that spell and
/// a creature of their own (CR 701.12b requires the two subjects to start
/// with different controllers, or the exchange does nothing).
///
/// Asserts (a) the creature's controller swapped, and (b) the exchanged
/// spell RESOLVES UNDER P0's CONTROL (`assert_hand_drawn(P0, 2)`, not P1) —
/// the CR 608.2c claim that a stack-subject exchange re-stamps who the spell
/// resolves for, not merely who controls a battlefield object.
///
/// REVERT-FAILING: reverting the zone gate (P2.5) makes the spell an illegal
/// exchange subject — `obj_a.zone != Zone::Battlefield` — so the entire
/// exchange no-ops (CR 701.12a) and P1 draws the cards instead.
///
/// Class D (Sudden Substitution's own "the spell's controller may choose new
/// targets for it" — a non-`you` chooser) is deliberately OUT OF RUN; this
/// row declines that offer.
#[test]
fn sudden_substitution_transfers_the_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Divination draws 2 cards for WHICHEVER player ends up controlling it —
    // that's the point of the test — so both libraries need enough cards
    // that drawing 2 doesn't deck either player out before the assertions run.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);
    // CR 701.12b: the two exchange subjects must have DIFFERENT controllers
    // or the exchange does nothing — P0 gives up a creature of their own to
    // take control of P1's spell.
    let p0_creature = scenario.add_creature(P0, "P0 Creature", 2, 2).id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P1, "Divination", false, "Draw two cards.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let sudden_substitution = scenario
        .add_spell_to_hand(P0, "Sudden Substitution", true)
        .from_oracle_text_with_keywords(&["Split second"], SUDDEN_SUBSTITUTION_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut divination_commit = runner.cast(divination).commit();
    let divination_stack_id = divination_commit.state().stack.back().unwrap().id;

    // REACH GUARD: Divination itself must be on the stack before P0 responds.
    assert_eq!(divination_commit.state().stack.len(), 1);

    {
        let state = divination_commit.state_mut();
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let outcome = divination_commit
        .cast(sudden_substitution)
        .target_objects(&[divination_stack_id, p0_creature])
        .decline_optional()
        .resolve();

    // (a) the creature's controller swapped (to P1 — the opposite direction
    // of the spell, since CR 701.12b requires the two subjects to start with
    // different controllers).
    assert_eq!(
        outcome
            .state()
            .objects
            .get(&p0_creature)
            .unwrap()
            .controller,
        P1,
        "P0's creature must swap to P1"
    );
    // (b) the exchanged spell resolves under P0's control.
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_hand_drawn(P1, 0);
}

/// SIBLING (V17): the ordinary two-permanent path (Switcheroo shape) is
/// unaffected by the zone-gate widening — proves `control_is_exchangeable`
/// still accepts (and only accepts) Battlefield for the ordinary case.
#[test]
fn exchange_control_between_two_battlefield_permanents_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_a = scenario.add_creature(P0, "Creature A", 2, 2).id();
    let creature_b = scenario.add_creature(P1, "Creature B", 3, 3).id();
    let switcheroo = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Switcheroo",
            false,
            "Exchange control of two target creatures.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(switcheroo)
        .target_objects(&[creature_a, creature_b])
        .resolve();

    assert_eq!(
        outcome.state().objects.get(&creature_a).unwrap().controller,
        P1
    );
    assert_eq!(
        outcome.state().objects.get(&creature_b).unwrap().controller,
        P0
    );
}

/// HOSTILE (V17): the targeted spell is countered in response, before Sudden
/// Substitution resolves — CR 701.12a: the exchange can't be completed (the
/// spell is no longer on the stack, no longer battlefield either), so
/// nothing swaps. The creature stays with its original controller too
/// (CR 701.12a all-or-nothing — not a partial swap).
#[test]
fn sudden_substitution_hostile_target_spell_countered_in_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p1_creature = scenario.add_creature(P1, "P1 Creature", 2, 2).id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P1, "Divination", false, "Draw two cards.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let sudden_substitution = scenario
        .add_spell_to_hand(P0, "Sudden Substitution", true)
        .from_oracle_text_with_keywords(&["Split second"], SUDDEN_SUBSTITUTION_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut divination_commit = runner.cast(divination).commit();
    let divination_stack_id = divination_commit.state().stack.back().unwrap().id;
    {
        let state = divination_commit.state_mut();
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let mut ss_commit = divination_commit
        .cast(sudden_substitution)
        .target_objects(&[divination_stack_id, p1_creature])
        .decline_optional()
        .commit();
    // Split second forbids further casts while Sudden Substitution is on the
    // stack, so P1 can't respond with a real counterspell here — instead this
    // row exercises the same seam the plan names (a spell leaving the stack
    // before the exchange resolves) by removing Divination from the stack
    // directly, the same way an SBA-driven fizzle or a resolved counter would.
    {
        let state = ss_commit.state_mut();
        state.stack.retain(|e| e.id != divination_stack_id);
    }
    let outcome = ss_commit.resolve();
    assert_eq!(
        outcome
            .state()
            .objects
            .get(&p1_creature)
            .unwrap()
            .controller,
        P1,
        "CR 701.12a all-or-nothing: with the spell subject gone, the creature must NOT swap either"
    );
    // CR 608.2b + CR 701.12a — INDEX-DISCIPLINE PIN for
    // `ability_utils::validate_targets_in_chain`'s ExchangeControl arm. That
    // arm PRUNES the illegal slot-A target, which slides the surviving slot-B
    // target into slot A's position; `exchange_control::resolve_slot` then
    // reads the creature into slot A and finds nothing for slot B. Pruning is
    // safe only because that second lookup runs dry and takes CR 701.12a's
    // all-or-nothing early return BEFORE any continuous effect is written.
    // Asserting "no continuous effect at all" (not merely "the creature kept
    // its controller") is what makes a future partially-completable
    // ExchangeControl fail here instead of silently exchanging the wrong
    // object.
    assert!(
        outcome.state().transient_continuous_effects.is_empty(),
        "no partial or mis-bound exchange may be written when one subject is illegal"
    );
}

/// SIBLING (V17) — CR 608.2b re-validation is now filter-aware for the
/// ORDINARY two-permanent path, not just the stack-subject one.
///
/// Before this change `ExchangeControl` fell to `validate_targets_in_chain`'s
/// generic `None` branch, which re-checked only `state.battlefield.contains`.
/// A target that stayed on the battlefield but stopped satisfying the
/// ability's own filter therefore survived re-validation and got exchanged.
/// The dedicated arm re-validates against each declared filter, so a creature
/// that is no longer a creature when Switcheroo resolves is illegal
/// (CR 608.2b: "its characteristics may have changed"), and with one subject
/// illegal the exchange can't be completed (CR 701.12a).
///
/// This row exists because the arm changes re-validation for EVERY card that
/// parses to `ExchangeControl`, not only the two cards in this run's scope.
///
/// REVERT-FAILING: restoring the generic battlefield-only check makes the
/// de-typed permanent a legal target again and both controllers swap.
#[test]
fn exchange_control_target_that_stops_matching_its_filter_is_illegal_on_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_a = scenario.add_creature(P0, "Creature A", 2, 2).id();
    let creature_b = scenario.add_creature(P1, "Creature B", 3, 3).id();
    let switcheroo = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Switcheroo",
            false,
            "Exchange control of two target creatures.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let mut commit = runner
        .cast(switcheroo)
        .target_objects(&[creature_a, creature_b])
        .commit();

    // REACH GUARD: both targets must have been accepted at announcement, or
    // this row would prove nothing about RESOLUTION-time re-validation.
    assert_eq!(
        commit.state().stack.len(),
        1,
        "REACH GUARD: Switcheroo must be on the stack with its two targets"
    );

    // "In response", creature B stops being a creature (it stays on the
    // battlefield, so the old battlefield-only check would still accept it).
    {
        let state = commit.state_mut();
        state
            .objects
            .get_mut(&creature_b)
            .expect("creature B exists")
            .card_types
            .core_types = vec![CoreType::Artifact];
    }

    let outcome = commit.resolve();

    assert!(
        outcome.state().transient_continuous_effects.is_empty(),
        "CR 608.2b + CR 701.12a: with one subject no longer matching its filter, no part of \
         the exchange occurs"
    );
    assert_eq!(
        outcome.state().objects.get(&creature_a).unwrap().controller,
        P0,
        "creature A must keep its controller"
    );
    assert_eq!(
        outcome.state().objects.get(&creature_b).unwrap().controller,
        P1,
        "creature B must keep its controller"
    );
}

// ---------------------------------------------------------------------------
// V18 — Perplexing Chimera, end to end
// ---------------------------------------------------------------------------

/// V18: Perplexing Chimera, full card, end to end. P1 casts a real creature
/// spell (Grizzly Bears — vanilla, so its own retarget offer is a guaranteed
/// no-op per CR 115.7's empty-target-list guard, keeping this row focused on
/// the exchange itself). P0 accepts the optional trigger. Assert the Chimera
/// is now P1's, the (former) spell is now P0's, and — since it becomes a
/// permanent — it enters under P0's control with `base_controller == P1`
/// (CR 110.2b).
///
/// REVERT-FAILING: reverting any Stage-2 unit fails this row at a distinct
/// point — P2.1 reverted drops the trigger entirely (V12's own failure);
/// P2.2 reverted makes the exchange a total no-op (V14's failure); P2.5
/// reverted no-ops the zone gate (V17's failure). This row is the
/// end-to-end conjunction of all of them.
#[test]
fn perplexing_chimera_steals_the_spell_end_to_end() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let grizzly_bears = scenario
        .add_creature_to_hand_from_oracle(P1, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(grizzly_bears).accept_optional().resolve();

    assert_eq!(
        outcome.state().objects.get(&chimera).unwrap().controller,
        P1,
        "the Chimera itself must swap to P1"
    );
    assert_eq!(
        outcome.zone_of(grizzly_bears),
        Zone::Battlefield,
        "REACH GUARD: Grizzly Bears must actually resolve onto the battlefield — a fizzle \
         cannot pass this row"
    );
    let bears = outcome.state().objects.get(&grizzly_bears).unwrap();
    assert_eq!(
        bears.controller, P0,
        "CR 400.7a: the permanent the spell becomes enters under the new controller"
    );
    assert_eq!(
        bears.base_controller,
        Some(P1),
        "CR 110.2b: the permanent's by-default controller is still the player who put the \
         spell on the stack"
    );
}

/// SIBLING (V18): declining the optional trigger leaves everything
/// unchanged — the Chimera stays P0's, and Grizzly Bears resolves for P1 as
/// normal. This is also the by-construction inertness pin: absent an
/// accepted exchange, nothing about the fix changes ordinary play.
#[test]
fn perplexing_chimera_declined_trigger_changes_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let grizzly_bears = scenario
        .add_creature_to_hand_from_oracle(P1, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(grizzly_bears).decline_optional().resolve();

    assert_eq!(
        outcome.state().objects.get(&chimera).unwrap().controller,
        P0,
        "declining the exchange must leave the Chimera with P0"
    );
    let bears = outcome.state().objects.get(&grizzly_bears).unwrap();
    assert_eq!(
        bears.controller, P1,
        "Grizzly Bears resolves for its caster, P1, as normal"
    );
}

/// HOSTILE (V18): Perplexing Chimera destroyed in response to its own
/// trigger — CR 701.12a: no part of the exchange occurs. The SelfRef source
/// is no longer current (CR 400.7), so `targeting::resolved_targets` binds
/// it to nothing rather than a stale id.
///
/// The triggering spell is a TARGETED one. With the vanilla creature spell
/// this row used to cast, `change_targets` took its `current_targets.is_empty()`
/// no-op arm and the retarget prompt was structurally unreachable — so the row
/// could not observe the "If you do" defect at all. A targeted spell makes the
/// prompt reachable, which is what turns the added assertion below into a real
/// discriminator rather than a tautology.
#[test]
fn perplexing_chimera_destroyed_in_response_to_its_own_trigger_is_a_total_noop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let pump = scenario
        .add_spell_to_hand_from_oracle(P1, "Giant Growth", true, PUMP_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut commit = runner.cast(pump).target_objects(&[p1_bear]).commit();
    advance_to_optional_choice(&mut commit);
    match commit.state().waiting_for {
        WaitingFor::OptionalEffectChoice { source_id, .. } => {
            assert_eq!(
                source_id, chimera,
                "REACH GUARD: the prompt must be the Chimera's own"
            );
        }
        ref other => panic!("expected the Chimera's OptionalEffectChoice, got {other:?}"),
    }

    // Destroy the Chimera "in response" — before its own trigger's choice is
    // answered — by moving it directly to the graveyard.
    {
        let state = commit.state_mut();
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(state, chimera, Zone::Graveyard, &mut events);
    }

    commit
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting must not panic even though the source is gone");
    assert!(
        commit.state().transient_continuous_effects.is_empty(),
        "CR 701.12a: no part of the exchange occurs once the SelfRef source is gone"
    );
    assert_eq!(
        commit.state().objects.get(&pump).unwrap().zone,
        Zone::Stack,
        "REACH GUARD: the triggering spell must still be on the stack (unresolved) here"
    );

    // CR 608.2c: and no retarget offer is EVER raised, all the way to the end
    // of the resolution. Pre-fix the accept latch left the outcome flag true,
    // so "If you do, you may choose new targets for the spell" fired after an
    // exchange CR 701.12a had refused. Drained by hand rather than through a
    // declining policy, because a policy that auto-declines the second offer
    // would answer the very prompt this row exists to prove is never raised.
    for _ in 0..40 {
        match &commit.state().waiting_for {
            WaitingFor::RetargetChoice { .. } => {
                panic!("a retarget offer was raised for an exchange that never happened")
            }
            WaitingFor::OptionalEffectChoice { source_id, .. } => panic!(
                "a second optional offer was raised for an exchange that never happened \
                 (source {source_id:?})"
            ),
            WaitingFor::Priority { .. } => {
                if commit.state().stack.is_empty() {
                    break;
                }
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining");
            }
            other => panic!("unexpected state while draining to the end: {other:?}"),
        }
    }

    // REACH GUARD: the spell really did resolve for its own caster, so the
    // drain above was not short-circuited by a fizzle.
    assert_eq!(
        commit.state().objects.get(&pump).unwrap().zone,
        Zone::Graveyard,
        "REACH GUARD: the triggering spell must have resolved"
    );
    assert_eq!(
        commit.state().objects.get(&p0_bear).unwrap().controller,
        P0,
        "P0's creature was never part of any exchange"
    );
    assert_eq!(commit.state().objects.get(&p1_bear).unwrap().controller, P1);
}

// ---------------------------------------------------------------------------
// V1 / V2 — an accepted Chimera trigger whose exchange did not occur
// ---------------------------------------------------------------------------

/// V1 — CR 701.12a + CR 608.2c: accepting the Chimera's "you may exchange
/// control of this creature and that spell" does NOT entitle you to the
/// printed "If you do, you may choose new targets for the spell" when the
/// exchange could not be made.
///
/// Production entry chain: `resolve_optional_effect_decision` (accept — which
/// LOWERS `optional` and LATCHES the performed flag true) → `resolve_ability_chain`
/// → the resolver-verdict block → the sub descent → `evaluate_condition`'s
/// `EffectOutcome { OptionalEffectPerformed }` arm.
///
/// First production branch reached: `exchange_control.rs`'s
/// `let Some(id_a) = resolve_slot(target_a, ..) else` arm — `resolved_targets`
/// binds nothing for `TargetFilter::SelfRef` once CR 400.7's currency check
/// fails on a Chimera that has left the battlefield.
///
/// REVERT-FAILING: pre-fix, nothing downstream of the accept latch ever lowers
/// the flag, so the gate reads true and the engine raises the "you may choose
/// new targets" offer (itself optional, hence a SECOND `OptionalEffectChoice`)
/// and then `RetargetChoice` — handing P0 a free retarget of an opponent's
/// spell it never gained control of. Post-fix the sub is skipped outright.
#[test]
fn an_accepted_chimera_exchange_that_did_not_happen_offers_no_retarget() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let _p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let pump = scenario
        .add_spell_to_hand_from_oracle(P1, "Giant Growth", true, PUMP_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut commit = runner.cast(pump).target_objects(&[p1_bear]).commit();
    advance_to_optional_choice(&mut commit);

    // Destroy the Chimera in response, so the exchange CANNOT be made
    // (CR 701.12a all-or-nothing).
    {
        let state = commit.state_mut();
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(state, chimera, Zone::Graveyard, &mut events);
    }
    commit
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the exchange must be legal");

    // THE DISCRIMINATOR, read at the prompt boundary: the very next thing the
    // engine asks for is neither the "you may choose new targets" offer nor the
    // retarget itself.
    match commit.state().waiting_for {
        WaitingFor::OptionalEffectChoice { source_id, .. } => panic!(
            "a second optional offer was raised for an exchange that never happened \
             (source {source_id:?}) — the \"If you do\" gate read true"
        ),
        WaitingFor::RetargetChoice { .. } => {
            panic!("a retarget offer was raised for an exchange that never happened")
        }
        _ => {}
    }

    // CO-WITNESS: nothing was exchanged.
    assert!(
        commit.state().transient_continuous_effects.is_empty(),
        "CR 701.12a: no part of the exchange occurs"
    );
}

/// V2 — V1's PAIRED POSITIVE REACH GUARD. The same board WITHOUT the destroy
/// still reaches the retarget offer, so V1's two negatives cannot pass because
/// the fixture never built a targeted spell or never reached the gate at all.
#[test]
fn an_accepted_chimera_exchange_that_happened_still_offers_the_retarget() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let pump = scenario
        .add_spell_to_hand_from_oracle(P1, "Giant Growth", true, PUMP_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut commit = runner.cast(pump).target_objects(&[p1_bear]).commit();

    let mut reached = None;
    for _ in 0..40 {
        match &commit.state().waiting_for {
            WaitingFor::RetargetChoice {
                player,
                current_targets,
                legal_new_targets,
                ..
            } => {
                reached = Some((*player, current_targets.clone(), legal_new_targets.clone()));
                break;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                commit
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting must succeed");
            }
            WaitingFor::Priority { .. } => {
                assert!(
                    !commit.state().stack.is_empty(),
                    "the stack emptied before the retarget offer was raised"
                );
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining");
            }
            other => panic!("unexpected state while draining to the retarget offer: {other:?}"),
        }
    }
    let (chooser, current, pool) = reached.expect("REACH GUARD: the retarget offer must be raised");

    // REACH GUARD: the exchange really happened.
    assert_eq!(
        commit.state().objects.get(&chimera).unwrap().controller,
        P1,
        "REACH GUARD: the Chimera must have swapped to P1"
    );
    assert_eq!(
        commit.state().transient_continuous_effects.len(),
        2,
        "CR 613.1b: the exchange installs one ChangeController effect per subject"
    );

    assert_eq!(chooser, P0, "CR 115.7: the spell's new controller chooses");
    assert_eq!(
        current,
        vec![TargetRef::Object(p1_bear)],
        "the offer is made against the spell's existing target"
    );
    assert!(
        pool.contains(&TargetRef::Object(p0_bear)),
        "an unrestricted \"target creature\" pool must offer P0's own creature too \
         (pool was {pool:?})"
    );
}

/// BLAST-RADIUS PIN (review round 2) — Gilded Drake's disposition when its
/// sole declared target becomes illegal while staying on the battlefield.
///
/// `validate_targets_in_chain`'s `ExchangeControl` arm re-validates against
/// each declared filter, where the generic branch it replaced checked only
/// `state.battlefield.contains`. For Gilded Drake ("exchange control of this
/// creature and up to one target creature an opponent controls. If you don't
/// or can't make an exchange, sacrifice this creature.") that flips the
/// outcome when the target stops being a creature in response:
///
///   * BEFORE — the target survived re-validation, so the ability resolved
///     and the exchange RAN against an illegal target. Plainly wrong.
///   * AFTER  — the target is illegal, it is this ability's only instance of
///     the word "target", so per CR 608.2b the ability doesn't resolve. This
///     is the correct DEFAULT, and it is what this row pins.
///
/// KNOWN GAP, deliberately not fixed here: Gilded Drake's printed "This
/// ability still resolves if its target becomes illegal" is an explicit CR
/// 608.2b exception that the parser does not model at all — the clause is
/// dropped, and `optional_targeting` is `false` despite "up to one target".
/// With it modelled, the ability would resolve, the exchange would not
/// happen, and the Drake would be sacrificed. Representing that exception is
/// a parser + AST change well outside this run; this row exists so the
/// current disposition is a recorded decision rather than an unnoticed side
/// effect, and so it fails loudly when the exception is implemented.
#[test]
fn gilded_drake_sole_target_that_stops_matching_its_filter_stops_the_ability() {
    use engine::game::ability_utils::validate_targets_in_chain;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        ControllerRef, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;

    let mut state = GameState::new_two_player(42);
    let drake = create_object(
        &mut state,
        CardId(1),
        P0,
        "Gilded Drake".to_string(),
        Zone::Battlefield,
    );
    let victim = create_object(
        &mut state,
        CardId(2),
        P1,
        "Victim".to_string(),
        Zone::Battlefield,
    );

    let mut ability = ResolvedAbility::new(
        Effect::ExchangeControl {
            target_a: TargetFilter::SelfRef,
            target_b: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ),
        },
        vec![TargetRef::Object(victim)],
        drake,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![],
        drake,
        P0,
    )));

    // REACH GUARD: while the victim IS a creature the target is kept, so this
    // row is exercising the re-validation seam and not an unrelated drop.
    state
        .objects
        .get_mut(&victim)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];
    assert_eq!(
        validate_targets_in_chain(&state, &ability).targets,
        vec![TargetRef::Object(victim)],
        "REACH GUARD: a legal creature target must survive re-validation"
    );

    // The victim stays on the battlefield but stops being a creature — the
    // exact case the old battlefield-presence-only check let through.
    state
        .objects
        .get_mut(&victim)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Artifact];

    let validated = validate_targets_in_chain(&state, &ability);
    assert!(
        validated.targets.is_empty(),
        "CR 608.2b: a target that no longer matches its filter is illegal, so this ability's \
         only target is illegal"
    );
    // ...and carry the claim the test's name makes all the way to the
    // disposition rather than stopping one inference short of it. CR 608.2b:
    // all targets illegal ⇒ the ability doesn't resolve, so the "otherwise
    // sacrifice this creature" rider never runs. Asserting it here also fails
    // loudly if `check_fizzle`'s contract changes underneath this row.
    assert!(
        engine::game::targeting::check_fizzle(&[TargetRef::Object(victim)], &validated.targets),
        "CR 608.2b: with its only target illegal the ability does not resolve"
    );
}

/// REGRESSION (final review) — an `ExchangeControl` node must never bind an
/// object that was not one of its own two claimed targets.
///
/// `validate_targets_in_chain`'s `ExchangeControl` arm prunes illegal targets,
/// which shifts survivors toward index 0. `exchange_control::resolve` consumes
/// `ability.targets` positionally with no per-slot recheck, so ANY entry left
/// in the list is bindable into one of the two exchange slots. An earlier
/// revision of this arm appended unclaimed propagated entries after the
/// survivors (mirroring the `Attach` arm); with `[A_illegal, B_legal,
/// C_propagated]` that produced `[B, C]` and the resolver exchanged control of
/// B and C — a pair the spell never targeted together.
///
/// Correct outcome: only `B` survives, the second `resolve_slot` runs dry, and
/// CR 701.12a's all-or-nothing rule makes the whole thing a no-op.
///
/// REVERT-FAILING: re-adding `kept.extend(target_iter.cloned())` to that arm
/// makes this row exchange B and C and fail on the emptiness assertion.
#[test]
fn exchange_control_ignores_unclaimed_propagated_targets() {
    use engine::game::ability_utils::validate_targets_in_chain;
    use engine::game::effects::exchange_control;
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef, TypedFilter};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;

    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        P0,
        "Source".into(),
        Zone::Battlefield,
    );
    let illegal = create_object(
        &mut state,
        CardId(2),
        P0,
        "Illegal A".into(),
        Zone::Battlefield,
    );
    let legal = create_object(
        &mut state,
        CardId(3),
        P1,
        "Legal B".into(),
        Zone::Battlefield,
    );
    let propagated = create_object(
        &mut state,
        CardId(4),
        P0,
        "Propagated C".into(),
        Zone::Battlefield,
    );

    // A is on the battlefield but is NOT a creature, so it fails the declared
    // filter at CR 608.2b re-validation. B and C both satisfy it.
    state
        .objects
        .get_mut(&illegal)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Artifact];
    for id in [legal, propagated] {
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
    }

    let ability = ResolvedAbility::new(
        Effect::ExchangeControl {
            target_a: TargetFilter::Typed(TypedFilter::creature()),
            target_b: TargetFilter::Typed(TypedFilter::creature()),
        },
        vec![
            TargetRef::Object(illegal),
            TargetRef::Object(legal),
            TargetRef::Object(propagated),
        ],
        source,
        P0,
    );

    let validated = validate_targets_in_chain(&state, &ability);
    // REACH GUARD: the illegal target really was dropped, and the unclaimed
    // third entry really was not carried forward — without this the row could
    // pass for the wrong reason (e.g. nothing was pruned at all).
    assert_eq!(
        validated.targets,
        vec![TargetRef::Object(legal)],
        "only the surviving claimed target is kept; the unclaimed third entry is not appended"
    );

    let mut events = Vec::new();
    exchange_control::resolve(&mut state, &validated, &mut events).unwrap();
    assert!(
        state.transient_continuous_effects.is_empty(),
        "CR 701.12a: with only one subject bindable the exchange can't complete, so no part of \
         it occurs — and in particular C, which was never a target of this effect, is untouched"
    );
}

/// V18 SIBLING (final review) — Perplexing Chimera's SECOND clause. After the
/// exchange, "you may choose new targets for the spell" must enumerate the
/// replacement pool against the spell's NEW controller.
///
/// The card's printed ruling is explicit: "The change of control happens before
/// new targets are chosen, so any targeting restrictions such as 'target
/// opponent' or 'target creature you control' are now made in reference to you,
/// not the spell's original controller."
///
/// This was wrong until the `pool_controller` binding in
/// `change_targets::legal_new_targets_for_entry`: the exchange installs a
/// layer-2 `ChangeController` on the OBJECT, while `ResolvedAbility.controller`
/// stays the caster until `stack::resolve_top` re-stamps it — which happens
/// after the retarget window has already closed. The pool was therefore built
/// for P1 while the chooser was P0.
///
/// MEASURED before the fix: `legal_new_targets == [chimera, p1_creature]` — the
/// creatures P1 controls, with P0's own creature absent, so P0 could not make
/// the one choice the ruling entitles them to.
///
/// The other Chimera rows cannot catch this: `perplexing_chimera_steals_the_
/// spell_end_to_end` deliberately uses vanilla Grizzly Bears so the retarget
/// offer is a guaranteed no-op, and `chimera_retarget_subject_binds_to_the_
/// triggering_spell` uses a filter with no `ControllerRef`. A controller-
/// relative filter is required to distinguish the two controllers at all.
#[test]
fn chimera_retarget_pool_is_built_for_the_new_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let p0_creature = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_creature = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let guile = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Ranger's Guile",
            true,
            "Target creature you control gets +1/+1 until end of turn.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut commit = runner
        .cast(guile)
        .target_objects(&[p1_creature])
        .accept_optional()
        .commit();

    // Drain to the retarget prompt, accepting the Chimera trigger on the way.
    let mut reached = None;
    for _ in 0..40 {
        match &commit.state().waiting_for {
            WaitingFor::RetargetChoice {
                player,
                legal_new_targets,
                ..
            } => {
                reached = Some((*player, legal_new_targets.clone()));
                break;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                commit
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the Chimera trigger must succeed");
            }
            WaitingFor::Priority { .. } => {
                assert!(
                    !commit.state().stack.is_empty(),
                    "the stack emptied before the retarget prompt was raised"
                );
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining");
            }
            other => panic!("unexpected state while draining to the retarget prompt: {other:?}"),
        }
    }
    let (chooser, pool) = reached.expect("REACH GUARD: the retarget prompt must be raised");

    // REACH GUARD: the exchange really happened, so this row is measuring the
    // post-steal pool and not a pre-steal one.
    assert_eq!(
        commit.state().objects.get(&chimera).unwrap().controller,
        P1,
        "REACH GUARD: the Chimera must have swapped to P1"
    );
    assert_eq!(chooser, P0, "the new controller chooses the new targets");

    assert!(
        pool.contains(&TargetRef::Object(p0_creature)),
        "\"target creature you control\" must now mean P0's creatures — P0's own creature \
         must be offered (pool was {pool:?})"
    );
    assert!(
        !pool.contains(&TargetRef::Object(p1_creature)),
        "P1's creature must NOT be offered — the restriction is read against P0 now \
         (pool was {pool:?})"
    );
    assert!(
        !pool.contains(&TargetRef::Object(chimera)),
        "the Chimera is P1's after the swap, so it must not be in P0's pool \
         (pool was {pool:?})"
    );
}

// ---------------------------------------------------------------------------
// V5 — Gilded Drake's "if you don't or can't make an exchange" rider
// ---------------------------------------------------------------------------

/// Stage Gilded Drake's ETB trigger onto the stack with `bear` chosen as its
/// declared target, and hand priority to P1 so a response can be cast.
///
/// Shared by V5 and its positive control so the two rows differ in exactly one
/// thing — whether the response is cast — and nothing else.
fn stage_gilded_drake_trigger(
    runner: &mut engine::game::scenario::GameRunner,
    drake: engine::types::identifiers::ObjectId,
) -> CastCommit<'_> {
    let mut commit = runner.cast(drake).commit();
    let mut staged = false;
    for _ in 0..40 {
        let on_battlefield =
            commit.state().objects.get(&drake).map(|obj| obj.zone) == Some(Zone::Battlefield);
        if on_battlefield && commit.state().stack.len() == 1 {
            staged = true;
            break;
        }
        match commit.state().waiting_for {
            WaitingFor::Priority { .. } => {
                assert!(
                    !commit.state().stack.is_empty(),
                    "the stack emptied before the Drake's ETB trigger could be staged"
                );
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while staging the trigger");
            }
            ref other => panic!("unexpected waiting state while staging the trigger: {other:?}"),
        }
    }
    // REACH GUARD: the Drake really is on the battlefield and its ETB trigger
    // really is on the stack, unresolved — the only window in which a response
    // can change who controls the Drake before the exchange resolves.
    //
    // NOTE the trigger raises no `TriggerTargetSelection` here: "up to one
    // target creature an opponent controls" has exactly one legal choice on
    // this board, so the engine binds it without prompting. The two rows below
    // assert on the bound target's disposition instead.
    assert!(
        staged,
        "REACH GUARD: the Drake must be on the battlefield with its ETB trigger on the stack"
    );
    {
        let state = commit.state_mut();
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    commit
}

fn sacrifice_resolutions(
    outcome: &engine::game::scenario::CastOutcome,
    source: engine::types::identifiers::ObjectId,
) -> usize {
    use engine::types::ability::EffectKind;
    use engine::types::events::GameEvent;
    outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Sacrifice,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

/// V5 — CR 701.12b + CR 608.2c: "If you don't or can't make an exchange,
/// sacrifice this creature."
///
/// P0 casts Gilded Drake targeting P1's bear. **In response to the ETB
/// trigger**, P1 casts "Gain control of target creature until end of turn."
/// on the Drake itself. By the time the trigger resolves, P1 controls BOTH
/// subjects, so CR 701.12b makes the exchange do nothing — and the printed
/// rider must fire.
///
/// The declared target survives CR 608.2b re-validation because the ability's
/// controller is still P0: **CR 603.3a** — "a triggered ability is controlled
/// by the player who controlled its source at the time it triggered" — so a
/// control change of the SOURCE does not re-seat it, and "target creature an
/// opponent controls" is still read against P0. Slot A is `SelfRef`, whose
/// currency check (CR 400.7) is zone/incarnation-based, not controller-based,
/// so P1 gaining control of the Drake does not unbind it either.
///
/// **THE REVERT-FAILING ASSERTION** is the `EffectResolved { Sacrifice }` in
/// the event trail. It cannot be a board delta: the sacrifice itself is a
/// legitimate no-op here (CR 701.21a — the Drake's controller is P1, not the
/// ability's controller P0, so `sacrifice::resolve`'s controller guard skips
/// it), which makes the board BYTE-IDENTICAL pre- and post-fix. Pre-fix
/// `mandatory_parent_effect_performed` fell into `_ => true`, the
/// `Not(IfYouDo)` gate read false, and the sub never ran at all.
///
/// NEGATIVE CO-ASSERTION — no `PermanentSacrificed` at all. Once this sub
/// actually runs, the walker propagates the parent's declared target (the
/// BEAR) into the `Sacrifice { target: SelfRef }` sub, and only the CR 701.21a
/// controller guard stops the Drake's rider from eating P1's bear. If that
/// guard is ever weakened this row fails loudly instead of silently
/// sacrificing the wrong permanent.
#[test]
fn gilded_drake_sacrifice_rider_fires_when_the_exchange_does_nothing() {
    use engine::types::ability::ContinuousModification;
    use engine::types::events::GameEvent;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let drake = scenario
        .add_creature_to_hand_from_oracle(P0, "Gilded Drake", 3, 3, GILDED_DRAKE_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Seize the Drake", true, GAIN_CONTROL_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let mut commit = stage_gilded_drake_trigger(&mut runner, drake);
    let outcome = commit
        .cast(steal)
        .target_objects(&[drake])
        .decline_optional()
        .resolve();

    // REACH GUARD: the response actually landed, so the CR 701.12b collision
    // this row depends on really was staged.
    assert_eq!(
        outcome.state().objects.get(&drake).unwrap().controller,
        P1,
        "REACH GUARD: P1 must control the Drake when the exchange resolves"
    );
    assert_eq!(
        outcome.state().objects.get(&bear).unwrap().controller,
        P1,
        "REACH GUARD: the bear stays P1's, so both subjects share a controller \
         (CR 701.12b) and the exchange does nothing"
    );

    // THE DISCRIMINATOR.
    assert_eq!(
        sacrifice_resolutions(&outcome, drake),
        1,
        "CR 608.2c: the \"if you don't or can't make an exchange\" rider must RESOLVE \
         exactly once (events were {:?})",
        outcome.events()
    );

    // NEGATIVE CO-ASSERTION.
    assert!(
        !outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::PermanentSacrificed { .. })),
        "CR 701.21a: nothing may actually be sacrificed — the Drake is P1's, and the bear \
         was never this rider's subject (events were {:?})",
        outcome.events()
    );

    // The exchange installed no Layer-2 control effect of its own; the only
    // transient effect present is the gain-control spell's.
    let drake_sourced: Vec<_> = outcome
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect.source_id == drake
                && effect
                    .modifications
                    .contains(&ContinuousModification::ChangeController)
        })
        .collect();
    assert!(
        drake_sourced.is_empty(),
        "CR 701.12a/b: a no-op exchange installs no control effect of its own"
    );
}

/// V5's PAIRED POSITIVE CONTROL — the same scenario with P1 declining to
/// respond. The exchange genuinely happens, so the `Not(IfYouDo)` rider must
/// NOT fire. Without this row, a fix that over-suppressed (or a fixture that
/// never reached the trigger at all) would pass V5 silently.
#[test]
fn gilded_drake_sacrifice_rider_stays_silent_when_the_exchange_happens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let drake = scenario
        .add_creature_to_hand_from_oracle(P0, "Gilded Drake", 3, 3, GILDED_DRAKE_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let commit = stage_gilded_drake_trigger(&mut runner, drake);
    let outcome = commit.resolve();

    // CR 701.12b: different controllers, so the exchange really happens.
    assert_eq!(
        outcome.state().objects.get(&drake).unwrap().controller,
        P1,
        "the Drake goes to the opponent"
    );
    assert_eq!(
        outcome.state().objects.get(&bear).unwrap().controller,
        P0,
        "and their creature comes back"
    );
    assert_eq!(
        sacrifice_resolutions(&outcome, drake),
        0,
        "CR 608.2c: an exchange that HAPPENED must not fire the \"if you don't or can't\" \
         rider (events were {:?})",
        outcome.events()
    );
}

// ---------------------------------------------------------------------------
// V8 — blast radius of the new `ControllerChanged` event
// ---------------------------------------------------------------------------

/// Verbatim from `client/public/card-data.json`. The middle clause parses to a
/// `TriggerMode::ChangesController` trigger with `valid_card: SelfRef` — one of
/// only four printed producers of that mode, and the reason this row exists.
const KHARN_TEXT: &str = "Berzerker — Khârn the Betrayer attacks or blocks each combat if \
    able.\nSigil of Corruption — When you lose control of Khârn the Betrayer, draw two \
    cards.\nThe Betrayer — If damage would be dealt to Khârn the Betrayer, prevent that damage \
    and an opponent of your choice gains control of it.";

/// Verbatim from `client/public/card-data.json`.
const SWITCHEROO_TEXT: &str = "Exchange control of two target creatures.";

/// V8 POSITIVE — CR 603.2 + CR 613.1b: now that `exchange_control::resolve`
/// publishes `ControllerChanged`, a "When you lose control of ~" trigger fires
/// on an exchange, exactly once — and, per PR #8332 round 1 (U3), for the
/// correct player.
///
/// Production entry chain: `exchange_control::resolve` → `collect_pending_triggers`
/// → `trigger_index.rs`'s `ControllerChanged{..} => TriggerEventKey::ChangesController`
/// (the gate that makes the matcher reachable at all) → `match_changes_controller`
/// → `collect_matching_triggers_inner`'s CR 603.10d + CR 603.3a controller
/// derivation (`triggers.rs`).
///
/// REVERT-FAILING (two independent legs): without the `ControllerChanged`
/// emission the event never exists, no `ChangesController` key is ever
/// pushed, and nobody draws (0/0). Without U3's controller derivation, the
/// trigger still fires exactly once but for the WRONG player — the gainer
/// (P1) instead of the loser (P0) — so a reversed-recipient assertion is
/// needed to catch that leg; a summed total cannot.
#[test]
fn exchanging_control_fires_a_lose_control_trigger_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);
    let kharn = scenario
        .add_creature_from_oracle(P0, "Khârn the Betrayer", 4, 4, KHARN_TEXT)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let switcheroo = scenario
        .add_spell_to_hand_from_oracle(P0, "Switcheroo", false, SWITCHEROO_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let outcome = runner
        .cast(switcheroo)
        .target_objects(&[kharn, bear])
        .resolve();

    // REACH GUARD: the exchange really happened (CR 701.12b needed two
    // different controllers, which this board supplies).
    assert_eq!(
        outcome.state().objects.get(&kharn).unwrap().controller,
        P1,
        "REACH GUARD: Khârn must have changed hands"
    );
    assert_eq!(
        outcome.state().objects.get(&bear).unwrap().controller,
        P0,
        "REACH GUARD: and the bear must have come the other way"
    );

    // THE DISCRIMINATOR: exactly one lose-control trigger resolved, controlled
    // by the player who LOST control of Khârn (P0, CR 603.10d + CR 603.3a) —
    // not the gainer (P1), and not a double-fire (which would read 4/0 or 2/2
    // depending on attribution).
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_hand_drawn(P1, 0);
}

/// V8 NEGATIVE — the Portent trap. A `ChangesController` trigger is scoped to
/// its OWN tracked object by `valid_card_matches`, so a bystander carrying the
/// same trigger must NOT fire when two unrelated objects exchange control.
///
/// This row also covers the STACK HALF: Perplexing Chimera's exchange publishes
/// a `ControllerChanged` whose `object_id` is a SPELL (CR 109.4 — objects on the
/// stack have a controller). That event is a legitimate verdict signal and must
/// stay inert to triggers; no printed `ChangesController` trigger has
/// `valid_card: None`, so none can match it.
#[test]
fn an_unrelated_lose_control_trigger_does_not_fire_on_someone_elses_exchange() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    // The BYSTANDER: it carries the ChangesController trigger, it is on the
    // battlefield throughout, and its controller never changes.
    let bystander = scenario
        .add_creature_from_oracle(P0, "Khârn the Betrayer", 4, 4, KHARN_TEXT)
        .id();
    let grizzly_bears = scenario
        .add_creature_to_hand_from_oracle(P1, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(grizzly_bears).accept_optional().resolve();

    // REACH GUARD: the exchange really happened — the Chimera swapped and the
    // spell resolved for its new controller. Without this the row could pass
    // because nothing exchanged at all.
    assert_eq!(
        outcome.state().objects.get(&chimera).unwrap().controller,
        P1,
        "REACH GUARD: the Chimera must have swapped to P1"
    );
    assert_eq!(
        outcome
            .state()
            .objects
            .get(&grizzly_bears)
            .unwrap()
            .controller,
        P0,
        "REACH GUARD: CR 400.7a — the exchanged spell's permanent enters under P0's control"
    );
    assert_eq!(
        outcome.state().objects.get(&bystander).unwrap().controller,
        P0,
        "REACH GUARD: the bystander's controller never changed"
    );

    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "the bystander's \"When you lose control of ~\" must not fire for an exchange it \
         was not part of, on the battlefield half OR the stack half"
    );
    assert_eq!(outcome.hand_drawn(P1), 0);
}

// ---------------------------------------------------------------------------
// V6c — Arteeoh's reflexive "When you do", end to end
// ---------------------------------------------------------------------------

/// Verbatim from `client/public/card-data.json`.
const ARTEEOH_TEXT: &str = "Flying, deathtouch\nWhenever Arteeoh deals combat damage to a \
    player, you may exchange control of two other target artifacts. When you do, create a token \
    that's a copy of target artifact you don't control, except it's a 1/1 green Squirrel \
    creature token in addition to its other colors and types.";

/// V3b (round-6 plan, per-node target ownership) — stage Arteeoh's trigger
/// through REAL COMBAT DAMAGE, submit its two declared exchange slots through
/// the production BULK `GameAction::SelectTargets` seam
/// (`engine_stack.rs::handle_trigger_target_selection_select_targets`, which
/// reaches `assign_targets_in_chain`), and accept the "you may" through the
/// real action pipeline.
///
/// This is exactly what makes the upgrade buildable: per-node target
/// ownership (§5.5/§5.6) is what lets `assign_targets_in_chain` accept BOTH
/// declared slots for a two-declared-slot `ExchangeControl` node instead of
/// rejecting the submission with `InvalidAction("Unused selected targets")`
/// after consuming only one of them (BASE, MEASURED both same-controller and
/// cross-controller).
///
/// Everything downstream — the trigger prompt, the reflexive trigger, its
/// target prompt, and the token — flows through the production `WaitingFor` /
/// `GameAction` path, which is what this row measures.
fn accept_arteeoh_exchange(
    runner: &mut engine::game::scenario::GameRunner,
    arteeoh: engine::types::identifiers::ObjectId,
    slot_a: engine::types::identifiers::ObjectId,
    slot_b: engine::types::identifiers::ObjectId,
) {
    use engine::game::combat::AttackTarget;
    use engine::types::ability::EffectKind;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(arteeoh, AttackTarget::Player(P1))])
        .expect("Arteeoh attacks");
    let _ = runner.combat_damage(); // parks on TriggerTargetSelection; does NOT drain it

    // REACH GUARD 1 (the production trigger seam):
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            source_id,
            target_slots,
            ..
        } => {
            assert_eq!(*source_id, Some(arteeoh));
            assert_eq!(target_slots.len(), 2);
            assert!(target_slots
                .iter()
                .all(|s| s.effect_kind == EffectKind::ExchangeControl));
        }
        other => panic!("expected Arteeoh's TriggerTargetSelection, got {other:?}"),
    }
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(slot_a), TargetRef::Object(slot_b)],
        })
        .expect("both declared exchange slots accept their targets"); // <- BASE Errs HERE
                                                                      // two PassPriority put the trigger through resolution to its "you may" offer
    runner
        .act(GameAction::PassPriority)
        .expect("priority passes");
    runner
        .act(GameAction::PassPriority)
        .expect("priority passes");

    // REACH GUARD 2: the chain really parked on Arteeoh's own "you may" offer.
    match runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { source_id, .. } => assert_eq!(
            source_id, arteeoh,
            "REACH GUARD: the offer must be Arteeoh's own"
        ),
        ref other => panic!("expected Arteeoh's OptionalEffectChoice, got {other:?}"),
    }

    assert!(
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .is_ok(),
        "accepting the exchange must be accepted by the reducer"
    );
}

fn squirrel_tokens(runner: &engine::game::scenario::GameRunner) -> Vec<String> {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].is_token)
        .map(|id| runner.state().objects[id].name.clone())
        .collect()
}

/// V6c — CR 701.12b + CR 603.12: Arteeoh's reflexive "When you do, create a
/// token …" must NOT fire when the accepted exchange exchanged nothing.
///
/// Both declared artifacts are P0's, so CR 701.12b makes the exchange a no-op
/// even though the controller accepted the offer. Suppression happens at
/// `resolve_ability_chain`'s `if !condition_met` early exit, which is strictly
/// BEFORE `try_materialize_reflexive_trigger` — a suppressed `WhenYouDo` sub
/// can never materialise a reflexive trigger at all.
///
/// This consumer's path is DISJOINT from the `IfYouDo` one: `evaluate_condition`'s
/// `WhenYouDo` arm reads `ability.optional && !performed`, and the accept has
/// already lowered `optional`, so that arm returns true regardless.
/// `when_you_do_mandatory_parent_did_nothing` is the only thing that can
/// suppress it, and all four of its conjuncts must hold — two of which this
/// change supplies (the resolver-verdict block lowers the latched flag; the new
/// `mandatory_parent_effect_performed` arm answers no).
///
/// REVERT-FAILING (both measured PRESENT pre-fix): the reflexive
/// `TriggerTargetSelection` carrying a `CopyTokenOf` slot, and the token itself.
#[test]
fn arteeoh_reflexive_token_does_not_fire_when_the_exchange_did_nothing() {
    use engine::types::ability::EffectKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    // BOTH exchange subjects under P0. Legal: both slots are
    // `Typed(Artifact, Another)` with no controller restriction.
    let a1 = scenario.add_artifact_from_oracle(P0, "Bauble A", "").id();
    let a2 = scenario.add_artifact_from_oracle(P0, "Bauble B", "").id();
    // The reflexive body's own target — "target artifact you don't control".
    let foreign = scenario
        .add_artifact_from_oracle(P1, "Foreign Relic", "")
        .id();

    let mut runner = scenario.build();
    accept_arteeoh_exchange(&mut runner, arteeoh, a1, a2);

    // SECOND REACH GUARD: the exchange genuinely did nothing (CR 701.12b). A
    // row where the exchange SUCCEEDED would prove nothing about the gate.
    assert_eq!(runner.state().objects[&a1].controller, P0);
    assert_eq!(runner.state().objects[&a2].controller, P0);

    // THE DISCRIMINATOR (1): no reflexive trigger was materialised.
    if let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for {
        assert!(
            !target_slots
                .iter()
                .any(|slot| slot.effect_kind == EffectKind::CopyTokenOf),
            "the reflexive \"When you do\" must not raise its target prompt for an exchange \
             that exchanged nothing (slots were {target_slots:?})"
        );
    }

    // THE DISCRIMINATOR (2): and no token exists, at the prompt boundary or
    // after draining whatever else is pending.
    runner.advance_until_stack_empty();
    assert!(
        squirrel_tokens(&runner).is_empty(),
        "no token may be created for an exchange that exchanged nothing (tokens were {:?})",
        squirrel_tokens(&runner)
    );
    // The reflexive body's would-be target is untouched and still P1's.
    assert_eq!(runner.state().objects[&foreign].controller, P1);
}

/// V6c's PAIRED POSITIVE REACH GUARD (mandatory — it is what makes the two
/// negatives above non-vacuous). The same staging with a CROSS-controller pair,
/// so CR 701.12b does not no-op: the reflexive trigger must still be raised and
/// the token must still be created.
#[test]
fn arteeoh_reflexive_token_still_fires_when_the_exchange_happens() {
    use engine::types::ability::EffectKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    let a1 = scenario.add_artifact_from_oracle(P0, "Bauble A", "").id();
    let foreign = scenario
        .add_artifact_from_oracle(P1, "Foreign Relic", "")
        .id();

    let mut runner = scenario.build();
    accept_arteeoh_exchange(&mut runner, arteeoh, a1, foreign);

    // CR 701.12b: different controllers, so the exchange really happens.
    assert_eq!(
        runner.state().objects[&a1].controller,
        P1,
        "REACH GUARD: a real exchange must move control"
    );

    // The reflexive trigger IS materialised, with its own `CopyTokenOf` slot.
    let slot_reached = match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots
            .iter()
            .any(|slot| slot.effect_kind == EffectKind::CopyTokenOf),
        _ => false,
    };
    assert!(
        slot_reached,
        "REACH GUARD: a completed exchange must raise the reflexive \"When you do\" target \
         prompt (state was {:?})",
        runner.state().waiting_for
    );

    // "target artifact you don't control" is read AFTER the exchange, so the
    // artifact P0 no longer controls is `a1` — the one it just handed over.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(a1)],
        })
        .expect("the reflexive body's slot accepts the artifact P0 no longer controls");
    runner.advance_until_stack_empty();

    assert_eq!(
        squirrel_tokens(&runner),
        vec!["Bauble A".to_string()],
        "a completed exchange must still create the copy token"
    );
    // The artifact P0 gained stays P0's — this row is not measuring a revert.
    assert_eq!(runner.state().objects[&foreign].controller, P0);
}

// ---------------------------------------------------------------------------
// Round-6 plan — per-node target ownership for paired-subject effects
// (Effect::ExchangeControl / Effect::ExchangeLifeTotals). Rows V1, V2
// (MANDATORY — the maintainer's Shifting Grift two-mode blocker fixture),
// V3a, V3c, V3d, V8, V9, V11, V18.
// ---------------------------------------------------------------------------

/// V1 — a 2-mode Shifting Grift exchanges BOTH pairs, each against its own
/// declared targets — not the head mode's pair twice (the BASE bug: M6
/// measured 4 `ChangeController` transients all on `{c1,c2}`, with `a1`/`a2`
/// never touched, because the whole-chain collect pass returned before
/// descending into the artifact mode).
///
/// REACH GUARD (paired positive, MANDATORY): the finalized stack ability
/// holds EXACTLY the creature pair on the root node and EXACTLY the artifact
/// pair on its sub-ability — the direct, structural proof of per-node
/// ownership, not just an end-state coincidence.
///
/// REVERT-FAILING: at BASE (debug) `SelectModes{[0,1]}` panics inside
/// `build_target_slots_labelled`'s `debug_assert_eq!` (measured, M5); at BASE
/// (release) all four transients land on `{c1,c2}` and `a1`/`a2` never move
/// (measured, M6), so the artifact-pair assertions below fail.
#[test]
fn shifting_grift_two_modes_exchange_their_own_pairs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana_pool());
    let c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let a1 = scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let a2 = scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let commit = runner
        .cast(grift)
        .modes(&[0, 1])
        .target_objects(&[c1, c2, a1, a2])
        .commit();

    // REACH GUARD: per-node ownership at announcement, structurally — the
    // root (creature mode) claims ONLY [c1, c2]; the sub-ability (artifact
    // mode) claims ONLY [a1, a2]. At BASE (release) the root would instead
    // hold all four flat targets and there would be no sub-ability slot.
    {
        use engine::types::game_state::StackEntryKind;
        let StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } = &commit.state().stack.back().unwrap().kind
        else {
            panic!("Shifting Grift must finalize its ability at commit");
        };
        assert_eq!(
            ability.targets,
            vec![TargetRef::Object(c1), TargetRef::Object(c2)],
            "REACH GUARD: the creature mode (root) must claim exactly its OWN pair"
        );
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("REACH GUARD: the artifact mode must be chained as a sub-ability");
        assert_eq!(
            sub.targets,
            vec![TargetRef::Object(a1), TargetRef::Object(a2)],
            "REACH GUARD: the artifact mode (sub node) must claim exactly its OWN pair"
        );
    }

    let outcome = commit.resolve();

    assert_eq!(
        outcome.state().objects[&c1].controller,
        P1,
        "creature pair must exchange"
    );
    assert_eq!(
        outcome.state().objects[&c2].controller,
        P0,
        "creature pair must exchange"
    );
    assert_eq!(
        outcome.state().objects[&a1].controller,
        P1,
        "artifact pair must exchange against ITS OWN targets, not be left untouched"
    );
    assert_eq!(
        outcome.state().objects[&a2].controller,
        P0,
        "artifact pair must exchange against ITS OWN targets, not be left untouched"
    );
}

/// V1's SIBLING: a 1-mode Shifting Grift cast (mode 0 only) still exchanges
/// exactly the creature pair — proves the paired arm's descent is not
/// accidentally required for the single-node case to keep working.
#[test]
fn shifting_grift_single_mode_still_exchanges_exactly_its_pair() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana_pool());
    let c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(grift)
        .modes(&[0])
        .target_objects(&[c1, c2])
        .resolve();

    assert_eq!(outcome.state().objects[&c1].controller, P1);
    assert_eq!(outcome.state().objects[&c2].controller, P0);
}

/// V2 (U1's DISCRIMINATING TEST — the maintainer's mandatory blocker
/// fixture). A 2-mode Shifting Grift where mode 1's (creature) targets go
/// illegal in response, and mode 2's (artifact) targets stay legal: the spell
/// must RESOLVE — not fizzle — with the artifact exchange happening and the
/// creature exchange skipped (CR 608.2b: only the illegal instance is
/// unaffected; the spell as a whole resolves because a legal target
/// remains).
///
/// REVERT-FAILING: at BASE this exact shape either panics (debug,
/// `build_target_slots_labelled`'s `debug_assert_eq!`) or, once ALL FOUR
/// targets are flattened onto ONE node (M6), a partial illegal-target set
/// mis-evaluates CR 608.2b across the wrong node boundary — this is precisely
/// the maintainer's HIGH blocker: "with two or more modes chosen, when mode
/// 1's targets go illegal and mode 2's stay legal, the spell now fizzles
/// instead of resolving."
#[test]
fn shifting_grift_second_mode_resolves_when_the_first_modes_targets_go_illegal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana_pool());
    let c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let a1 = scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let a2 = scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let mut commit = runner
        .cast(grift)
        .modes(&[0, 1])
        .target_objects(&[c1, c2, a1, a2])
        .commit();

    // REACH GUARD: the spell is really on the stack with both modes' targets
    // declared before we strip anything.
    assert_eq!(
        commit.state().stack.len(),
        1,
        "REACH GUARD: Shifting Grift must be on the stack with all four targets"
    );

    // "In response", BOTH creature targets stop being creatures — mode 1's
    // node loses every legal target. Mode 2's artifact targets are untouched.
    {
        let state = commit.state_mut();
        for id in [c1, c2] {
            state
                .objects
                .get_mut(&id)
                .expect("creature target exists")
                .card_types
                .core_types = vec![CoreType::Artifact];
        }
    }

    let outcome = commit.resolve();

    // (a) a1/a2 controllers exchanged.
    assert_eq!(
        outcome.state().objects[&a1].controller,
        P1,
        "the artifact mode's own targets are unaffected by the OTHER mode's illegal targets"
    );
    assert_eq!(outcome.state().objects[&a2].controller, P0);
    // (b) c1/c2 controllers UNCHANGED — the illegal instance is simply
    // unaffected (CR 608.2b), not swapped anyway.
    assert_eq!(
        outcome.state().objects[&c1].controller,
        P0,
        "an illegal target must not be affected by the part of the effect for which it's illegal"
    );
    assert_eq!(outcome.state().objects[&c2].controller, P1);
    // (c) Shifting Grift is in its owner's graveyard WITH the artifact
    // exchange having happened — not countered/fizzled.
    assert_eq!(
        outcome.zone_of(grift),
        Zone::Graveyard,
        "the spell must RESOLVE (not fizzle) because one node still has a legal target"
    );

    // HOSTILE (the CR 608.2b "all its targets are now illegal" half): if
    // instead every one of the four targets goes illegal, the spell truly
    // fizzles and NO exchange happens at all.
    let mut hostile_scenario = GameScenario::new();
    hostile_scenario.at_phase(Phase::PreCombatMain);
    hostile_scenario.with_mana_pool(P0, generic_mana_pool());
    let hc1 = hostile_scenario.add_creature(P0, "Creature HC1", 2, 2).id();
    let hc2 = hostile_scenario.add_creature(P1, "Creature HC2", 2, 2).id();
    let ha1 = hostile_scenario
        .add_artifact_from_oracle(P0, "Artifact HA1", "")
        .id();
    let ha2 = hostile_scenario
        .add_artifact_from_oracle(P1, "Artifact HA2", "")
        .id();
    let hostile_grift = hostile_scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut hostile_runner = hostile_scenario.build();
    let mut hostile_commit = hostile_runner
        .cast(hostile_grift)
        .modes(&[0, 1])
        .target_objects(&[hc1, hc2, ha1, ha2])
        .commit();
    {
        let state = hostile_commit.state_mut();
        for id in [hc1, hc2] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Artifact];
        }
        for id in [ha1, ha2] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }
    }
    let hostile_outcome = hostile_commit.resolve();
    assert_eq!(
        hostile_outcome.state().objects[&hc1].controller,
        P0,
        "HOSTILE: with every target illegal for every node, the whole spell fizzles"
    );
    assert_eq!(hostile_outcome.state().objects[&hc2].controller, P1);
    assert_eq!(hostile_outcome.state().objects[&ha1].controller, P0);
    assert_eq!(hostile_outcome.state().objects[&ha2].controller, P1);
    assert_eq!(hostile_outcome.zone_of(hostile_grift), Zone::Graveyard);
}

/// V3a — a two-declared-slot `ExchangeControl` node can have targets ASSIGNED
/// AT ALL, directly against the `pub` seam `assign_targets_in_chain`, on
/// Arteeoh's parsed trigger.
///
/// REVERT-FAILING: at BASE, `assign_targets_in_chain` consumes only ONE of
/// the two declared slots and rejects the rest with
/// `Err(InvalidAction("Unused selected targets"))` — measured for both the
/// same-controller and cross-controller submission — because
/// `chain_has_target_sink` answers `false` for a chain whose only sink is a
/// paired-subject node.
#[test]
fn arteeoh_two_declared_exchange_slots_assign_to_their_own_node() {
    use engine::game::ability_utils::{
        assign_targets_in_chain, build_resolved_from_def, build_target_slots,
    };
    use engine::parser::oracle::parse_oracle_text;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    let a1 = scenario.add_artifact_from_oracle(P0, "Bauble A", "").id();
    let a2 = scenario.add_artifact_from_oracle(P1, "Bauble B", "").id();

    let runner = scenario.build();
    let parsed = parse_oracle_text(ARTEEOH_TEXT, "Arteeoh, Dread Scavenger", &[], &[], &[]);
    let def = *parsed
        .triggers
        .first()
        .expect("Arteeoh has a combat-damage trigger")
        .execute
        .clone()
        .expect("that trigger has an execute");
    let mut resolved = build_resolved_from_def(&def, arteeoh, P0);

    // REACH GUARD: exactly 2 slots surfaced.
    let slots =
        build_target_slots(runner.state(), &resolved).expect("Arteeoh's two slots must build");
    assert_eq!(
        slots.len(),
        2,
        "REACH GUARD: Arteeoh's trigger must surface exactly 2 slots"
    );

    let result = assign_targets_in_chain(
        runner.state(),
        &mut resolved,
        &[TargetRef::Object(a1), TargetRef::Object(a2)],
    );
    assert!(
        result.is_ok(),
        "both declared exchange slots must be accepted, got {result:?}"
    );
    assert_eq!(
        resolved.targets,
        vec![TargetRef::Object(a1), TargetRef::Object(a2)]
    );
    assert!(
        resolved
            .sub_ability
            .as_ref()
            .is_none_or(|sub| sub.targets.is_empty()),
        "the CopyTokenOf sub must claim no targets at declaration time — its target is chosen \
         at resolution (defers_conditional_target_selection on \"When you do\")"
    );

    // HOSTILE: submitting only ONE target must be rejected.
    let mut resolved_short = build_resolved_from_def(&def, arteeoh, P0);
    let short_result = assign_targets_in_chain(
        runner.state(),
        &mut resolved_short,
        &[TargetRef::Object(a1)],
    );
    assert!(
        short_result.is_err(),
        "a short submission must be rejected (Missing required target), got {short_result:?}"
    );
}

/// V3c (BLOCKER FIX). §5.7 IS EXERCISED: Arteeoh's SAME production trigger
/// prompt walked ONE SLOT AT A TIME with `GameAction::ChooseTarget`, hitting
/// the trigger `ChooseTarget` walk → `assign_selected_slots_in_chain`. This
/// is the ONLY path the AI takes through a target prompt
/// (`ai_support::candidates::target_step_actions` emits ONLY `ChooseTarget`).
///
/// REVERT-FAILING (MEASURED both sides): at BASE the SECOND `ChooseTarget`
/// returns `Err(InvalidAction("Unused selected target slots"))` —
/// `assign_selected_slots_in_chain`'s own message, not
/// `assign_targets_in_chain`'s — and the state stays parked on
/// `TriggerTargetSelection`.
#[test]
fn arteeoh_exchange_slots_are_walked_one_at_a_time_by_choose_target() {
    use engine::game::combat::AttackTarget;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    let a1 = scenario.add_artifact_from_oracle(P0, "Bauble A", "").id();
    let foreign = scenario
        .add_artifact_from_oracle(P1, "Foreign Relic", "")
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(arteeoh, AttackTarget::Player(P1))])
        .expect("Arteeoh attacks");
    let _ = runner.combat_damage();

    // REACH GUARD 1 — the production trigger prompt, and the walk's starting cursor.
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            source_id,
            target_slots,
            selection,
            ..
        } => {
            assert_eq!(*source_id, Some(arteeoh));
            assert_eq!(target_slots.len(), 2);
            assert_eq!(selection.current_slot, 0);
            assert!(selection.selected_slots.is_empty());
        }
        other => panic!("expected Arteeoh's TriggerTargetSelection, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a1)),
        })
        .expect("first exchange slot accepts its target");

    // REACH GUARD 2 (paired positive, MANDATORY): the walk advanced rather
    // than completing early.
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { selection, .. } => {
            assert_eq!(selection.current_slot, 1);
            assert_eq!(selection.selected_slots, vec![Some(TargetRef::Object(a1))]);
            assert_eq!(
                selection.current_legal_targets,
                vec![TargetRef::Object(foreign)]
            );
        }
        other => panic!("expected the walk to advance to slot 1, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(foreign)),
        })
        .expect("second exchange slot accepts its target"); // <- BASE Errs HERE
    runner
        .act(GameAction::PassPriority)
        .expect("priority passes");
    runner
        .act(GameAction::PassPriority)
        .expect("priority passes");

    // REACH GUARD 3
    match runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { source_id, .. } => assert_eq!(source_id, arteeoh),
        ref other => panic!("expected Arteeoh's OptionalEffectChoice, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the exchange");

    assert_eq!(runner.state().objects[&a1].controller, P1);
    assert_eq!(runner.state().objects[&foreign].controller, P0);
}

/// V3c's HOSTILE: both of Arteeoh's declared exchange slots are `optional:
/// false` (MEASURED at the live prompt) — this is the arm §5.6 has no
/// analogue for, and the reason V3c exists at all.
#[test]
fn arteeoh_choose_target_walk_rejects_a_declined_required_slot() {
    use engine::game::combat::AttackTarget;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    let a1 = scenario.add_artifact_from_oracle(P0, "Bauble A", "").id();
    let _foreign = scenario
        .add_artifact_from_oracle(P1, "Foreign Relic", "")
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(arteeoh, AttackTarget::Player(P1))])
        .expect("Arteeoh attacks");
    let _ = runner.combat_damage();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a1)),
        })
        .expect("first slot accepts");

    let result = runner.act(GameAction::ChooseTarget { target: None });
    assert!(
        result.is_err(),
        "both exchange slots are non-optional, so declining slot 1 must be rejected, \
         got {result:?}"
    );
}

/// Stage a 2-mode Shifting Grift cast through the CAST-TIME slot walk (bulk
/// `SelectTargets` is NOT submitted), returning the runner parked on
/// `WaitingFor::TargetSelection` with all 4 slots surfaced.
fn stage_grift_cast_time_target_selection(
    scenario: GameScenario,
    grift: engine::types::identifiers::ObjectId,
) -> engine::game::scenario::GameRunner {
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&grift].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: grift,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Shifting Grift");

    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: vec![0, 1],
                    })
                    .expect("select both Grift modes");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("auto-pay the announced Spree cost");
            }
            WaitingFor::TargetSelection { .. } => break,
            other => panic!("unexpected waiting state while driving to TargetSelection: {other:?}"),
        }
    }
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(
                target_slots.len(),
                4,
                "REACH GUARD: 2 modes x 2 slots each = 4"
            );
        }
        other => panic!("expected TargetSelection, got {other:?}"),
    }
    runner
}

/// V3d — §5.7 at the OTHER production entry point: a 2-mode Shifting Grift's
/// CAST-TIME slot walk, hitting `casting_targets.rs`'s `ChooseTarget`
/// `TargetSelectionAdvance::Complete` arm → `assign_selected_slots_in_chain`.
///
/// REVERT-FAILING BY OUTCOME (release-safe): at BASE, `chain_has_target_sink`
/// is false, so `assign_selected_slots_in_chain` takes its blanket early
/// return and ALL FOUR selected slots land on the root (creature) node — the
/// artifact pair is never exchanged even though all four targets were
/// legally submitted.
#[test]
fn shifting_grift_two_modes_walked_slot_by_slot_bind_their_own_pairs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana_pool());
    let c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let a1 = scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let a2 = scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = stage_grift_cast_time_target_selection(scenario, grift);

    // REACH GUARDS (paired positives, MANDATORY): the cursor advances by one
    // after each `ChooseTarget`.
    for (i, target) in [c1, c2, a1, a2].into_iter().enumerate() {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { selection, .. } => {
                assert_eq!(selection.current_slot, i);
            }
            other => panic!("expected TargetSelection at slot {i}, got {other:?}"),
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap_or_else(|e| {
                panic!("ChooseTarget({target:?}) at slot {i} must be accepted: {e:?}")
            });
    }

    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&c1].controller, P1);
    assert_eq!(runner.state().objects[&c2].controller, P0);
    assert_eq!(
        runner.state().objects[&a1].controller,
        P1,
        "the artifact pair must exchange against ITS OWN targets"
    );
    assert_eq!(runner.state().objects[&a2].controller, P0);
}

/// V3d's HOSTILE: `GameAction::ChooseTarget { target: None }` on any of the
/// four non-optional slots must be rejected.
#[test]
fn shifting_grift_choose_target_walk_rejects_a_declined_required_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana_pool());
    let _c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let _c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let _a1 = scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let _a2 = scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = stage_grift_cast_time_target_selection(scenario, grift);
    let result = runner.act(GameAction::ChooseTarget { target: None });
    assert!(
        result.is_err(),
        "the creature mode's first slot is non-optional, so declining it must be rejected, \
         got {result:?}"
    );
}

/// V18 — the TRIGGER AUTO-ASSIGN route (`triggers.rs`'s
/// `prepare_trigger_targets`, the branch taken when exactly one legal
/// assignment exists) stays `AutoAssigned`, with each node holding its own
/// list. This is the ONE production route with a SILENT failure mode: a §5.6
/// defect here does not surface as an `Err` anywhere — it becomes
/// `PreparedTriggerTargets::NeedsFallbackPush` →
/// `TriggerDispatchDisposition::DroppedTargetUnresolved`, and the trigger
/// simply vanishes. It is also Karona, False God Avatar's ONLY production
/// route (one Phase/Upkeep trigger, no `sub_ability`, no activated ability).
///
/// Table-driven over TWO carriers, both through the real pipeline with
/// VERBATIM Oracle text.
#[test]
fn paired_trigger_targets_auto_assign_without_a_prompt_and_bind_per_node() {
    // CARRIER A — Karona, False God Avatar: `ExchangeControl(Typed{You},
    // Typed{TargetOpponent})`, 2 claimed slots, no `sub_ability`. The staged
    // permanent must be P0's ONLY permanent — that (and only that) is what
    // makes exactly one legal assignment exist and forces the auto-select
    // branch. P1's board is cosmetic: `Typed{controller: TargetOpponent}`
    // resolves to the CONTROLLER (CR 603.2's triggering-event-player
    // fallback), so both slots read P0's permanents regardless of P1's
    // board.
    {
        let mut scenario = GameScenario::new();
        let _karona = scenario
            .add_enchantment_from_oracle(
                P0,
                "Karona, False God Avatar",
                KARONA_FALSE_GOD_AVATAR_TEXT,
            )
            .id();
        let _p1_creature = scenario.add_creature(P1, "P1 Creature", 2, 2).id();

        let mut runner = scenario.build();
        runner.advance_to_upkeep();

        // ASSERTED / REVERT-FAILING: never parks on TriggerTargetSelection.
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. }
            ),
            "the trigger must auto-assign, not prompt — got {:?}",
            runner.state().waiting_for
        );
        // It reaches the stack (a NeedsFallbackPush regression would instead
        // make stack.len() == 0 immediately).
        assert_eq!(
            runner.state().stack.len(),
            1,
            "the auto-assigned trigger must reach the stack"
        );
        // REACH GUARD (paired positive, MANDATORY), folded into the same
        // check: the dispatched stack entry carries exactly 2 REAL targets
        // and no sub-ability — this is what rules out "never parked on
        // TriggerTargetSelection" being satisfied vacuously by the
        // zero-slot `PreparedTriggerTargets::NoTargets` branch instead of a
        // genuine 2-slot auto-assignment (a standalone `build_target_slots`
        // probe outside a live trigger context cannot resolve
        // `ControllerRef::TargetOpponent`, which needs `triggering_event_player`
        // — CR 603.2 — so this in-pipeline check is the correct instrument).
        {
            use engine::types::game_state::StackEntryKind;
            let StackEntryKind::TriggeredAbility { ability, .. } =
                &runner.state().stack.back().unwrap().kind
            else {
                panic!("Karona's stack entry must be a TriggeredAbility");
            };
            assert_eq!(
                ability.targets.len(),
                2,
                "per-node ownership: this node's own claimed pair"
            );
            assert!(ability.sub_ability.is_none());
        }

        // DO NOT assert "the exchange happened" for Karona: the single legal
        // assignment is the same permanent in both slots
        // (`[Object(karona), Object(karona)]`), a disclosed, pre-existing
        // under-declaration (the parse surfaces two object slots and no
        // player slot for "target opponent"), so CR 701.12b makes it a no-op
        // — unaffected by this change either way.
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
        runner
            .act(GameAction::PassPriority)
            .expect("priority resolves the trigger");
        assert_eq!(
            runner.state().stack.len(),
            0,
            "two PassPriority must drain the auto-assigned trigger"
        );
    }

    // CARRIER B — Mister Negative: `ExchangeLifeTotals(Controller,
    // Typed{Opponent})`, 1 claimed slot, WITH a real `sub_ability`
    // (`Draw{EventContextAmount, Controller}`). Staged as a real cast from
    // hand so its ETB trigger fires through production dispatch; on a
    // two-player board there is exactly one opponent, forcing the
    // auto-select branch again. `at_phase(Phase::PreCombatMain)` is
    // MANDATORY (MEASURED): at the scenario default phase the cast is
    // rejected as sorcery-speed-only and the harness would panic before any
    // assertion ran.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 7);
        let mn = scenario
            .add_creature_to_hand_from_oracle(P0, "Mister Negative", 5, 5, MISTER_NEGATIVE_TEXT)
            .with_mana_cost(ManaCost::zero())
            .id();

        let mut runner = scenario.build();
        let outcome = runner.cast(mn).accept_optional().resolve();

        // REVERT-FAILING / ASSERTED: the paired node's own targets, and the
        // life-total exchange (per-node ownership, not a flat list).
        outcome.assert_life_delta(P0, -13);
        outcome.assert_life_delta(P1, 13);
    }
}

/// V18's REPLACEMENT HOSTILE — a carrier whose declared slots genuinely go
/// empty, on the SAME `prepare_trigger_targets` route: Arteeoh, Dread
/// Scavenger on a board with NO artifacts at all. Both declared
/// `ExchangeControl` slots have an EMPTY legal set, `build_target_slots`
/// `Err`s, and `prepare_trigger_targets` never reaches the stack (CR 603.3d —
/// "If a choice is required when the triggered ability goes on the stack but
/// no legal choices can be made for it… the ability is simply removed from
/// the stack").
///
/// This is what pins that §5.5/§5.6 did not turn a no-legal-target trigger
/// into an assignment error, or vice versa.
#[test]
fn arteeoh_combat_damage_trigger_with_no_legal_artifact_targets_is_dropped_not_errored() {
    use engine::game::combat::AttackTarget;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    // No artifacts anywhere.

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(arteeoh, AttackTarget::Player(P1))])
        .expect("Arteeoh attacks");
    let outcome = runner.combat_damage();

    // PAIRED POSITIVE REACH GUARD, MANDATORY: combat damage genuinely
    // landed, so the trigger event genuinely occurred and `stack.len() == 0`
    // below is a DROPPED trigger, not a trigger that never fired.
    assert_eq!(
        outcome.state().players[P1.0 as usize].life,
        17,
        "REACH GUARD: combat damage must have landed (20 -> 17)"
    );
    assert_eq!(
        outcome.state().stack.len(),
        0,
        "with no legal artifact targets anywhere, the trigger must be dropped, not raise a \
         prompt and not error"
    );
}

/// CONTROL for the hostile above: the same staging WITH one artifact per
/// player must raise the prompt (`stack.len() == 1`) — proving the
/// instrument fires when targets ARE available.
#[test]
fn arteeoh_combat_damage_trigger_with_legal_artifact_targets_raises_the_prompt() {
    use engine::game::combat::AttackTarget;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    let _a1 = scenario.add_artifact_from_oracle(P0, "Bauble A", "").id();
    let _foreign = scenario
        .add_artifact_from_oracle(P1, "Foreign Relic", "")
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(arteeoh, AttackTarget::Player(P1))])
        .expect("Arteeoh attacks");
    let _ = runner.combat_damage();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "with legal artifact targets available, the trigger must raise the prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
}

/// V8 — announced mode order does not disturb the mapping. Modes are
/// declared to the reducer in ANNOUNCEMENT order (`[1, 0]` — artifact mode
/// first, creature mode second), but slots are still bound in CR 608.2c
/// PRINTED order (creature-pair-then-artifact-pair), because
/// `build_chained_resolved`/`ordered_selected_mode_indices` sort the chosen
/// indices before chaining — a fact this change's collect/assign arms do not
/// alter.
///
/// REACH GUARD: `selected_mode_labels` names the creature mode first and the
/// artifact mode second, regardless of announcement order.
#[test]
fn shifting_grift_modes_announced_out_of_order_still_bind_printed_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana_pool());
    let c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let a1 = scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let a2 = scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // Announce mode 1 (artifacts) BEFORE mode 0 (creatures) — the reverse of
    // printed order.
    let commit = runner
        .cast(grift)
        .modes(&[1, 0])
        .target_objects(&[c1, c2, a1, a2])
        .commit();

    {
        use engine::types::game_state::StackEntryKind;
        let StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } = &commit.state().stack.back().unwrap().kind
        else {
            panic!("Shifting Grift must finalize its ability at commit");
        };
        assert_eq!(
            ability.selected_mode_labels.len(),
            2,
            "both announced modes must have a label"
        );
        assert!(
            ability.selected_mode_labels[0]
                .to_lowercase()
                .contains("creature"),
            "REACH GUARD: PRINTED order (creature mode first), not announcement order \
             (artifact mode was announced first) — labels were {:?}",
            ability.selected_mode_labels
        );
        assert!(
            ability.selected_mode_labels[1]
                .to_lowercase()
                .contains("artifact"),
            "labels were {:?}",
            ability.selected_mode_labels
        );
        // Per-node target ownership itself is unaffected by announcement
        // order — the SAME structural check V1 makes.
        assert_eq!(
            ability.targets,
            vec![TargetRef::Object(c1), TargetRef::Object(c2)],
            "the creature mode (root) must still claim exactly its own pair"
        );
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("the artifact mode must be chained as a sub-ability");
        assert_eq!(
            sub.targets,
            vec![TargetRef::Object(a1), TargetRef::Object(a2)],
            "the artifact mode (sub node) must still claim exactly its own pair"
        );
    }

    let outcome = commit.resolve();
    assert_eq!(outcome.state().objects[&c1].controller, P1);
    assert_eq!(outcome.state().objects[&c2].controller, P0);
    assert_eq!(outcome.state().objects[&a1].controller, P1);
    assert_eq!(outcome.state().objects[&a2].controller, P0);
}

/// V9 — the fix generalises past N = 2. A 3-mode Shifting Grift exchanges
/// THREE disjoint pairs, each node holding exactly its own targets: 6 slots,
/// 6 transients over 6 distinct objects. This is the row that exercises the
/// recursive descent at chain depth 3 (root + 2 sub-abilities), not just
/// depth 2, and doubles as the residual-risk (3-mode Grift combinatorics)
/// smoke signal — the whole cast must complete well inside the default test
/// timeout.
///
/// REACH GUARD: `target_slots.len() == 6` at announcement.
/// `allow_repeat_modes: false` makes CR 700.2d's duplicate-mode case
/// UNREACHABLE here — recorded, not tested.
#[test]
fn shifting_grift_all_three_modes_exchange_three_disjoint_pairs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana_pool());
    let c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let a1 = scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let a2 = scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let e1 = scenario
        .add_enchantment_from_oracle(P0, "Enchantment E1", "")
        .id();
    let e2 = scenario
        .add_enchantment_from_oracle(P1, "Enchantment E2", "")
        .id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let commit = runner
        .cast(grift)
        .modes(&[0, 1, 2])
        .target_objects(&[c1, c2, a1, a2, e1, e2])
        .commit();

    // REACH GUARD: per-node ownership at announcement, structurally, at
    // chain depth 3 — root (creatures), sub (artifacts), sub-of-sub
    // (enchantments).
    {
        use engine::types::game_state::StackEntryKind;
        let StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } = &commit.state().stack.back().unwrap().kind
        else {
            panic!("Shifting Grift must finalize its ability at commit");
        };
        assert_eq!(
            ability.targets,
            vec![TargetRef::Object(c1), TargetRef::Object(c2)]
        );
        let sub_artifacts = ability
            .sub_ability
            .as_ref()
            .expect("the artifact mode must be chained as a sub-ability");
        assert_eq!(
            sub_artifacts.targets,
            vec![TargetRef::Object(a1), TargetRef::Object(a2)]
        );
        let sub_enchantments = sub_artifacts
            .sub_ability
            .as_ref()
            .expect("the enchantment mode must be chained two levels deep");
        assert_eq!(
            sub_enchantments.targets,
            vec![TargetRef::Object(e1), TargetRef::Object(e2)]
        );
    }

    let outcome = commit.resolve();
    assert_eq!(outcome.state().objects[&c1].controller, P1);
    assert_eq!(outcome.state().objects[&c2].controller, P0);
    assert_eq!(outcome.state().objects[&a1].controller, P1);
    assert_eq!(outcome.state().objects[&a2].controller, P0);
    assert_eq!(outcome.state().objects[&e1].controller, P1);
    assert_eq!(outcome.state().objects[&e2].controller, P0);
}

/// V11 — the no-legal-target rejection is the STABLE outcome across BOTH
/// slot builders, on a board with creatures but NO artifacts.
///
/// (b) DISCRIMINATING HALF: `build_target_slots(&build_chained_resolved(..))`
/// (whole-chain) must `Err` — BASE returns `Ok` with 2 slots (the artifact
/// mode's `no_legal_target_slots()` exit was never reached because the
/// whole-chain collect pass returned before descending into it). Asserts on
/// a `Result`, so revert-red in `--release` too.
/// (a) NON-DISCRIMINATING STABILITY HALF, labelled as such: `SelectModes`
/// itself still `Err`s at BASE and after (the per-mode `build_target_slots_labelled`
/// builder was never broken) — a no-regression sibling, not evidence.
#[test]
fn shifting_grift_artifactless_second_mode_is_rejected_by_both_slot_builders() {
    use engine::game::ability_utils::{build_chained_resolved, build_target_slots};
    use engine::parser::oracle::parse_oracle_text;

    // ---- (b) THE DISCRIMINATING HALF — direct against the two `pub` builders, on a
    // board with creatures but NO artifacts. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let _c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let runner = scenario.build();

    let parsed = parse_oracle_text(SHIFTING_GRIFT_TEXT, "Shifting Grift", &[], &[], &[]);
    let chained = build_chained_resolved(&parsed.abilities, &[0, 1], grift, P0).unwrap();
    let whole_chain = build_target_slots(runner.state(), &chained);
    assert!(
        whole_chain.is_err(),
        "REVERT-FAILING: with no legal artifact targets anywhere, the whole-chain build \
         must Err (BASE: Ok with 2 truncated slots), got {whole_chain:?}"
    );

    // REACH GUARD: the same shape WITH an artifact on each side yields `Ok`
    // with 4 slots — a fresh board so the artifactless board above is not
    // itself mutated (MEASURED, plan round-3 P4).
    let mut artifact_scenario = GameScenario::new();
    artifact_scenario.at_phase(Phase::PreCombatMain);
    let ac1 = artifact_scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let ac2 = artifact_scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let _aa1 = artifact_scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let _aa2 = artifact_scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let artifact_grift = artifact_scenario
        .add_spell_to_hand_from_oracle(P0, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let artifact_runner = artifact_scenario.build();
    let chained_with_artifacts =
        build_chained_resolved(&parsed.abilities, &[0, 1], artifact_grift, P0).unwrap();
    let slots_with_artifacts = build_target_slots(artifact_runner.state(), &chained_with_artifacts)
        .expect("REACH GUARD: the same shape with an artifact on each side must build");
    assert_eq!(
        slots_with_artifacts.len(),
        4,
        "REACH GUARD: the same shape with an artifact on each side yields 4 slots"
    );
    let _ = (ac1, ac2);

    // ---- (a) THE NON-DISCRIMINATING STABILITY HALF, labelled as such: the real
    // `SelectModes` outcome is unchanged — still `Err` — because the per-mode
    // `build_target_slots_labelled` builder was never broken (CR 700.2a). Reuses
    // the SAME artifactless `runner` from (b), driven through the real cast
    // pipeline. ----
    let mut runner = runner;
    let card_id = runner.state().objects[&grift].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: grift,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Shifting Grift");
    let select_modes_result = loop {
        match runner.state().waiting_for.clone() {
            WaitingFor::ModeChoice { .. } => {
                break runner.act(GameAction::SelectModes {
                    indices: vec![0, 1],
                });
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("auto-pay Grift's base cost");
            }
            other => panic!("unexpected waiting state before ModeChoice: {other:?}"),
        }
    };
    assert!(
        select_modes_result.is_err(),
        "non-discriminating stability sibling: SelectModes must still Err on both sides \
         (ActionNotAllowed(\"No legal targets available\")), got {select_modes_result:?}"
    );

    // HOSTILE: mode 0 ALONE on the same (artifactless) board must still
    // succeed — the artifactless mode is what's rejected, not the whole cast.
    let mode0_only = build_chained_resolved(&parsed.abilities, &[0], grift, P0).unwrap();
    assert!(
        build_target_slots(runner.state(), &mode0_only).is_ok(),
        "HOSTILE: mode 0 alone must still succeed on the artifactless board"
    );
}

/// V12 — retargeting a chained paired-subject spell (CR 115.7d) spans the
/// resolved chain: EVERY mode's targets are addressed, each at the node that
/// owns it. Seam: `engine.rs::apply_retarget` -> `RetargetSlotAddress` ->
/// `ability_utils::chain_retarget_slots`.
///
/// P1 casts a 2-mode Shifting Grift (creatures + artifacts). Perplexing
/// Chimera's "whenever an opponent casts a spell" trigger fires for P0; P0
/// accepts the exchange (steals the Grift spell) and is offered "you may
/// choose new targets for the spell".
///
/// REVERT-FAILING (two directions):
/// 1. At `bb28b0e8b` this row's first assertion fails — `current_targets.len()`
///    is 2, not 4 — because `change_targets::resolve` read `stack_ability.targets`
///    on the root only.
/// 2. At BASE, before `bb28b0e8b`, this cast PANICS at `SelectModes` — the
///    per-node target ownership that commit introduced is the precondition an
///    address `(node, slot)` depends on, and this note is what guards it from
///    being quietly undone.
#[test]
fn chimera_retarget_of_a_two_mode_grift_offers_every_modes_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, generic_mana_pool());
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let c1 = scenario.add_creature(P0, "Creature C1", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Creature C2", 2, 2).id();
    let a1 = scenario
        .add_artifact_from_oracle(P0, "Artifact A1", "")
        .id();
    let a2 = scenario
        .add_artifact_from_oracle(P1, "Artifact A2", "")
        .id();
    let a3 = scenario
        .add_artifact_from_oracle(P0, "Artifact A3", "")
        .id();
    let grift = scenario
        .add_spell_to_hand_from_oracle(P1, "Shifting Grift", false, SHIFTING_GRIFT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut commit = runner
        .cast(grift)
        .modes(&[0, 1])
        .target_objects(&[c1, c2, a1, a2])
        .commit();

    // Drain to the retarget prompt, accepting the Chimera trigger on the way.
    let mut reached = None;
    for _ in 0..40 {
        match &commit.state().waiting_for {
            WaitingFor::RetargetChoice {
                current_targets,
                slots,
                slot_pools,
                legal_new_targets,
                ..
            } => {
                reached = Some((
                    current_targets.clone(),
                    slots.clone(),
                    slot_pools.clone(),
                    legal_new_targets.clone(),
                ));
                break;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                commit
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the Chimera trigger must succeed");
            }
            WaitingFor::Priority { .. } => {
                assert!(
                    !commit.state().stack.is_empty(),
                    "the stack emptied before the retarget prompt was raised"
                );
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining");
            }
            other => panic!("unexpected state while draining to the retarget prompt: {other:?}"),
        }
    }
    let (current_targets, slots, slot_pools, legal_new_targets) =
        reached.expect("REACH GUARD: the retarget prompt must be raised");

    // REACH GUARD: the Chimera exchange really happened.
    assert_eq!(
        commit.state().objects.get(&chimera).unwrap().controller,
        P1,
        "REACH GUARD: the Chimera must have swapped to P1"
    );

    // Assertion 1: EVERY mode's targets are exposed, in order — the ordering
    // is load-bearing because the submission shares its index space, and
    // because the first two positions are exactly BASE's `current_targets`
    // (Invariant B).
    assert_eq!(
        current_targets,
        vec![
            TargetRef::Object(c1),
            TargetRef::Object(c2),
            TargetRef::Object(a1),
            TargetRef::Object(a2),
        ],
        "every mode's targets must be exposed, in order, got {current_targets:?}"
    );

    // Assertion 2: the addresses pin the code owner's mechanism itself — the
    // creature mode's pair lives at the root, the artifact mode's pair lives
    // at the sub.
    assert_eq!(slots.len(), 4);
    assert!(slots[0].path.is_empty() && slots[1].path.is_empty());
    assert_eq!(slots[2].path, vec![ChainStep::SubAbility]);
    assert_eq!(slots[3].path, vec![ChainStep::SubAbility]);
    assert_eq!(slots[0].slot, 0);
    assert_eq!(slots[1].slot, 1);
    assert_eq!(slots[2].slot, 0);
    assert_eq!(slots[3].slot, 1);

    // Assertion 3 (Invariant SC): the two paired-subject root positions share
    // one authority; every offered candidate at every position is admitted
    // there (asserted end-to-end via the reducer itself below, positions 0/2
    // via direct submission).
    assert_eq!(
        slot_pools[0], slot_pools[1],
        "the two root positions share one authority"
    );

    // Assertion 4: THE DISCRIMINATOR — the union still offers the artifact
    // A3, and position 2 (the artifact-mode slot) may take it.
    assert!(
        legal_new_targets.contains(&TargetRef::Object(a3)),
        "the union must still offer every position's pool, got {legal_new_targets:?}"
    );
    assert!(
        slot_pools[2].contains(&TargetRef::Object(a3)),
        "the artifact-mode position must admit A3, got {:?}",
        slot_pools[2]
    );

    // Assertion 5: the creature mode's own pool survives the union rather
    // than being replaced (Invariant B, literal): the union is BASE's cascade
    // verbatim as a prefix.
    assert!(legal_new_targets.contains(&TargetRef::Object(c1)));

    // Assertion 7 (hostile, checked BEFORE the accepted submission so the
    // parked state is untouched): the sub-only artifact into position 0 (a
    // `Filtered(creature)` position) must be refused.
    let hostile = commit.act(GameAction::RetargetSpell {
        new_targets: vec![
            TargetRef::Object(a3),
            TargetRef::Object(c2),
            TargetRef::Object(a1),
            TargetRef::Object(a2),
        ],
    });
    assert!(
        hostile.is_err(),
        "a sub-only object must not be admitted at the creature mode's own position"
    );

    // Assertion 6: the maintainer's literal requirement — submit a full,
    // legal cross-mode reassignment.
    commit
        .act(GameAction::RetargetSpell {
            new_targets: vec![
                TargetRef::Object(c1),
                TargetRef::Object(c2),
                TargetRef::Object(a3),
                TargetRef::Object(a2),
            ],
        })
        .expect("a full cross-mode reassignment offered by the union must be accepted");

    // Resolve fully.
    for _ in 0..40 {
        match commit.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if commit.state().stack.is_empty() {
                    break;
                }
                let _ = commit.act(GameAction::PassPriority);
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                let _ = commit.act(GameAction::DecideOptionalEffect { accept: true });
            }
            _ => break,
        }
    }

    // Assertion 8: the exchange resolved against the RETARGETED pair (a3, a2)
    // for the artifact mode, and the untouched creature pair for the creature
    // mode.
    assert_eq!(commit.state().objects[&a3].controller, P1);
    assert_eq!(commit.state().objects[&a2].controller, P0);
    assert_eq!(commit.state().objects[&a1].controller, P0);
    assert_eq!(commit.state().objects[&c1].controller, P1);
    assert_eq!(commit.state().objects[&c2].controller, P0);
}

/// V15 — the newly reachable sub-chain descent (§5.2/§5.3) surfaces NO
/// additional slot anywhere in the corpus. Table-driven over the six
/// measurable carriers of the nine paired-with-`sub_ability` corpus chains
/// (Modify Memory, Profane Transfusion and Sudden Substitution are omitted:
/// their spell bodies parse to `Effect::Unimplemented` on this route; their
/// sub shapes — `Draw{Fixed, Controller}`, `Effect::Unimplemented` — are each
/// covered by another carrier in this table).
///
/// PINNED EXPECTATIONS (MEASURED, plan round-3 P3): `Ok(2)`, `Ok(2)`,
/// `Ok(1)`, `Ok(1)`, `Ok(0)`, `Ok(1)` respectively, with `effect_kind` per
/// slot restricted to `{ExchangeControl, ExchangeLifeTotals}` and NEVER
/// `CopyTokenOf` / `ChangeTargets` / `Draw` — the HOSTILE half, asserted
/// explicitly per carrier.
///
/// REACH GUARD (paired positive): a non-zero slot count for at least four of
/// the six, and Arteeoh's `CopyTokenOf` slot IS observable at its own LATER
/// prompt (V3c's post-accept assertion, on the same Oracle text) — so "no
/// extra slot here" is a real negative, not a dead instrument.
#[test]
fn paired_sub_ability_chains_surface_no_additional_slots() {
    use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{Effect, EffectKind};
    use engine::types::identifiers::ObjectId;

    const DJINN_OF_INFINITE_DECEITS_TEXT: &str = "Flying\n{T}: Exchange control of two target \
        nonlegendary creatures. You can't activate this ability during combat.";
    // GILDED_DRAKE_TEXT is the module-level const (identical text) shared
    // with the other Gilded Drake rows.
    const VOLATILE_STORMDRAKE_TEXT: &str = "Flying, hexproof from activated and triggered \
        abilities\nWhen this creature enters, exchange control of this creature and target \
        creature an opponent controls. If you do, you get {E}{E}{E}{E}, then sacrifice that \
        creature unless you pay an amount of {E} equal to its mana value.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arteeoh = scenario
        .add_creature_from_oracle(P0, "Arteeoh, Dread Scavenger", 3, 3, ARTEEOH_TEXT)
        .id();
    let djinn = scenario
        .add_creature_from_oracle(
            P0,
            "Djinn of Infinite Deceits",
            2,
            7,
            DJINN_OF_INFINITE_DECEITS_TEXT,
        )
        .id();
    let drake = scenario
        .add_creature_from_oracle(P0, "Gilded Drake", 3, 3, GILDED_DRAKE_TEXT)
        .id();
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let stormdrake = scenario
        .add_creature_from_oracle(P0, "Volatile Stormdrake", 3, 2, VOLATILE_STORMDRAKE_TEXT)
        .id();
    let mister_negative = scenario
        .add_creature_from_oracle(P0, "Mister Negative", 5, 5, MISTER_NEGATIVE_TEXT)
        .id();
    // Legal-target scaffolding: two nonlegendary P1 creatures (Djinn — "two
    // target nonlegendary creatures", no controller restriction), an
    // opponent's creature (Gilded Drake / Volatile Stormdrake), and two
    // artifacts (Arteeoh's "two other target artifacts").
    let _p1_creature_a = scenario.add_creature(P1, "P1 Creature A", 2, 2).id();
    let _p1_creature_b = scenario.add_creature(P1, "P1 Creature B", 2, 2).id();
    let _a1 = scenario.add_artifact_from_oracle(P0, "Bauble A", "").id();
    let _a2 = scenario.add_artifact_from_oracle(P1, "Bauble B", "").id();

    let runner = scenario.build();

    let expect_slots = |name: &str,
                        text: &str,
                        source: ObjectId,
                        expected: usize,
                        expected_kinds: &[EffectKind]| {
        let parsed = parse_oracle_text(text, name, &[], &[], &[]);
        let def = if let Some(trigger) = parsed.triggers.first() {
            *trigger
                .execute
                .clone()
                .expect("the trigger must have an execute")
        } else {
            // Some cards' `abilities` list carries a leading keyword-line
            // entry (e.g. "Flying" parses to its own `Unimplemented` ability)
            // ahead of the real paired-subject ability — find the one whose
            // effect is actually ExchangeControl/ExchangeLifeTotals rather
            // than assuming index 0.
            parsed
                .abilities
                .iter()
                .find(|a| {
                    matches!(
                        *a.effect,
                        Effect::ExchangeControl { .. } | Effect::ExchangeLifeTotals { .. }
                    )
                })
                .unwrap_or_else(|| {
                    panic!(
                        "{name}: no paired-subject ability found in {:?}",
                        parsed.abilities
                    )
                })
                .clone()
        };
        let resolved = build_resolved_from_def(&def, source, P0);
        let slots = build_target_slots(runner.state(), &resolved)
            .unwrap_or_else(|e| panic!("{name}: build_target_slots failed: {e:?}"));
        assert_eq!(
            slots.len(),
            expected,
            "{name}: expected {expected} slots, got {slots:?}"
        );
        for slot in &slots {
            assert!(
                expected_kinds.contains(&slot.effect_kind),
                "{name}: HOSTILE — slot effect_kind {:?} must be in {expected_kinds:?}, never \
                 CopyTokenOf / ChangeTargets / Draw (no extra slot from the sub-chain descent)",
                slot.effect_kind
            );
        }
    };

    expect_slots(
        "Arteeoh, Dread Scavenger",
        ARTEEOH_TEXT,
        arteeoh,
        2,
        &[EffectKind::ExchangeControl],
    );
    expect_slots(
        "Djinn of Infinite Deceits",
        DJINN_OF_INFINITE_DECEITS_TEXT,
        djinn,
        2,
        &[EffectKind::ExchangeControl],
    );
    expect_slots(
        "Gilded Drake",
        GILDED_DRAKE_TEXT,
        drake,
        1,
        &[EffectKind::ExchangeControl],
    );
    expect_slots(
        "Mister Negative",
        MISTER_NEGATIVE_TEXT,
        mister_negative,
        1,
        &[EffectKind::ExchangeLifeTotals],
    );
    expect_slots(
        "Perplexing Chimera",
        PERPLEXING_CHIMERA_TEXT,
        chimera,
        0,
        &[],
    );
    expect_slots(
        "Volatile Stormdrake",
        VOLATILE_STORMDRAKE_TEXT,
        stormdrake,
        1,
        &[EffectKind::ExchangeControl],
    );
}
