//! CR 608.2h + CR 113.7a + CR 111.7: an effect's QUANTITY that counts the source's
//! attachments must still be answerable when the source has ceased to exist.
//!
//! Oracle text (Whiplash, Vengeful Engineer — 2/2 Legendary Creature, verbatim from
//! card-data):
//!   "Whiplash enters tapped.
//!    Whenever Whiplash attacks, if he's equipped, each opponent loses X life and you gain
//!    X life, where X is the number of Equipment attached to him."
//!
//! WHY THIS CARD. PR #5792 (t105) repaired the GATE for this class: a
//! `TriggerCondition::SourceMatchesFilter` intervening-if ("if he's equipped") now answers
//! from `LKISnapshot::attachments` when the source is gone. Whiplash was called out there
//! as the known near-miss — its gate opens, but its EFFECT then asks a second, independent
//! question at the QUANTITY layer:
//!
//!   X = ObjectCount { Typed[Subtype(Equipment)] + FilterProp::AttachedToSource }
//!
//! That count enumerates the LIVE battlefield and asks each Equipment "are you attached to
//! the source?" — i.e. `obj.attached_to == Some(source.id)`. SBA unattaches every Equipment
//! the instant its host leaves the battlefield (CR 704.5n), so every candidate answers NO
//! and X resolves to 0. The gate opens onto an effect that does nothing.
//!
//! THE DISCRIMINATING AXIS IS NOT THE CR 111.7 PURGE. Unlike the gate half (where a token's
//! purge from `state.objects` was the whole defect), the quantity half breaks for a NONTOKEN
//! dead source too: the unattachment is done by SBA on ANY battlefield exit, so the Equipment's
//! `attached_to` is cleared whether the host went to the graveyard or ceased to exist. Both
//! legs are witnessed below; both were red.
//!
//! CR 608.2h is the governing rule and it is explicit: "If the effect requires information
//! from a specific object, INCLUDING THE SOURCE OF THE ABILITY ITSELF, the effect uses the
//! current information of that object if it's in the public zone it was expected to be in;
//! if it's no longer in that zone ... the effect uses the object's LAST KNOWN INFORMATION."
//! The source's expected zone is the battlefield. Once it is not there, the attachment set
//! must be read from `LKISnapshot::attachments`, exactly as the gate half already does.
//!
//! This drives the REAL pipeline end to end: the card is synthesized from verbatim Oracle
//! text, Equipment is attached through the engine's `attach::attach_to` authority, the attack
//! trigger fires off the real combat machinery (`declare_attackers`), the source is killed
//! through the real zone-change pipeline (`move_to_zone`, which snapshots LKI) and purged by
//! the real SBA (`check_state_based_actions`), and the trigger resolves off the real stack.
//! The observable is the opponent's life total (and the controller's, for the paired gain).

use engine::game::combat::AttackTarget;
use engine::game::effects::attach;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::game::{sba, zones};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Whiplash, Vengeful Engineer — 2/2. Verbatim Oracle text from card-data.
const WHIPLASH: &str = "Whiplash enters tapped.\nWhenever Whiplash attacks, if he's equipped, each opponent loses X life and you gain X life, where X is the number of Equipment attached to him.";

/// Only a token ceases to exist under CR 111.7. For the GATE half (#5792) that was the
/// discriminating axis; for the QUANTITY half under test here BOTH legs are broken, because
/// SBA unattaches on any battlefield exit (CR 704.5n).
#[derive(Clone, Copy, PartialEq)]
enum SourceKind {
    Token,
    Nontoken,
}

/// Whether the source dies with its attack trigger on the stack, or survives to resolution.
/// `Survives` is the HARNESS POSITIVE CONTROL — it exercises the live path, which must be
/// untouched by this change.
#[derive(Clone, Copy, PartialEq)]
enum SourceFate {
    Survives,
    Dies,
}

/// An Equipment on the battlefield. Built so `capture_attachment_snapshot` (zones.rs)
/// classifies it as `AttachmentKind::Equipment` — it keys off the "Equipment" subtype.
fn make_equipment(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        "Test Equipment".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Equipment".to_string());
    id
}

/// The outcome of one run: life lost by the opponent, and life gained by the controller.
/// Whiplash's two clauses read the SAME X, so asserting both catches a fix that repairs only
/// one of the two `QuantityExpr` sites.
struct Drain {
    opponent_lost: i32,
    controller_gained: i32,
}

