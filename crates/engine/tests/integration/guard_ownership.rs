//! Unlowerable leading `"if <guard>,"` guards: the body gaps honestly unless a typed
//! owner on the assembled tree consumes it in the dropped guard's stead.
//!
//! CR 614.1 / CR 614.1a (the EVENT reading — "would") vs CR 608.2c (the STATE reading)
//! decide whether a gap is recorded at all: only the EVENT reading gaps. Two owner classes
//! then keep an EVENT-guarded body alive:
//!
//! - **O1a** — CR 614.1a + CR 608.2n: a graveyard-redirect rider that is the direct
//!   `sub_ability` of an `Effect::CastFromZone`. Corpus-backed: V3/V4/V5 are verbatim
//!   Torrential Gearhulk / Mission Briefing / Power Pack.
//! - **O1b** — CR 608.2c + CR 614.1a: an exile rider that is the direct `sub_ability`
//!   of an `Effect::Counter`. **A FORWARD GUARD WITH NO CORPUS MEMBER, and the arm's own
//!   comment naming Force of Negation and No More Lies does not make it one.** Both print
//!   `"If that spell is countered this way, exile it instead of putting it into its owner's
//!   graveyard."` — no "would", so `condition_names_an_event` classifies them STATE, no mark
//!   is minted and the O1b arm is never reached. Measured over the whole corpus, not inferred
//!   from those two: no card pairing a counter with a sentence-leading `"if … would …"` guard
//!   exists in `data/mtgjson/AtomicCards.json`. V6 (Delay) and V7n (Remand) are declared
//!   CONTROLS for the same reason. The arm's only discriminator is therefore the declared
//!   synthetic row **V7s**, on the same footing as the declared-synthetic rows V17 and
//!   `oracle_effect::tests::an_o1a_rider_shape_under_a_state_guard_is_not_an_ownership_candidate`.
//!
//! O1b is NOT deleted the way O2 was, and the distinction is the point: O2 was
//! LOGICALLY unreachable — its two conjuncts were mutually exclusive, so no input of any
//! kind could reach it. O1b is reachable by any input of the shape V7s pins; it is only the
//! printed corpus that has no member today.
//!
//! A third class, **O2** (CR 615.5, a "prevented this way" follow-up under an
//! `Effect::PreventDamage` ancestor), was deleted as dead: a back-reference rider reads
//! STATE by construction, so it never reached an owner test. V8 and V9 stay as CONTROLS —
//! their riders survive because a STATE guard does not gap, which is the base behaviour O2
//! was never needed to produce. See the withdrawal note beside
//! `parser::oracle_effect::tests`' V10c.
//!
//! Every row here runs through `parse_oracle_text`, because the ownership verdict is
//! settled at the tail of the parse pipeline — after line routing — and no
//! chain-level entry point reaches it. The venue-S and venue-C companions of these
//! rows live in `parser/oracle_effect/tests.rs`.
//!
//! Row V15 ("an owned body keeps its spell route") has no test of its own by design:
//! it is carried by `oracle_tests.rs::prevention_followup_if_this_way_does_not_emit_condition_warning`
//! staying green unchanged.

use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Effect, SpellStackToGraveyardReplacement, SubAbilityLink,
    TargetFilter,
};
use engine::types::ability_visit::{
    visit_ability_def, visit_replacement, visit_static, visit_trigger,
};
use engine::types::zones::Zone;
use std::ops::ControlFlow;

// ---------------------------------------------------------------------------
// Fixtures. Every `real` fixture is the card's verbatim Oracle text.
// ---------------------------------------------------------------------------

// V1 (Ajani's Aid) is WITHDRAWN — see the "V1 — WITHDRAWN" block below, where the row
// stood. Its fixture const is deleted with it.

/// V3 — Torrential Gearhulk, verbatim.
const TORRENTIAL_GEARHULK: &str = "Flash\nWhen this creature enters, you may cast target instant \
card from your graveyard without paying its mana cost. If that spell would be put into your \
graveyard, exile it instead.";

/// V4 — Mission Briefing, verbatim.
const MISSION_BRIEFING: &str = "Surveil 2, then choose an instant or sorcery card in your \
graveyard. You may cast it this turn. If that spell would be put into your graveyard, exile it \
instead. (To surveil 2, look at the top two cards of your library, then put any number of them \
into your graveyard and the rest on top of your library in any order.)";

/// V5 — Power Pack, verbatim.
const POWER_PACK: &str = "Flying, vigilance, trample, haste\nWhenever Power Pack deals combat \
damage to a player, exile target instant or sorcery card from your graveyard chosen at random. \
At the beginning of your next upkeep, you may cast that card without paying its mana cost. If \
that spell would be put into your graveyard, exile it instead.";

/// V6 — Delay, verbatim.
const DELAY: &str = "Counter target spell. If the spell is countered this way, exile it with \
three time counters on it instead of putting it into its owner's graveyard. If it doesn't have \
suspend, it gains suspend. (At the beginning of its owner's upkeep, they remove a time counter. \
When the last is removed, they may play it without paying its mana cost. If it's a creature, it \
has haste.)";

