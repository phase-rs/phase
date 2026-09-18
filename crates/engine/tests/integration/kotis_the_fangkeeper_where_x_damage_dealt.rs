//! Regression for issue #5923 and for the resolution-scoped cast window.
//!
//! Kotis, the Fangkeeper's combat-damage trigger must exile the top X cards of
//! the damaged player's library (X = damage dealt) and let Kotis's controller
//! cast, from among just that exiled batch, only the cards with mana value X or
//! less — **while the trigger is resolving**.
//!
//! Two distinct defects are covered here:
//!
//! 1. Issue #5923: before the `oracle_nom/quantity.rs` fix, the "where X is the
//!    amount of damage dealt" binding was left unresolved, so the totality guard
//!    in `oracle_effect/lower.rs` collapsed both the `ExileTop` step and the
//!    `CastFromZone` sub-ability to `Effect::Unimplemented` and neither the
//!    exile nor the free-cast offer ever happened.
//!    <https://github.com/phase-rs/phase/issues/5923>
//!
//! 2. The cast grant was then lowered to an INDEFINITE lingering
//!    `CastingPermission` ("stay castable until they leave exile"). That is
//!    wrong for this Oracle grammar. CR 608.2g: a resolving object "continues to
//!    resolve, which may include casting other spells this way", and "no other
//!    spells can normally be cast … during resolution" — there is no later
//!    window in which the permission could be used. WotC's own ruling for Kotis
//!    is explicit: "You cast the spells from among the exiled cards while
//!    Kotis's last ability is resolving and still on the stack. You can't wait
//!    to cast them later in the turn."

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastOfferKind, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KOTIS_ORACLE: &str = "Indestructible\nWhenever Kotis deals combat damage to a player, exile the top X cards of their library, where X is the amount of damage dealt. You may cast any number of spells with mana value X or less from among them without paying their mana costs.";

struct KotisFixture {
    runner: GameRunner,
    cheap: ObjectId,
    expensive: ObjectId,
    filler: ObjectId,
    controller_top: ObjectId,
}

/// CR 120.2a: Kotis deals 2 combat damage (each attacking creature deals combat
/// damage equal to its power), so X = 2. The damaged player's top two library
/// cards are exiled (one within budget, MV 1; one over budget, MV 5) and a third
/// card stays in the library, proving the exile is bounded to exactly X cards
/// from the DAMAGED player's library, not Kotis's controller's.
fn kotis_combat_damage_fixture() -> KotisFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (Kotis's controller) has its own library card that must NOT be
    // touched by Kotis's trigger.
    let controller_top = scenario.add_card_to_library_top(P0, "Controller Top");

    // P1 (the damaged player) library, top-to-bottom after seeding:
    // Cheap Card (MV 1, within budget) -> Expensive Card (MV 5, over budget)
    // -> Filler Card (must remain in library; outside the top-X window).
    let filler = scenario.add_card_to_library_top(P1, "Filler Card");
    let expensive = scenario.add_card_to_library_top(P1, "Expensive Card");
    let cheap = scenario.add_card_to_library_top(P1, "Cheap Card");

    let kotis = scenario
        .add_creature(P0, "Kotis, the Fangkeeper", 2, 1)
        .from_oracle_text_with_keywords(&["Indestructible"], KOTIS_ORACLE)
        .id();

    let mut runner = scenario.build();
    {
        let card = runner.state_mut().objects.get_mut(&cheap).unwrap();
        card.card_types.core_types.push(CoreType::Instant);
        card.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 1,
        };
    }
    {
        let card = runner.state_mut().objects.get_mut(&expensive).unwrap();
        card.card_types.core_types.push(CoreType::Instant);
        card.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 5,
        };
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(kotis, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Kotis attacking P1");

    KotisFixture {
        runner,
        cheap,
        expensive,
        filler,
        controller_top,
    }
}

