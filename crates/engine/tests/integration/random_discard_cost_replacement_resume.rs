//! End-to-end coverage for the RANDOM unless-discard replacement continuation
//! (`PendingCostMoveResume::RandomDiscardUnlessPayment`).
//!
//! A random unless-discard pays inline with no prompt, so unlike its
//! player-chosen sibling it has no `WardDiscardChoice` re-prompt loop to own the
//! remainder. If a replacement effect interrupts the batch, TWO things live only
//! in the paying stack frame that returns: the unless-payment itself (which must
//! still be settled, or the guarded ability is left neither paid nor unpaid at
//! bare priority) and the batch cursor (the picks still owed).
//!
//! REACHABILITY — worth stating, because it determines what a valid fixture is.
//! After the `DiscardCause` split a COST discard can no longer pause at the
//! `Discard` replacement gate: the corpus's only two `ReplacementEvent::Discard`
//! definitions are the Library of Leng class (`EffectCausedDiscard`, correctly
//! excluded for costs) and the Dodecapod class (not `Optional`, so it raises no
//! choice). The pause survives only at the SECOND gate — the hand→graveyard
//! `Moved` replacement inside `complete_discard_to_graveyard`, which is not
//! gated on `caused_by_effect`. So these fixtures use a graveyard-redirect
//! replacement (Rest in Peace class), made `Optional` so it raises the choice.
//!
//! CR ANCHORS:
//!   * CR 616.1 — the affected player chooses which applicable replacement to
//!     apply; that choice is what parks the batch.
//!   * CR 701.9a/b — discard, and random discard specifically.
//!   * CR 118.12a — the "unless" construction; declining ≡ the effect happens.
//!   * CR 118.3 — a cost cannot be paid partially.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Balduvian Horde's printed Oracle text — a one-card random unless-discard.
const BALDUVIAN_HORDE: &str =
    "When this creature enters, sacrifice it unless you discard a card at random.";

/// Rest in Peace class, made OPTIONAL so it surfaces an Accept/Decline choice
/// instead of applying silently. Watches other cards (`valid_card: None`) moving
/// to the graveyard from anywhere, and exiles them instead.
fn optional_graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

/// P0 casts Balduvian Horde with `hand_size` other cards in hand, and a
/// battlefield permanent hosting the optional graveyard-redirect replacement.
fn setup(hand_size: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Red,
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );

    // The replacement host. On P1 so it cannot be confused with the Horde.
    scenario
        .add_creature(P1, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement());

    let horde = scenario
        .add_creature_to_hand_from_oracle(P0, "Balduvian Horde", 5, 5, BALDUVIAN_HORDE)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 2,
            shards: vec![
                engine::types::mana::ManaCostShard::Red,
                engine::types::mana::ManaCostShard::Red,
            ],
        })
        .id();

    let hand: Vec<ObjectId> = (0..hand_size)
        .map(|i| scenario.add_card_to_hand(P0, &format!("Filler Card {i}")))
        .collect();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.cast(horde).resolve();
    (runner, horde, hand)
}

fn advance_to_unless_prompt(runner: &mut GameRunner) {
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    panic!(
        "the ETB trigger never surfaced an unless-payment prompt: {:?}",
        runner.state().waiting_for
    );
}

fn moved_out_of_hand(runner: &GameRunner, hand: &[ObjectId]) -> usize {
    hand.iter()
        .filter(|id| runner.state().objects[id].zone != Zone::Hand)
        .count()
}