/// V7n — Remand, verbatim.
const REMAND: &str = "Counter target spell. If that spell is countered this way, put it into its \
owner's hand instead of into that player's graveyard.\nDraw a card.";

/// V7s — **DECLARED SYNTHETIC, two real donors, one composed pairing.** The head is the
/// printed first sentence of Delay / Force of Negation / No More Lies ("Counter target
/// spell."); the rider is Torrential Gearhulk's printed rider sentence, verbatim. Only the
/// pairing is composed, and it is composed deliberately: no printed card pairs a counter with
/// a sentence-leading EVENT ("would") guard, which is precisely why O1b needs a row of its own
/// (see this module's header).
const SYNTHETIC_EVENT_GUARDED_COUNTER_RIDER: &str = "Counter target spell. If that spell would \
be put into your graveyard, exile it instead.";

/// V8 — Deflecting Palm, verbatim.
const DEFLECTING_PALM: &str = "The next time a source of your choice would deal damage to you \
this turn, prevent that damage. If damage is prevented this way, Deflecting Palm deals that \
much damage to that source's controller.";

/// V9 — Comeuppance, verbatim.
const COMEUPPANCE: &str = "Prevent all damage that would be dealt to you and planeswalkers you \
control this turn by sources you don't control. If damage from a creature source is prevented \
this way, Comeuppance deals that much damage to that creature. If damage from a noncreature \
source is prevented this way, Comeuppance deals that much damage to the source's controller.";

// V10's fixture const is deleted with the row (see the "V10 — WITHDRAWN" block below).
// The venue-C companion V10c carries its own copy of the same synthetic text.

// V11's fixture const is deleted with the row (see the "V11 — WITHDRAWN" block below).
// The claim's surviving venue-S row carries its own copy of the guard sentence.

/// V12 — Hallowed Moonlight, verbatim.
const HALLOWED_MOONLIGHT: &str =
    "Until end of turn, if a creature would enter and it wasn't cast, exile it instead.\nDraw a \
card.";

/// V13p — Phyrexian Vindicator, verbatim.
const PHYREXIAN_VINDICATOR: &str = "Flying\nIf damage would be dealt to this creature, prevent \
that damage. When damage is prevented this way, this creature deals that much damage to any \
other target.";

/// V17 — declared synthetic, two real donors, one composed pairing.
///
/// Sentence 1 is the hostile context (`"Draw a card."` standing where an owner head would
/// normally be), the same device the venue-C rows use. Sentences 2 and 3 are Torrential
/// Gearhulk's and Kylox's Voltstrider's printed riders verbatim — two members of the same
/// `graveyard_destination_rider` class, so each is an ownership CANDIDATE and each carries
/// the EVENT reading ("would"). Only the stacking is composed: no corpus face prints two
/// such riders in sequence under a head that owns neither, which is exactly why the row
/// exists.
const STACKED_UNOWNED_RIDERS: &str = "Draw a card. If that spell would be put into your \
graveyard, exile it instead. If that spell would be put into a graveyard, put it on the \
bottom of its owner's library instead.";

/// V14 — Torch the Tower, verbatim, all three lines.
const TORCH_THE_TOWER: &str = "Bargain (You may sacrifice an artifact, enchantment, or token as \
you cast this spell.)\nTorch the Tower deals 2 damage to target creature or planeswalker. If \
this spell was bargained, instead it deals 3 damage to that permanent and you scry 1.\nIf a \
permanent dealt damage by Torch the Tower would die this turn, exile it instead.";

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn parse(text: &str, card_name: &str, keywords: &[&str], types: &[&str]) -> ParsedAbilities {
    let keywords: Vec<String> = keywords.iter().map(|k| (*k).to_string()).collect();
    let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
    parse_oracle_text(text, card_name, &keywords, &types, &[])
}

/// Every `Effect` reachable from a finished parse, through the engine's own complete
/// traversal rather than a bespoke walk.
fn all_effects(parsed: &ParsedAbilities) -> Vec<Effect> {
    let mut collected = Vec::new();
    let mut visit = |effect: &Effect| {
        collected.push(effect.clone());
        ControlFlow::Continue(())
    };
    for def in &parsed.abilities {
        let _ = visit_ability_def(def, &mut visit);
    }
    for trigger in &parsed.triggers {
        let _ = visit_trigger(trigger, &mut visit);
    }
    for static_def in &parsed.statics {
        let _ = visit_static(static_def, &mut visit);
    }
    for replacement in &parsed.replacements {
        let _ = visit_replacement(replacement, &mut visit);
    }
    collected
}

/// `(name, description)` of every `Effect::Unimplemented` in the tree.
fn gaps(parsed: &ParsedAbilities) -> Vec<(String, Option<String>)> {
    all_effects(parsed)
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Unimplemented { name, description } => Some((name, description)),
            _ => None,
        })
        .collect()
}