/// Put a Whiplash on the battlefield with `attached` Equipment attached to him (and
/// `unattached_decoys` further Equipment on the battlefield attached to NOTHING), attack,
/// optionally kill him while the attack trigger is on the stack, and resolve.
fn whiplash_attacks_then_dies(
    kind: SourceKind,
    attached: usize,
    unattached_decoys: usize,
    fate: SourceFate,
) -> Drain {
    let mut scenario = GameScenario::new();
    // PLACE the scenario at PreCombatMain rather than advancing into it from turn 1: the draw
    // step would deck P0 against an empty library and end the game before combat
    // (`stack_object_keyword_grants.rs` documents the same foot-gun).
    scenario.at_phase(Phase::PreCombatMain);

    let whiplash = scenario
        .add_creature_from_oracle(P0, "Whiplash, Vengeful Engineer", 2, 2, WHIPLASH)
        .id();

    let mut runner = scenario.build();

    // DECOYS FIRST. Equipment that exists on the battlefield but is attached to nothing.
    // Without these, a "count every Equipment on the battlefield" fix — which would be
    // rules-wrong — would pass every assertion below. Their presence is what makes each
    // count a statement about ATTACHMENT rather than about mere existence.
    for _ in 0..unattached_decoys {
        make_equipment(runner.state_mut(), P0);
    }

    let mut equipped = Vec::new();
    for _ in 0..attached {
        let eq = make_equipment(runner.state_mut(), P0);
        // CR 301.5: attach through the engine's own authority, not by hand-editing state.
        attach::attach_to(runner.state_mut(), eq, whiplash);
        equipped.push(eq);
    }

    // CR 111.7 / CR 704.5d: only a token ceases to exist on leaving the battlefield.
    if kind == SourceKind::Token {
        runner
            .state_mut()
            .objects
            .get_mut(&whiplash)
            .expect("the source must exist at setup")
            .is_token = true;
    }

    // PREMISE GUARD: the attachments must actually be established, or every count below is
    // measuring nothing.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&whiplash)
            .expect("whiplash at setup")
            .attachments
            .len(),
        attached,
        "PREMISE: exactly {attached} Equipment must be attached to Whiplash at setup — the \
         attachment set is the single axis under test"
    );

    // "Whiplash enters tapped" — but he is placed directly on the battlefield by the
    // scenario builder, so untap him explicitly; a tapped creature cannot be declared as an
    // attacker (CR 508.1a) and the trigger would never fire at all.
    runner
        .state_mut()
        .objects
        .get_mut(&whiplash)
        .expect("whiplash")
        .tapped = false;

    // CR 508.1: declaring Whiplash as an attacker is what fires the trigger.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(whiplash, AttackTarget::Player(P1))])
        .expect("Whiplash must be able to attack");

    // CR 603.4 (first check): "if he's equipped" gates the trigger at trigger time. With no
    // Equipment attached the ability never goes on the stack at all.
    let expected_stack = usize::from(attached > 0);
    assert_eq!(
        runner.state().stack.len(),
        expected_stack,
        "CR 603.4: the attack trigger must be on the stack exactly when 'if he's equipped' was \
         TRUE at trigger time. If it were absent when equipped, we would be testing the \
         COLLECTION path, not the quantity resolved at RESOLUTION."
    );

    if fate == SourceFate::Dies {
        // Kill Whiplash with his trigger already on the stack, through the REAL zone-change
        // pipeline (which snapshots LKI) and the REAL SBA (which purges a token under
        // CR 111.7 / CR 704.5d and unattaches the Equipment under CR 704.5n).
        let mut events = Vec::new();
        zones::move_to_zone(runner.state_mut(), whiplash, Zone::Graveyard, &mut events);
        sba::check_state_based_actions(runner.state_mut(), &mut events);

        assert!(
            runner.state().lki_cache.contains_key(&whiplash),
            "CR 400.7: battlefield exit must snapshot LKI for the source in both arms"
        );
        match kind {
            SourceKind::Token => assert!(
                !runner.state().objects.contains_key(&whiplash),
                "CR 111.7: the token source must have CEASED TO EXIST — if it is still in \
                 state.objects this leg is vacuous"
            ),
            SourceKind::Nontoken => assert!(
                runner.state().objects.contains_key(&whiplash),
                "a nontoken source stays in state.objects (in the graveyard)"
            ),
        }

        // THE DEFECT, STATED AS A FACT ABOUT THE BOARD. Every Equipment has been unattached
        // by SBA (CR 704.5n). This is what makes the live count read 0 — and it is true for
        // the NONTOKEN leg as well, which is why both legs are red.
        for eq in &equipped {
            assert!(
                runner
                    .state()
                    .objects
                    .get(eq)
                    .expect("the Equipment itself survives its host — it is not a token")
                    .attached_to
                    .is_none(),
                "CR 704.5n: SBA unattaches Equipment the instant its host leaves the \
                 battlefield. The live board therefore CANNOT answer 'how many Equipment are \
                 attached to him' — that is precisely why CR 608.2h routes the question to \
                 LAST KNOWN INFORMATION."
            );
        }
    }

    let life_before_p0 = runner.life(P0);
    let life_before_p1 = runner.life(P1);
    runner.advance_until_stack_empty();

    Drain {
        opponent_lost: life_before_p1 - runner.life(P1),
        controller_gained: runner.life(P0) - life_before_p0,
    }
}