/// CR 616.1 + CR 118.12: the batch parks on the replacement choice, and
/// ACCEPTING it redirects the filler to exile and resumes the preserved
/// payment: the cost counts as paid, the
/// guarded unless-effect (sacrifice) does NOT happen, and no cost continuation
/// is left parked.
///
/// This is the discriminating case for `PendingCostMoveResume::
/// RandomDiscardUnlessPayment`. Without the persisted continuation the drain has
/// no owner able to call `finish_unless_payment`, and the Horde is left neither
/// sacrificed nor kept.
#[test]
fn random_discard_cost_resumes_its_payment_after_an_accepted_replacement() {
    let (mut runner, horde, hand) = setup(3);
    advance_to_unless_prompt(&mut runner);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the random discard must be accepted");

    // The batch parked on the graveyard-redirect choice rather than completing.
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the random discard to park on a ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        runner.state().pending_cost_move_resume.is_some(),
        "the unless-payment continuation must be persisted while the choice is open"
    );
    // NEGATIVE, paired with the positive reach guard directly above: the EFFECT
    // layer's batch carrier must stay empty for a COST payment. The assertion is
    // non-vacuous precisely because the line above proves a random discard batch
    // really did pause here — only the layer differs.
    assert!(
        runner.state().pending_discard_batch.is_none(),
        "a cost-layer random discard must not park an EFFECT batch (DiscardCause::Cost)"
    );
    let accept_idx = candidates
        .iter()
        .position(|c| c.description == "Accept")
        .expect("an Accept option");

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: accept_idx })
        .expect("accepting the redirect must be accepted");

    // CR 118.12: the resume must settle through the PAID epilogue, not the
    // decline tail. `EffectResolved` is emitted only by
    // `finish_successful_unless_payment` — which also runs the `IfAPlayerDoes`
    // alternative-outcome sub and the `SequentialSibling` chain. Routing a
    // successful resume through `finish_unless_payment` skips all three, and
    // Balduvian Horde's body is too simple to notice, so this event is the
    // discriminator that does.
    assert!(
        resumed
            .events
            .iter()
            .any(|e| matches!(e, engine::types::events::GameEvent::EffectResolved { .. })),
        "the paid epilogue must run on resume (EffectResolved), got {:?}",
        resumed.events
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        moved_out_of_hand(&runner, &hand),
        1,
        "exactly one card left the hand as the payment"
    );
    let moved_filler = hand
        .iter()
        .find(|id| runner.state().objects[id].zone != Zone::Hand)
        .expect("exactly one filler must have moved");
    assert_eq!(
        runner.state().objects[moved_filler].zone,
        Zone::Exile,
        "accepting the replacement must redirect the paid filler to exile"
    );
    assert_eq!(
        runner.state().objects[&horde].zone,
        Zone::Battlefield,
        "the resumed payment counts as paid, so the Horde is NOT sacrificed"
    );
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "the continuation must be drained, not left parked"
    );
}

/// CR 616.1: DECLINING the optional replacement lets the natural hand→graveyard
/// move happen. That is still a delivered discard, so the payment resumes
/// identically — the same continuation must own both branches of the choice.
#[test]
fn random_discard_cost_resumes_its_payment_after_a_declined_replacement() {
    let (mut runner, horde, hand) = setup(3);
    advance_to_unless_prompt(&mut runner);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the random discard must be accepted");

    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let decline_idx = candidates
        .iter()
        .position(|c| c.description == "Decline")
        .expect("a Decline option");

    runner
        .act(GameAction::ChooseReplacement { index: decline_idx })
        .expect("declining the redirect must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        moved_out_of_hand(&runner, &hand),
        1,
        "the card still leaves the hand — declining the redirect sends it to the graveyard"
    );
    let moved_filler = hand
        .iter()
        .find(|id| runner.state().objects[id].zone != Zone::Hand)
        .expect("exactly one filler must have moved");
    assert_eq!(
        runner.state().objects[moved_filler].zone,
        Zone::Graveyard,
        "declining the replacement must send the paid filler to the graveyard"
    );
    assert_eq!(
        runner.state().objects[&horde].zone,
        Zone::Battlefield,
        "a declined redirect is still a completed discard, so the cost is paid"
    );
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "the continuation must be drained on the decline branch too"
    );
}