fn gap_names(parsed: &ParsedAbilities) -> Vec<String> {
    gaps(parsed).into_iter().map(|(name, _)| name).collect()
}

/// The library-redirect member of the same `graveyard_destination_rider` class: "put the
/// parent's target at a library position".
fn is_library_parent_target_rider(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::PutAtLibraryPosition {
            target: TargetFilter::ParentTarget,
            ..
        }
    )
}

/// The O1a/O1b rider shape: "exile the parent's target".
fn is_exile_parent_target_rider(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::ParentTarget,
            ..
        }
    )
}

fn has_exile_parent_target_rider(parsed: &ParsedAbilities) -> bool {
    all_effects(parsed).iter().any(is_exile_parent_target_rider)
}

/// V16, asserted on every venue-P and venue-R fixture: the deferred verdict is parser
/// scratch, so no tree a finished parse hands out may still carry one. Asked of the
/// serialized tree — the same predicate the corpus-wide gate applies to
/// `card-data.json` — so it cannot be satisfied by a walk that simply fails to look.
///
/// Deliberately NOT applied to the venue-C/S rows: there the resolver has not run yet and an
/// ownership candidate is *supposed* to carry a live mark. The row that used to be named here
/// as the example — V10c,
/// `parser::oracle_effect::tests::o2_rider_clause_is_marked_by_the_seam_even_with_no_shield` —
/// is WITHDRAWN with the O2 apparatus, and its former sibling V11c with the reading-aware
/// candidacy rule (a STATE guard over an O1a shape is no longer a candidate, so there is no
/// mark to assert). The surviving positive is
/// `parser::oracle_effect::tests::v17s_both_stacked_riders_are_ownership_candidates`.
///
/// That is the only test in the tree that asserts a mark is MINTED. Everywhere else the
/// positive is carried behaviourally: V3 and V5 go red if the seam stops minting, because an
/// unmarked EVENT clause is gapped on the spot by `lower_clause_ast` and their riders vanish.
/// Stated here rather than left implicit, because this predicate is the negative half and a
/// reader checking "who proves a mark is ever created?" arrives at this comment first.
fn assert_no_live_guard_mark(parsed: &ParsedAbilities, row: &str) {
    fn holds(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key("unlowered_guard") || map.values().any(holds)
            }
            serde_json::Value::Array(items) => items.iter().any(holds),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => false,
        }
    }
    let value = serde_json::to_value(parsed).expect("ParsedAbilities serializes");
    assert!(
        !holds(&value),
        "{row}: a deferred guard verdict survived the parse pipeline"
    );
}

/// Walk a definition's own `sub_ability` chain (CR 608.2c, "in the order written").
fn chain(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut out = Vec::new();
    let mut cursor = Some(def);
    while let Some(node) = cursor {
        out.push(node);
        cursor = node.sub_ability.as_deref();
    }
    out
}

// ---------------------------------------------------------------------------
// V1 — WITHDRAWN. Wrong on the card, and redundant with V13.
//
// V1 asserted that Ajani's Aid's `"If you search your library this way, shuffle."` gaps
// with its body emitted nowhere. Both halves are wrong:
//
//   * The clause is a STATE guard (CR 608.2c — no "would"), and under the amended rule
//     only the EVENT reading gaps, so it falls through to base behaviour by design.
//   * The guard is VACUOUS, not merely unowned: its truth condition is entailed by the
//     effect it gates. `Effect::SearchLibrary`'s own doc says the trailing sentence "is
//     the effect's own Shuffle sub-ability and is always reached in the multi-zone case
//     because Library is in the set", and `swallow_check`'s escape for it says "the
//     engine's SearchLibrary effect auto-shuffles, so the 'if' gates nothing". Gapping it
//     would DELETE a correctly-represented instruction, and two pre-existing rows assert
//     that `Shuffle` on this very sentence:
//     `oracle_effect::tests::hunger_tide_rises_chapter_iv_sacrifice_search_put_chain` and
//     `oracle_effect::tests::claim_jumper_parses_repeat_once_while_opponent_lands`.
//
// The claim V1 was meant to carry — a non-candidate guard gaps at the seam with no body
// emitted — is true of the EVENT reading, and V13 already carries exactly that claim at
// venue C on a measured corpus guard body.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// V3 / V4 / V5 — O1a.
// ---------------------------------------------------------------------------