/// Drive combat until Kotis's trigger has opened its resolution-scoped cast
/// window: declare no blockers, order the single trigger, accept the "you may"
/// offer, and pass priority until the window is parked.
fn drain_until_kotis_cast_window(runner: &mut GameRunner) -> Vec<ObjectId> {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order Kotis's trigger");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            // CR 603.5 + CR 608.2d: the "you may cast ... from among them"
            // sub-ability is a "may" effect — accept it so the cast window opens.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional cast offer");
            }
            // PRIMARY: the trigger pauses mid-resolution on the free-cast window.
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::FreeCastWindow { candidates, .. },
            } => {
                assert_eq!(player, P0, "the window belongs to Kotis's controller");
                return candidates;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while draining Kotis's trigger");
            }
            other => panic!(
                "unexpected waiting state while draining Kotis's trigger: {other:?} \
                 (phase={:?})",
                runner.state().phase
            ),
        }
    }
    panic!("Kotis's trigger never opened its resolution-scoped cast window");
}

/// The granting source carried by the parked window's immutable policy.
///
/// Reach guards read the batch through THIS id rather than through a
/// fixture-side handle so the guard is anchored to the very window whose
/// candidate list is under assertion. A window that belonged to some other
/// source would then fail the guard instead of silently validating a batch the
/// offer never consulted.
fn kotis_window_source(runner: &GameRunner) -> ObjectId {
    let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow { face_policy, .. },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected to be parked on Kotis's free-cast window, got {:?}",
            runner.state().waiting_for
        );
    };
    face_policy.source_id
}

/// The engine's per-source "exiled this turn" ledger — the full batch BEFORE
/// any mana-value budget is applied.
///
/// This is the set the `expensive` reach guard needs: `member_pool` on the
/// window is already budget-filtered, so it can never witness that an
/// over-budget card reached the batch at all.
fn tracked_exile_batch(state: &GameState, source: ObjectId) -> &[ObjectId] {
    state
        .cards_exiled_with_source_this_turn
        .get(&source)
        .map_or(&[][..], Vec::as_slice)
}

/// CR 304.1 + CR 307.1: the card-type half of "cast any number of spells" —
/// an instant card or a sorcery card is what may be cast as a spell.
///
/// Kept as its own predicate so a reach guard can prove that type eligibility
/// is NOT what excluded a negative control, isolating the exclusion to the one
/// property under test (mana-value budget, or batch membership).
fn is_instant_or_sorcery(state: &GameState, id: ObjectId) -> bool {
    let core_types = &state.objects[&id].card_types.core_types;
    core_types.contains(&CoreType::Instant) || core_types.contains(&CoreType::Sorcery)
}

/// CR 608.2g + CR 608.2h: the batch is exiled, the window opens DURING the
/// trigger's resolution, and the frozen X (= 2 damage dealt) admits only the
/// mana-value-1 card from that same batch.
///
/// Revert guard: on the old `LingeringPermission` lowering this test fails at
/// `drain_until_kotis_cast_window`, because the trigger finished resolving and
/// handed back priority instead of ever parking a `FreeCastWindow`.
#[test]
fn kotis_opens_a_resolution_scoped_window_bounded_by_x() {
    let KotisFixture {
        mut runner,
        cheap,
        expensive,
        filler,
        controller_top,
    } = kotis_combat_damage_fixture();

    let candidates = drain_until_kotis_cast_window(&mut runner);

    // Exactly the top two P1 library cards were exiled; the third stays put,
    // and P0's own library is untouched. "Their library" is an Oracle-text
    // grammar interpretation — the pronoun binds to the nearest preceding
    // player noun, the damaged player from "deals combat damage to a
    // player," not Kotis's controller — not a claim covered by a specific CR
    // number (CR 608.2c governs the ORDER effects apply their instructions,
    // not pronoun antecedents).
    let state = runner.state();
    assert_eq!(state.objects[&cheap].zone, Zone::Exile);
    assert_eq!(state.objects[&expensive].zone, Zone::Exile);
    assert_eq!(
        state.objects[&filler].zone,
        Zone::Library,
        "only the top X (2) cards may be exiled, not the whole library"
    );
    assert_eq!(
        state.objects[&controller_top].zone,
        Zone::Library,
        "Kotis must exile from the DAMAGED player's library, not its controller's"
    );

    // CR 608.2h: X was determined once, as the trigger resolved (2 damage), and
    // bounds the offer. Both cards are in the same exiled batch; only the one
    // within the ceiling may be offered.
    assert!(
        candidates.contains(&cheap),
        "a mana value 1 card (<= X=2) exiled by Kotis must be offered"
    );
    assert!(
        !candidates.contains(&expensive),
        "a mana value 5 card (> X=2) must NOT be offered even though it was exiled in the same batch"
    );
}