/// HARNESS POSITIVE CONTROL — green before and after.
///
/// A living Whiplash with 2 Equipment drains 2. This is the assertion that makes every zero
/// below meaningful: without it, a probe whose trigger never resolves at all would make the
/// "defect" assertions pass for entirely the wrong reason.
///
/// A third Equipment sits on the battlefield unattached, so `2` also proves the count FILTERED
/// rather than merely counting Equipment.
#[test]
fn premise_living_equipped_whiplash_drains_for_its_equipment_count() {
    let drain = whiplash_attacks_then_dies(SourceKind::Nontoken, 2, 1, SourceFate::Survives);
    assert_eq!(
        drain.opponent_lost, 2,
        "a living Whiplash with 2 Equipment attached (and 1 unattached elsewhere) MUST drain \
         exactly 2. If this is 0 the probe is broken and every other assertion here is void; \
         if it is 3 the count is not filtering on attachment at all."
    );
    assert_eq!(
        drain.controller_gained, 2,
        "the paired 'you gain X life' clause reads the SAME X and must agree"
    );
}

/// CR 603.4 FIRST-CHECK CONTROL — green before and after.
///
/// An UNEQUIPPED Whiplash never triggers: "if he's equipped" is false when the attack event
/// occurs. An Equipment is on the battlefield here (just not attached), so a zero proves the
/// gate ran and rejected him — not that no Equipment existed to find.
#[test]
fn unequipped_whiplash_never_triggers() {
    let drain = whiplash_attacks_then_dies(SourceKind::Nontoken, 0, 1, SourceFate::Survives);
    assert_eq!(
        drain.opponent_lost, 0,
        "CR 603.4: the ability triggers only if 'he's equipped' is true at the trigger event"
    );
}

/// PRIMARY WITNESS — RED before the fix (read 0, must be 2).
///
/// A token copy of Whiplash with 2 Equipment attacks and dies with his trigger on the stack.
/// CR 113.7a: the ability exists on the stack independently of its source. CR 608.2h: the
/// source's information — including the attachment set its own X counts — comes from LAST
/// KNOWN INFORMATION once the source is no longer in the zone it was expected in. He was
/// equipped with 2 Equipment when he last existed, so X = 2.
///
/// Before the fix, `FilterProp::AttachedToSource` asked each LIVE Equipment
/// `attached_to == Some(source)`, SBA had already cleared that field (CR 704.5n), X resolved
/// to 0, and the drain silently did nothing.
#[test]
fn purged_token_source_counts_its_attachments_from_lki() {
    let drain = whiplash_attacks_then_dies(SourceKind::Token, 2, 0, SourceFate::Dies);
    assert_eq!(
        drain.opponent_lost, 2,
        "CR 608.2h: X counts the Equipment attached to the source as it LAST EXISTED — 2"
    );
    assert_eq!(
        drain.controller_gained, 2,
        "the paired 'you gain X life' clause reads the SAME X and must agree"
    );
}

/// SECOND WITNESS — the NONTOKEN dead source. RED before the fix (read 0, must be 2).
///
/// This leg is the one the task charter did not predict. The printed card dies the same way
/// and STAYS in `state.objects` (graveyard), so — unlike the gate half repaired by #5792 —
/// the subject was never invisible. But SBA unattached its Equipment all the same
/// (CR 704.5n), so the live board could not answer the count either. The quantity half is
/// therefore broken on BOTH legs, and the CR 111.7 purge is NOT its discriminating axis.
#[test]
fn nontoken_dead_source_counts_its_attachments_from_lki() {
    let drain = whiplash_attacks_then_dies(SourceKind::Nontoken, 2, 0, SourceFate::Dies);
    assert_eq!(
        drain.opponent_lost, 2,
        "CR 608.2h + CR 704.5n: the Equipment is unattached the instant the host leaves, so \
         'how many Equipment are attached to him' must be answered from LAST KNOWN \
         INFORMATION even for a source that is merely dead rather than purged"
    );
    assert_eq!(
        drain.controller_gained, 2,
        "the paired 'you gain X life' clause reads the SAME X and must agree"
    );
}

/// DISCRIMINATING CONTROL — the fix must COUNT, not FABRICATE.
///
/// This is the arm that proves the LKI fallback restores the source's ability to ANSWER the
/// question honestly rather than to answer "all the Equipment on the board".
///
/// A purged token Whiplash had exactly ONE Equipment attached; TWO more sit on the battlefield
/// attached to nothing. X must be exactly 1.
///
/// * A fail-closed count (the defect) reads 0 — fails.
/// * A count that reads the LKI snapshot but ignores the filter reads 1 — passes, correctly.
/// * A count that enumerates every Equipment on the battlefield reads 3 — fails.
///
/// Without this arm, the primary witness alone could be satisfied by a fix that simply counted
/// all Equipment, which happens to equal 2 there.
#[test]
fn purged_token_source_counts_only_its_own_attachments_not_every_equipment() {
    let drain = whiplash_attacks_then_dies(SourceKind::Token, 1, 2, SourceFate::Dies);
    assert_eq!(
        drain.opponent_lost, 1,
        "X is the number of Equipment attached to HIM — 1 — not the number of Equipment in \
         existence (3), and not the fail-closed 0. The LKI look-back must restore the ability \
         to COUNT, not the licence to over-count."
    );
    assert_eq!(
        drain.controller_gained, 1,
        "the paired 'you gain X life' clause reads the SAME X and must agree"
    );
}