/// V3. CR 614.1a + CR 608.2n: the graveyard-redirect rider is the direct `sub_ability`
/// of the `CastFromZone` that grants the cast, so the `CastFromZone` consumes it in the
/// dropped guard's stead and the tree is untouched.
#[test]
fn v3_o1a_rider_under_its_cast_from_zone_owner_survives() {
    let parsed = parse(
        TORRENTIAL_GEARHULK,
        "Torrential Gearhulk",
        &["Flash"],
        &["Creature"],
    );

    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("the ETB trigger must carry a payload");
    // Reach-guard: the owner relation itself.
    assert!(
        matches!(*execute.effect, Effect::CastFromZone { .. }),
        "V3 reach-guard: the head must be CastFromZone, got {:?}",
        execute.effect
    );
    let rider = execute
        .sub_ability
        .as_deref()
        .expect("V3 reach-guard: the rider must be the head's direct sub_ability");
    assert!(
        is_exile_parent_target_rider(&rider.effect),
        "V3: the rider must survive as ChangeZone {{ Exile, ParentTarget }}, got {:?}",
        rider.effect
    );
    assert!(
        gaps(&parsed).is_empty(),
        "V3: an owned body gaps nowhere, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V3");
}

/// V4. The same body, the same guard, under a head that consumes nothing: Mission
/// Briefing's rider hangs below `GrantCastingPermission`, which carries no redirect.
/// Owning by *shape* rather than by *parent* would keep it.
#[test]
fn v4_o1a_body_under_a_non_consuming_head_gaps() {
    let parsed = parse(MISSION_BRIEFING, "Mission Briefing", &[], &["Sorcery"]);

    // Reach-guard: the chain assembled far enough to reach the non-consuming head.
    assert!(
        all_effects(&parsed)
            .iter()
            .any(|effect| matches!(effect, Effect::GrantCastingPermission { .. })),
        "V4 reach-guard: the GrantCastingPermission head must be present"
    );
    assert!(
        gap_names(&parsed).contains(&"unparsed_replacement".to_string()),
        "V4: the EVENT guard's body must be recorded as an unparsed_replacement, got {:?}",
        gaps(&parsed)
    );
    assert!(
        !has_exile_parent_target_rider(&parsed),
        "V4: the rider must be emitted nowhere — it has no owner here"
    );
    assert_no_live_guard_mark(&parsed, "V4");
}

/// V5. CR 603.7a: Power Pack's owner sits inside an `Effect::CreateDelayedTrigger`.
/// A resolver that stops at the delayed-trigger boundary, or one that reads "the
/// previous clause" instead of the assembled parent, gaps a rider that is owned.
#[test]
fn v5_o1a_owner_inside_a_delayed_trigger_is_found() {
    let parsed = parse(
        POWER_PACK,
        "Power Pack",
        &["Flying", "Vigilance", "Trample", "Haste"],
        &["Creature"],
    );

    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("the combat-damage trigger must carry a payload");
    let delayed = chain(execute)
        .into_iter()
        .find_map(|node| match &*node.effect {
            Effect::CreateDelayedTrigger { effect, .. } => Some(effect.as_ref()),
            _ => None,
        })
        .expect("V5 reach-guard: the delayed-trigger wrapper must be present");
    assert!(
        matches!(*delayed.effect, Effect::CastFromZone { .. }),
        "V5 reach-guard: the delayed payload's head must be CastFromZone, got {:?}",
        delayed.effect
    );
    let rider = delayed
        .sub_ability
        .as_deref()
        .expect("V5: the rider must be the delayed head's direct sub_ability");
    assert!(
        is_exile_parent_target_rider(&rider.effect),
        "V5: the rider must survive intact, got {:?}",
        rider.effect
    );
    assert!(
        gaps(&parsed).is_empty(),
        "V5: an owned body gaps nowhere, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V5");
}

// ---------------------------------------------------------------------------
// V6 / V7n — O1b.
// ---------------------------------------------------------------------------

/// V6 — a **control**, not an O1b discriminator. CR 608.2c + CR 614.1a: Delay's
/// back-reference guard ("if the spell is countered this way") carries no "would", so it
/// reads STATE.
///
/// The rider therefore survives by FALL-THROUGH, and ownership is never consulted for it —
/// the same correction V8 and V9 carry. A STATE guard is not an ownership candidate
/// (`oracle_effect::is_ownership_candidate`'s first conjunct is `reading == Event`), so no
/// mark is minted at the clause seam at all; `oracle::guard_owner`'s O1b arm is not reached,
/// and `oracle.rs`'s `reading == GuardReading::Event` conjunct would short-circuit ahead of
/// it even if one were. Deleting the O1b arm leaves this row green.
///
/// It stays because it pins the fall-through itself: a future widening of the EVENT reading
/// that swept in "countered this way" would gap Delay's rider, and this row would say so.
/// The O1b arm's own discriminator is `v7s_*` below.
#[test]
fn v6_o1b_exile_rider_under_counter_survives() {
    let parsed = parse(DELAY, "Delay", &[], &["Instant"]);

    let head = &parsed.abilities[0];
    // Reach-guard: the owner relation itself.
    assert!(
        matches!(*head.effect, Effect::Counter { .. }),
        "V6 reach-guard: the head must be Counter, got {:?}",
        head.effect
    );
    let rider = head
        .sub_ability
        .as_deref()
        .expect("V6 reach-guard: the rider must be the head's direct sub_ability");
    assert!(
        is_exile_parent_target_rider(&rider.effect),
        "V6: the exile rider must survive, got {:?}",
        rider.effect
    );
    assert!(
        gaps(&parsed).is_empty(),
        "V6: an owned body gaps nowhere, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V6");
}