/// CR 608.2g: accepting casts the spell AS the trigger resolves — the card goes
/// straight to the stack from exile, without the controller ever regaining
/// priority in between.
#[test]
fn kotis_free_casts_the_chosen_spell_during_the_trigger_resolution() {
    let KotisFixture {
        mut runner, cheap, ..
    } = kotis_combat_damage_fixture();

    drain_until_kotis_cast_window(&mut runner);
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(cheap),
        })
        .expect("free-casting the exiled card must succeed");

    assert_eq!(
        runner.state().objects[&cheap].zone,
        Zone::Stack,
        "the chosen card must be cast onto the stack during the trigger's resolution"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the cast is free (CR 118.9) and must consume no mana"
    );

    // Drain the stack so the free-cast spell resolves.
    for _ in 0..24 {
        if runner.state().stack.is_empty() {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert_ne!(
        runner.state().objects[&cheap].zone,
        Zone::Exile,
        "the cast card must have left exile"
    );
}

/// CR 608.2g: THE defect this change fixes. Declining the window ends the
/// controller's opportunity — the exiled batch stays in exile with NO standing
/// casting permission, because "you can't wait to cast them later in the turn".
///
/// Revert guard: under the old `LingeringPermission` lowering the declined
/// `cheap` card remained in `spell_objects_available_to_cast` for the rest of
/// the game, so both assertions below flip.
#[test]
fn kotis_declining_leaves_no_lingering_cast_permission() {
    let KotisFixture {
        mut runner,
        cheap,
        expensive,
        ..
    } = kotis_combat_damage_fixture();

    drain_until_kotis_cast_window(&mut runner);
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the resolution-scoped window must succeed");

    // Return to a priority window — the point at which a lingering permission
    // would have become exercisable.
    //
    // REACH GUARD: the loop below has two exits — the intended empty-stack
    // `WaitingFor::Priority`, and a `PassPriority` rejection. Only the first one
    // proves the decline actually carried the resolution chain to completion, so
    // record it and require it BEFORE reading `spell_objects_available_to_cast`.
    // Without this, a continuation that stalls after
    // `FreeCastWindowChoice { selection: None }` (leaving the engine parked on
    // some non-priority `WaitingFor`) would fall out of the loop on the error
    // arm and still satisfy the two "no permission" assertions vacuously — the
    // permission scan is trivially empty in a state that never reached a
    // priority window at all.
    let mut reached_empty_stack_priority = false;
    for _ in 0..24 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            reached_empty_stack_priority = true;
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        reached_empty_stack_priority,
        "declining the window must let the rest of the resolution chain finish and hand \
         priority back with an empty stack; parked at {:?} with stack {:?}",
        runner.state().waiting_for,
        runner.state().stack.len(),
    );

    let state = runner.state();
    assert_eq!(
        state.objects[&cheap].zone,
        Zone::Exile,
        "a declined card stays in exile"
    );
    let available = spell_objects_available_to_cast(state, P0);
    assert!(
        !available.contains(&cheap),
        "declining the resolution window must leave NO casting permission behind — \
         CR 608.2g gives no later opportunity (Kotis ruling: \"You can't wait to cast \
         them later in the turn\")"
    );
    assert!(
        !available.contains(&expensive),
        "the over-budget card was never castable and must stay uncastable"
    );
}

/// Batch-scope fixture. Kotis has power 3, so X = 3 (CR 120.2a) and the exiled
/// batch holds TWO cards inside the budget (MV 1 and MV 3) next to one outside
/// it (MV 5). A fourth, in-budget card is put into exile by an entirely
/// unrelated mechanism to serve as the negative control for "from among them".
///
/// The control card is owned by the DAMAGED player, exactly like the three
/// batch cards, so ownership cannot be what excludes it — only the
/// exiled-by-this-source pool can be.
struct KotisBatchFixture {
    runner: GameRunner,
    kotis: ObjectId,
    cheap_one: ObjectId,
    cheap_two: ObjectId,
    expensive: ObjectId,
    unrelated_exiled: ObjectId,
}