/// CR 118.12a + CR 118.3: REACH-GUARD. With an empty hand the cost is unpayable,
/// so the guarded effect happens and the Horde is sacrificed — proving the two
/// tests above are not passing merely because the Horde survives by default.
///
/// Note the fixture's own second-order effect: the sacrifice moves the Horde to
/// the graveyard, which trips the SAME optional redirect the payment did. That
/// choice is answered here (decline) so the sacrifice completes naturally. It is
/// a distinct choice from the payment's — the payment never started, so no
/// unless-payment continuation is ever parked, which is the other half of this
/// guard.
#[test]
fn random_discard_cost_with_no_cards_still_sacrifices() {
    let (mut runner, horde, _hand) = setup(0);
    advance_to_unless_prompt(&mut runner);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting an unpayable cost must be accepted");
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "an unpayable cost never begins, so nothing may be parked"
    );

    // The sacrifice's own graveyard move offers the redirect; decline it so the
    // Horde lands in the graveyard rather than exile.
    if let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone() {
        let decline_idx = candidates
            .iter()
            .position(|c| c.description == "Decline")
            .expect("a Decline option on the sacrifice's graveyard move");
        runner
            .act(GameAction::ChooseReplacement { index: decline_idx })
            .expect("declining the redirect must be accepted");
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&horde].zone,
        Zone::Graveyard,
        "an unpayable random discard sacrifices the Horde"
    );
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "nothing may be left parked"
    );
}

/// Hymn to Tourach's printed Oracle text (Scryfall, verified verbatim
/// 2026-08-16) — the EFFECT layer's multi-pick random discard, and the only
/// selection mode besides the forced whole-hand branch that can lose cards to a
/// mid-batch pause.
const HYMN_TO_TOURACH: &str = "Target player discards two cards at random.";

/// CR 701.9b + CR 608.2c: an EFFECT-caused random discard that pauses on its
/// first pick must still make its second one.
///
/// This is the random sibling of the forced whole-hand arm in
/// `windfall_greatest_discard_aggregate.rs`. The gate-2 `Moved` redirect fires
/// on every VICTIM card, so the batch pauses on pick 1, resumes, pauses again on
/// pick 2, and resumes — two prompts, two cards gone.
///
/// FIXTURE NOTE (measured, not assumed). The redirect is narrowed with
/// `Not { SpecificObject { id: hymn } }` rather than left at `valid_card: None`.
/// An unnarrowed redirect also watches the SPELL's own CR 608.2n stack →
/// graveyard move, and that move happens while the first pick's choice is still
/// parked: `pending_replacement` is a single slot, so the spell's move overwrote
/// the victim's parked choice and the victim never left the hand. Measured on
/// this fixture at this tip — prompt #0's `pending_replacement` was
/// `ZoneChange { object_id: <Hymn>, from: Stack, to: Graveyard }`, and the
/// first-picked victim stayed in `Zone::Hand` for the rest of the game. The
/// overwrite was already in place at prompt #0, i.e. before any resume code
/// runs, so it is a single-slot `pending_replacement` defect independent of the
/// batch cursor this test covers. It is NOT repaired here; the fixture excludes
/// the spell instead of silently absorbing it.
///
/// Discriminating: at the pre-fix tip the effect layer threw the returned cursor
/// away, so exactly ONE card left the hand and exactly ONE prompt was raised.
/// The prompt count is the reach guard — a run that raised no prompt never
/// exercised the pause path and could satisfy a bare zone check for the wrong
/// reason.
#[test]
fn effect_random_discard_finishes_its_batch_after_a_replacement_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let hymn = scenario
        .add_spell_to_hand_from_oracle(P0, "Hymn to Tourach", false, HYMN_TO_TOURACH)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    scenario
        .add_creature(P1, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement().valid_card(
            TargetFilter::Not {
                filter: Box::new(TargetFilter::SpecificObject { id: hymn }),
            },
        ));
    let hand: Vec<ObjectId> = (0..4)
        .map(|i| scenario.add_card_to_hand(P1, &format!("Victim Card {i}")))
        .collect();
    let mut runner = scenario.build();

    runner.cast(hymn).target_player(P1).resolve();

    let mut prompts = 0;
    for _ in 0..16 {
        let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
        else {
            break;
        };
        let decline = candidates
            .iter()
            .position(|c| c.description == "Decline")
            .expect("a Decline option");
        prompts += 1;
        runner
            .act(GameAction::ChooseReplacement { index: decline })
            .expect("declining the redirect must be accepted");
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        (prompts, moved_out_of_hand(&runner, &hand)),
        (2, 2),
        "both random picks must be made across the pause (prompts, cards gone)"
    );
}