/// V7n — a **control**, not a discriminator.
///
/// The O1b arm is exile-only, and this row pins the structural reason: the corpus's
/// non-exile counter redirects ride `Effect::Counter { countered_spell_zone }` and mint
/// no rider sub-ability at all, so exile is the only rider destination the O1b arm can
/// ever be asked about.
///
/// A red side here is a **finding that re-opens a withdrawn row**, not a bug in this
/// phase: earlier rounds proposed a hostile built on invented counter-redirect text,
/// and it was withdrawn precisely because the corpus does not print that shape. If a
/// parser change starts producing a non-exile counter *rider*, this test catches it
/// here rather than letting `guard_owner` silently widen.
#[test]
fn v7n_non_exile_counter_redirect_rides_the_head_and_mints_no_rider() {
    let parsed = parse(REMAND, "Remand", &[], &["Instant"]);

    let head = &parsed.abilities[0];
    assert!(
        matches!(
            *head.effect,
            Effect::Counter {
                countered_spell_zone: Some(SpellStackToGraveyardReplacement::Hand),
                ..
            }
        ),
        "V7n reach-guard: the redirect must ride the Counter head itself, got {:?}",
        head.effect
    );
    let draw = head
        .sub_ability
        .as_deref()
        .expect("V7n: Remand's second line must fold in as a sibling");
    assert!(
        matches!(*draw.effect, Effect::Draw { .. }),
        "V7n: the Counter's sub_ability is the printed draw, got {:?}",
        draw.effect
    );
    assert_eq!(
        draw.sub_link,
        SubAbilityLink::SequentialSibling,
        "V7n: the draw is the next printed instruction, not a continuation of the counter"
    );
    assert!(
        !all_effects(&parsed).iter().any(|effect| matches!(
            effect,
            Effect::ChangeZone {
                target: TargetFilter::ParentTarget,
                ..
            } | Effect::PutAtLibraryPosition {
                target: TargetFilter::ParentTarget,
                ..
            }
        )),
        "V7n: a non-exile counter redirect must mint no rider sub-ability"
    );
    assert!(
        gaps(&parsed).is_empty(),
        "V7n: Remand gaps nowhere, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V7n");
}

/// V7s — **the O1b arm's only discriminator.** CR 608.2c + CR 614.1a: an EVENT-reading guard
/// over an exile rider whose direct parent is the `Effect::Counter` must NOT gap.
///
/// V6 and V7n cannot carry this claim and are declared controls: both print back-reference
/// guards ("… is countered this way"), which carry no "would", read STATE, and so are never
/// ownership candidates — the O1b arm is not reached on either. Deleting the arm leaves both
/// green. This row is the one that goes red.
///
/// **Revert-to-red:** replace the `Some(Effect::Counter { .. })` arm in `oracle::guard_owner`
/// with `false` (or delete it, falling through to the `_ => false` default) and the rider's
/// EVENT guard finds no owner. `resolve_guards_in_ability` then rewrites the whole clause to
/// an `unparsed_replacement` gap, so BOTH halves below flip: the exile rider disappears and a
/// gap appears.
///
/// Its candidacy precondition — that the rider is DEFERRED at the seam rather than gapped
/// there — is pinned at venue S by
/// `parser::oracle_effect::tests::v17s_both_stacked_riders_are_ownership_candidates`, whose
/// first case is this row's rider clause. It cannot be pinned here: the resolver has already
/// cleared the mark by the time this venue sees the tree, and a clause gapped at the seam is
/// byte-identical to one gapped by the resolver. Without that companion a red here would be
/// ambiguous between "the O1b arm broke" and "the seam stopped deferring".
#[test]
fn v7s_o1b_event_guarded_exile_rider_under_counter_is_owned() {
    let parsed = parse(
        SYNTHETIC_EVENT_GUARDED_COUNTER_RIDER,
        "Synthetic Event Guarded Counter Rider",
        &[],
        &["Instant"],
    );

    let head = &parsed.abilities[0];
    // Reach-guard 1: the owner relation itself — the O1b arm reads the DIRECT parent, so a
    // rider that landed anywhere else would not exercise it.
    assert!(
        matches!(*head.effect, Effect::Counter { .. }),
        "V7s reach-guard: the head must be Counter, got {:?}",
        head.effect
    );
    let rider = head
        .sub_ability
        .as_deref()
        .expect("V7s reach-guard: the rider must be the head's direct sub_ability");
    // Reach-guard 2: the rider is the exile shape `is_graveyard_exile_rider_subability`
    // recognizes. Without this the row could be green on a rider the arm would refuse.
    assert!(
        is_exile_parent_target_rider(&rider.effect),
        "V7s reach-guard: the rider must be the exile shape, got {:?}",
        rider.effect
    );
    // Reach-guard 3: the guard was not absorbed into the head's own typed redirect field.
    // If assembly folded it into `countered_spell_zone` there would be no guard left to own.
    assert!(
        rider.condition.is_none(),
        "V7s reach-guard: the unlowerable guard must not have been lowered into the rider's \
         condition slot, got {:?}",
        rider.condition
    );

    // The claim: an owned EVENT-guarded body gaps nowhere.
    assert!(
        gaps(&parsed).is_empty(),
        "V7s: the O1b owner must consume the guard, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V7s");
}