fn kotis_batch_scope_fixture() -> KotisBatchFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 (the damaged player) library, top-to-bottom after seeding:
    // Cheap One (MV 1) -> Cheap Two (MV 3) -> Expensive Card (MV 5).
    let expensive = scenario.add_card_to_library_top(P1, "Expensive Card");
    let cheap_two = scenario.add_card_to_library_top(P1, "Cheap Two");
    let cheap_one = scenario.add_card_to_library_top(P1, "Cheap One");

    // Exiled by an unrelated mechanism, never touched by Kotis, within the
    // mana-value budget and otherwise perfectly eligible.
    let unrelated_exiled = scenario
        .add_spell_to_exile(P1, "Unrelated Exiled Card", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let kotis = scenario
        .add_creature(P0, "Kotis, the Fangkeeper", 3, 1)
        .from_oracle_text_with_keywords(&["Indestructible"], KOTIS_ORACLE)
        .id();

    let mut runner = scenario.build();
    for (id, generic) in [(cheap_one, 1), (cheap_two, 3), (expensive, 5)] {
        let card = runner.state_mut().objects.get_mut(&id).unwrap();
        card.card_types.core_types.push(CoreType::Instant);
        card.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic,
        };
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(kotis, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Kotis attacking P1");

    KotisBatchFixture {
        runner,
        kotis,
        cheap_one,
        cheap_two,
        expensive,
        unrelated_exiled,
    }
}

/// CR 608.2g + CR 202.3: "You may cast ANY NUMBER of spells with mana value X
/// or less FROM AMONG THEM" is a repeating batch grant scoped to this trigger's
/// own exiled batch — not a single pick, and not a licence to cast anything
/// that merely happens to be sitting in exile.
///
/// CR 608.2g states the resolving ability "continues to resolve, which may
/// include casting other spells this way", which is precisely what makes the
/// window re-open after the first cast.
///
/// Three distinct scoping properties are asserted:
///
/// 1. BATCH, not single pick — both in-budget cards from the SAME batch are
///    offered, and after casting one the window re-opens still offering the
///    other, proving the grant is not exhausted by the first cast.
/// 2. BOUNDED by X — the MV 5 card in that same batch is never offered.
/// 3. SCOPED to this source's batch — an in-budget card exiled by an unrelated
///    mechanism is never offered, so "from among them" cannot widen into "any
///    eligible card anywhere in exile".
#[test]
fn kotis_offers_the_whole_in_budget_batch_and_nothing_outside_it() {
    let KotisBatchFixture {
        mut runner,
        kotis,
        cheap_one,
        cheap_two,
        expensive,
        unrelated_exiled,
    } = kotis_batch_scope_fixture();

    let candidates = drain_until_kotis_cast_window(&mut runner);

    let source = kotis_window_source(&runner);
    assert_eq!(
        source, kotis,
        "the window under assertion must be the one Kotis's trigger opened, so the \
         batch the reach guards below read is this trigger's own"
    );

    assert!(
        candidates.contains(&cheap_one),
        "a mana value 1 card (<= X=3) exiled by Kotis must be offered"
    );
    assert!(
        candidates.contains(&cheap_two),
        "a SECOND mana value 3 card (<= X=3) in the SAME batch must be offered too — \
         \"any number\" is a batch grant, not a single pick"
    );
    // REACH GUARD (budget control). `!candidates.contains(&expensive)` is
    // satisfied by ANY failure to reach the offer, including a fixture in which
    // Kotis never exiled the card at all. Prove first that `expensive` actually
    // landed in THIS trigger's exiled batch and is otherwise a perfectly
    // eligible instant sitting in exile, so the ONLY property left that can
    // explain its absence is the mana-value ceiling (CR 202.3: MV 5 > X = 3).
    let state = runner.state();
    assert!(
        tracked_exile_batch(state, source).contains(&expensive),
        "the over-budget control must actually have been exiled by THIS Kotis trigger \
         — otherwise its absence from the offer proves nothing about the \
         mana-value bound; tracked batch was {:?}",
        tracked_exile_batch(state, source)
    );
    assert_eq!(
        state.objects[&expensive].zone,
        Zone::Exile,
        "the over-budget control must still be in exile where the window looks"
    );
    assert!(
        is_instant_or_sorcery(state, expensive),
        "the over-budget control must be an otherwise-castable instant/sorcery, so \
         card type cannot be what excludes it"
    );
    assert!(
        state.objects[&expensive].mana_cost.mana_value() > 3,
        "the over-budget control must genuinely exceed X = 3, or it is not a control \
         for the budget at all"
    );
    assert!(
        !candidates.contains(&expensive),
        "a mana value 5 card (> X=3) must NOT be offered even though it was exiled \
         in the same batch"
    );

    // REACH GUARD (batch-scope control). Symmetrically, prove `unrelated_exiled`
    // is a card the window would have to offer if "from among them" widened to
    // all of exile: still in `Zone::Exile`, an eligible instant, and WITHIN the
    // X = 3 budget — while being absent from this trigger's exiled batch. Batch
    // membership is then the only property that can explain its exclusion.
    assert_eq!(
        state.objects[&unrelated_exiled].zone,
        Zone::Exile,
        "the batch-scope control must still be sitting in exile — if it left, its \
         absence from the offer would say nothing about \"from among them\""
    );
    assert!(
        is_instant_or_sorcery(state, unrelated_exiled),
        "the batch-scope control must be an otherwise-castable instant/sorcery"
    );
    assert!(
        state.objects[&unrelated_exiled].mana_cost.mana_value() <= 3,
        "the batch-scope control must be WITHIN X = 3, so the budget cannot be what \
         excludes it"
    );
    assert!(
        !tracked_exile_batch(state, source).contains(&unrelated_exiled),
        "the batch-scope control must NOT be in this trigger's exiled batch \
         — that is the single property under test"
    );
    assert!(
        !candidates.contains(&unrelated_exiled),
        "a mana value 1 card exiled by an UNRELATED mechanism must NOT be offered — \
         \"from among them\" must not widen to every eligible card sitting in exile"
    );

    // Cast the first one inside the window (CR 608.2g: it goes straight to the
    // stack, no player receives priority).
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(cheap_one),
        })
        .expect("free-casting the first exiled card must succeed");
    assert_eq!(
        runner.state().objects[&cheap_one].zone,
        Zone::Stack,
        "the first chosen card must be cast during the trigger's resolution"
    );

    // The grant must NOT be spent: the same resolution re-offers the remaining
    // in-budget card from the batch, and still nothing outside it.
    let reoffered = drain_until_kotis_cast_window(&mut runner);
    assert!(
        reoffered.contains(&cheap_two),
        "after casting one card the window must re-open still offering the other \
         in-budget card — CR 608.2g: the ability \"continues to resolve, which may \
         include casting other spells this way\""
    );
    assert!(
        !reoffered.contains(&cheap_one),
        "an already-cast card must not be offered a second time"
    );
    // REACH GUARD (re-offer). The re-opened window is a freshly built candidate
    // set, so its negatives need the same proof that both controls are still in
    // the state that makes their exclusion meaningful.
    let reoffer_source = kotis_window_source(&runner);
    assert_eq!(
        reoffer_source, kotis,
        "the re-opened window must still be Kotis's own"
    );
    let state = runner.state();
    assert!(
        tracked_exile_batch(state, reoffer_source).contains(&expensive)
            && state.objects[&expensive].zone == Zone::Exile,
        "the over-budget control must still be an exiled member of this trigger's batch \
         when the window re-opens"
    );
    assert!(
        state.objects[&unrelated_exiled].zone == Zone::Exile
            && is_instant_or_sorcery(state, unrelated_exiled)
            && !tracked_exile_batch(state, reoffer_source).contains(&unrelated_exiled),
        "the batch-scope control must still be an eligible exiled non-member when the \
         window re-opens"
    );
    assert!(
        !reoffered.contains(&expensive) && !reoffered.contains(&unrelated_exiled),
        "the re-opened window must keep the same budget and batch scoping"
    );

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(cheap_two),
        })
        .expect("free-casting the second exiled card must succeed");
    assert_eq!(
        runner.state().objects[&cheap_two].zone,
        Zone::Stack,
        "the second card must also reach the stack, proving BOTH batch members \
         were independently castable through one grant"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "both casts are free (CR 118.9) and must consume no mana"
    );
}