// ---------------------------------------------------------------------------
// V8 / V9 / V10 — O2.
// ---------------------------------------------------------------------------

/// V8 — a **control**. CR 615.5 + CR 608.2c: the "prevented this way" follow-up survives
/// under its shield. It survives because its back-reference guard reads STATE and a STATE
/// guard does not gap — not because an owner arm pardons it. This row held under the
/// deleted O2 arm and holds without it, which is the evidence that the arm was dead.
#[test]
fn v8_o2_single_rider_under_the_shield_survives() {
    let parsed = parse(DEFLECTING_PALM, "Deflecting Palm", &[], &["Instant"]);

    let head = &parsed.abilities[0];
    assert!(
        matches!(*head.effect, Effect::PreventDamage { .. }),
        "V8 reach-guard: the head must be PreventDamage, got {:?}",
        head.effect
    );
    let rider = head
        .sub_ability
        .as_deref()
        .expect("V8: the reflection rider must survive under the shield");
    assert_eq!(
        rider.sub_link,
        SubAbilityLink::ContinuationStep,
        "V8 reach-guard: assembly's fold must actually have run"
    );
    assert!(
        matches!(*rider.effect, Effect::DealDamage { .. }),
        "V8: the rider must still be the reflection, got {:?}",
        rider.effect
    );
    assert!(
        gaps(&parsed).is_empty(),
        "V8: an owned body gaps nowhere, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V8");
}

/// V9 — a **control**, and the row that made the O2 arm look necessary. Comeuppance's
/// second rider hangs under the *first*, so its direct parent is a `DealDamage`, which is
/// why O2 was written as an ancestor test. Both riders in fact survive on the STATE reading
/// alone: their guards are CR 608.2c back-references with no "would". The row stays because
/// a future widening of the EVENT reading would gap them.
#[test]
fn v9_o2_is_an_ancestor_test_so_the_second_rider_survives() {
    let parsed = parse(COMEUPPANCE, "Comeuppance", &[], &["Instant"]);

    let head = &parsed.abilities[0];
    assert!(
        matches!(*head.effect, Effect::PreventDamage { .. }),
        "V9: the head must be PreventDamage, got {:?}",
        head.effect
    );
    let first = head
        .sub_ability
        .as_deref()
        .expect("V9: the creature-source rider must survive");
    // Reach-guard: the fold ran and this is the real two-rider chain.
    assert!(
        matches!(
            first.condition,
            Some(AbilityCondition::PostReplacementDamageSourceMatchesFilter { .. })
        ),
        "V9 reach-guard: rider one must carry its source filter, got {:?}",
        first.condition
    );
    let second = first
        .sub_ability
        .as_deref()
        .expect("V9: the noncreature-source rider must survive under rider one");
    assert!(
        matches!(*first.effect, Effect::DealDamage { .. })
            && matches!(*second.effect, Effect::DealDamage { .. }),
        "V9: both riders must remain DealDamage, got {:?} / {:?}",
        first.effect,
        second.effect
    );
    assert!(
        gaps(&parsed).is_empty(),
        "V9: both owned bodies gap nowhere, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V9");
}

// V10 — WITHDRAWN. The gap it asserts fires on a card whose shield merely failed to parse.
//
// V10 asserted that an O2 back-reference rider with no `PreventDamage` ancestor gaps. An
// O2 mark is always the STATE reading — `prevented_this_way_rider_source_gate` recognizes
// a CR 608.2c back-reference, and back-references read STATE by construction — so under
// the amended rule the resolver always clears it and this row's gap never fires.
//
// That is the right outcome, and Ria Ivor is the measured witness. Its rider is
// `"If damage is prevented this way, create …"`, and its prevention sentence does NOT
// parse at base: `oracle_replacement`'s own snapshot row
// `ria_ivor_trigger_body_keeps_fall_through_shapes` documents that "the prevention
// sentence stays an honest `Unimplemented` gap and the rider stays a `SequentialSibling`".
// So the ancestor test finds no shield, and a V10-shaped rule gaps the `Token` body that
// base emits correctly — deleting a correct instruction to punish an UNRELATED parse gap.
// A CR 615.5 gap rule would first have to distinguish "no shield printed" from "the
// printed shield did not parse"; no such rule exists here, so the row goes rather than the
// behaviour.
//
// Its venue-C companion V10c (in `parser/oracle_effect/tests.rs`) is WITHDRAWN with it. V10c
// asserted that the seam DEFERS a CR 615.5 "prevented this way" rider, i.e. that
// `is_ownership_candidate`'s `prevented_this_way_rider_source_gate` disjunct mints a mark for
// it — and that disjunct is deleted, so there is no mark left to assert. The withdrawal note
// at `parser::oracle_effect::tests`' V10c carries the full derivation; this file's header
// records the same outcome.

// ---------------------------------------------------------------------------
// V11 — WITHDRAWN at this venue. The claim moves to venue S, which has no absorber.
//
// V11 asserted that an O1a rider SHAPE under a STATE guard gaps here. It never reached
// that assertion: it fails on its own reach-guard — "the CastFromZone head must be present
// in the returned tree" — under the shipped candidate and under the amended rule alike.
// The head is not in the assembled tree for this text, so venue P cannot host the claim.
//
// Venue C cannot host it either: on a chain the clause is absorbed by the pre-existing
// `instead_condition` last resort in `parser::oracle_effect` before the seam sees it.
// (That is how the original venue-C twin V11c failed — "the seam must DEFER this clause
// rather than decide it".)
//
// The claim itself is retained, at venue S, by
// `parser::oracle_effect::tests::an_o1a_rider_shape_under_a_state_guard_is_not_an_ownership_candidate`
// — the only venue where the entry point IS the producer and no absorber sits between.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// V12 — an unowned rider with no parent at all.
// ---------------------------------------------------------------------------

/// V12. Hallowed Moonlight's rider is the chain's *head*, so it has no parent effect to
/// be owned by. Any fall-through keeps it.
#[test]
fn v12_unowned_rider_with_no_parent_gaps_and_keeps_the_following_clause() {
    let parsed = parse(HALLOWED_MOONLIGHT, "Hallowed Moonlight", &[], &["Instant"]);

    // Reach-guard: line routing still worked and the second printed line is intact.
    assert!(
        all_effects(&parsed)
            .iter()
            .any(|effect| matches!(effect, Effect::Draw { .. })),
        "V12 reach-guard: the printed \"Draw a card.\" must still be present and lowered"
    );
    assert!(
        gap_names(&parsed).contains(&"unparsed_replacement".to_string()),
        "V12: the EVENT-guarded rider must gap, got {:?}",
        gaps(&parsed)
    );
    assert!(
        !has_exile_parent_target_rider(&parsed),
        "V12: the rider must be emitted nowhere"
    );
    assert_no_live_guard_mark(&parsed, "V12");
}

// ---------------------------------------------------------------------------
// V13p — a control: the downstream-dispatcher case.
// ---------------------------------------------------------------------------

/// V13p — a **control**, not a discriminator. Phyrexian Vindicator's first line is
/// claimed by the replacement-line dispatcher before the clause seam ever sees it, so
/// its honest gap is named `replacement_structure` and no guard gap is minted at all.
///
/// This row exists so a later round cannot re-derive this card as a guard gap: an
/// earlier draft asserted exactly that here and was red against a correct
/// implementation.
#[test]
fn v13p_dispatcher_named_gap_is_not_re_derived_as_a_guard_gap() {
    let parsed = parse(
        PHYREXIAN_VINDICATOR,
        "Phyrexian Vindicator",
        &["Flying"],
        &["Creature"],
    );

    let names = gap_names(&parsed);
    assert!(
        names.contains(&"replacement_structure".to_string()),
        "V13p: the dispatcher must name this gap, got {:?}",
        gaps(&parsed)
    );
    assert!(
        !names
            .iter()
            .any(|name| name == "unparsed_replacement" || name == "unparsed_condition"),
        "V13p: no guard gap may be minted on this card, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V13p");
}

// ---------------------------------------------------------------------------
// V17 — the resolver's own gap must not pardon the guards below it.
// ---------------------------------------------------------------------------

/// V17. Two stacked unowned EVENT riders. The second's direct parent is the first, and the
/// first gaps — so if the resolver read the parent pointer AFTER rewriting it, the second
/// would see an `Effect::Unimplemented` parent and be pardoned by `guard_owner`'s R-a arm,
/// with its guard silently dropped and its body emitted.
///
/// R-a's class is the DISPATCHER's refusals (Invoke Calamity), which are already in the tree
/// when the resolver starts. A gap the resolver itself just minted is not one of those, and
/// admitting it makes a clause's verdict depend on whether an ancestor happened to gap first
/// — the same clause, gapped or pardoned by position.
///
/// Neither V12 (no parent at all) nor V4 (a non-consuming, non-gapped parent) reaches this
/// shape, which is why the defect survived them.
///
/// **PRECONDITION, pinned elsewhere because this venue cannot pin it.** Every assertion below
/// discriminates only while BOTH riders are ownership candidates, i.e. DEFERRED at the clause
/// seam and settled by the resolver. If either stopped being one, `lower_clause_ast` would gap
/// it in place instead — and a seam gap and a resolver gap are byte-identical
/// (`clause_gap_unimplemented_as(ClauseGapKind::Replacement, clause_text)` in both), so the
/// same two `unparsed_replacement` names, the same absent bodies and the same ≥3-node chain
/// would appear under EITHER ordering of the rewrite. The row would then be green at the base
/// it was written to discriminate against, and would prove nothing. Neither reach-guard below
/// excludes that: they check the tree's shape, and the tree's shape is identical either way.
///
/// So the candidacy half is asserted at venue S, at the producing seam, by
/// `parser::oracle_effect::tests::v17s_both_stacked_riders_are_ownership_candidates`. That
/// row goes red the moment either rider stops being deferred — which is the only way this one
/// can silently stop discriminating.
#[test]
fn v17_a_gap_this_pass_minted_does_not_pardon_the_guards_below_it() {
    let parsed = parse(
        STACKED_UNOWNED_RIDERS,
        "Stacked Unowned Riders",
        &[],
        &["Instant"],
    );

    // Reach-guard 1: the hostile context assembled — the head is the printed draw, so
    // neither rider has a `CastFromZone` or `Counter` owner anywhere above it.
    let nodes = chain(&parsed.abilities[0]);
    assert!(
        matches!(*nodes[0].effect, Effect::Draw { .. }),
        "V17 reach-guard: the chain must head with the printed draw, got {:?}",
        nodes[0].effect
    );
    assert!(
        !all_effects(&parsed)
            .iter()
            .any(|effect| matches!(effect, Effect::CastFromZone { .. } | Effect::Counter { .. })),
        "V17 reach-guard: no O1a/O1b owner may exist in the tree, or the row proves nothing"
    );
    // Reach-guard 2: the riders really did stack — three nodes deep, not two siblings of the
    // root. Without this the row could be green because the second rider never became the
    // first's child at all.
    assert!(
        nodes.len() >= 3,
        "V17 reach-guard: the two riders must stack below the head, got a chain of {} \
         node(s): {:?}",
        nodes.len(),
        nodes.iter().map(|n| &n.effect).collect::<Vec<_>>()
    );

    // The claim: BOTH unowned EVENT guards gap. Before the ordering fix the second was
    // pardoned and only one gap was recorded.
    let names = gap_names(&parsed);
    assert_eq!(
        names
            .iter()
            .filter(|name| *name == "unparsed_replacement")
            .count(),
        2,
        "V17: each unowned EVENT guard must gap on its own merits, got {:?}",
        gaps(&parsed)
    );
    // …and neither body is emitted. This is the half that flips: a pardoned guard leaves its
    // body running ungated, which is an instruction the card does not print.
    assert!(
        !has_exile_parent_target_rider(&parsed),
        "V17: the first rider's body must be emitted nowhere"
    );
    assert!(
        !all_effects(&parsed)
            .iter()
            .any(is_library_parent_target_rider),
        "V17: the second rider's body must be emitted nowhere — it is no more owned than \
         the first"
    );
    assert_no_live_guard_mark(&parsed, "V17");
}

// ---------------------------------------------------------------------------
// V14 — the placement invariant's discriminating test.
// ---------------------------------------------------------------------------

/// V14. Torch the Tower's rider is printed on line 3 and its owner on line 2, so the
/// ownership decision is only correct once the lines have joined. Deciding inside chain
/// assembly loses the `AddTargetReplacement` — measured as one extra flipped card.
///
/// It must be the multi-line card: the absorb arm assembly relies on is guarded by a
/// non-empty builder, so a single-clause fixture passes under every placement and is
/// vacuous here.
#[test]
fn v14_line_three_rider_survives_because_ownership_is_decided_after_routing() {
    let parsed = parse(
        TORCH_THE_TOWER,
        "Torch the Tower",
        &["Bargain"],
        &["Instant"],
    );

    let nodes = chain(&parsed.abilities[0]);
    // Reach-guard: line 2's own chain is present, so line 3 joined a real spell body.
    assert!(
        matches!(*nodes[0].effect, Effect::DealDamage { .. }),
        "V14 reach-guard: line 2 must head the chain, got {:?}",
        nodes[0].effect
    );
    assert!(
        nodes
            .iter()
            .any(|node| matches!(*node.effect, Effect::Scry { .. })),
        "V14 reach-guard: the bargain override's scry must be on the chain"
    );
    assert!(
        nodes
            .iter()
            .any(|node| matches!(*node.effect, Effect::AddTargetReplacement { .. })),
        "V14: line 3 must remain nested on line 2's chain"
    );
    let names = gap_names(&parsed);
    assert!(
        !names.iter().any(|name| name == "instead_override"),
        "V14: line 3 must not fall out as a top-level instead_override, got {:?}",
        gaps(&parsed)
    );
    assert!(
        !names
            .iter()
            .any(|name| name == "unparsed_replacement" || name == "unparsed_condition"),
        "V14: the owned rider must not gap, got {:?}",
        gaps(&parsed)
    );
    assert_no_live_guard_mark(&parsed, "V14");
}
