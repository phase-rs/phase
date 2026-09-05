//! Phase B1 — the Will-cycle duration seam.
//!
//! Two independent parser units, both exercised through the crate's PUBLIC
//! surface:
//!
//! * **U1** (`parser/oracle_static/restriction.rs`) — a leading duration head
//!   on the graveyard cast/play permission. CR 611.2a: the head states the
//!   window; before U1 it blocked the entire permission body.
//! * **U2** (`parser/oracle_replacement.rs`) — an antecedent-INTERNAL window on
//!   the graveyard-exile replacement, captured as a typed `Duration` and
//!   stamped onto `ReplacementDefinition.expiry`. CR 514.2 + CR 611.2a: an
//!   unstamped window is not "missing", it is PERMANENT, because CR 611.2a
//!   makes an unstated duration last until the end of the game.
//!
//! **Honesty statement.** B1 makes ZERO cards supported. Yawgmoth's Will,
//! Gaea's Will and Magus of the Will still parse to `Effect::Unimplemented`;
//! `v5_will_cycle_cards_remain_honestly_unsupported` pins that as a REGRESSION
//! GUARD, not as a discriminating test.
//!
//! **Stack size.** `parse_oracle_text` overflows the default 8 MB test stack
//! and prints a convincing PARTIAL negative on the way down. Every body that
//! calls it therefore runs on a 256 MB thread via `on_big_stack`.

use engine::parser::oracle::ParsedAbilities;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::parser::oracle_static::parse_static_line;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    CardPlayMode, ControllerRef, Effect, FilterProp, RestrictionExpiry, StaticDefinition,
    TargetFilter,
};
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;

/// `parse_oracle_text` recurses deeply enough to blow the default 8 MB test
/// stack. A blown stack does NOT look like a failure — it looks like a
/// plausible partial parse. Run every such body on 256 MB.
fn on_big_stack<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .expect("spawn 256MB parser thread")
        .join()
        .expect("parser thread must not panic")
}

fn parse_sorcery(text: &'static str, name: &'static str) -> ParsedAbilities {
    on_big_stack(move || parse_oracle_text(text, name, &[], &["Sorcery".to_string()], &[]))
}

fn parse_with_types(
    text: &'static str,
    name: &'static str,
    types: &'static [&'static str],
) -> ParsedAbilities {
    on_big_stack(move || {
        let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
        parse_oracle_text(text, name, &[], &types, &[])
    })
}

/// Count `Duration_ThisTurn` swallow warnings — the diagnostic U2 retires by
/// representing the window instead of dropping it.
fn duration_this_turn_warnings(parsed: &ParsedAbilities) -> usize {
    parsed
        .parse_warnings
        .iter()
        .filter(|w| {
            matches!(
                w,
                OracleDiagnostic::SwallowedClause { detector, .. }
                    if detector == "Duration_ThisTurn"
            )
        })
        .count()
}

fn permission_mode(def: &StaticDefinition) -> (&CastFrequency, &CardPlayMode) {
    match &def.mode {
        StaticMode::GraveyardCastPermission {
            frequency,
            play_mode,
            ..
        } => (frequency, play_mode),
        other => panic!("expected GraveyardCastPermission, got {other:?}"),
    }
}

// ── V1 ────────────────────────────────────────────────────────────────────
// DISCRIMINATING. Pre-change this returns `None`: the leading "Until end of
// turn, " head is unmatched by every branch of the permission parser, so the
// body below it is unreachable. Revert U1 and the first `expect` fires.

const WILL_PERMISSION: &str =
    "Until end of turn, you may play lands and cast spells from your graveyard.";
const WILL_PERMISSION_HEADLESS: &str = "You may play lands and cast spells from your graveyard.";

#[test]
fn v1_leading_until_end_of_turn_head_no_longer_blocks_the_permission() {
    // THE discriminating assertion. Pre-change: `None`.
    let def = parse_static_line(WILL_PERMISSION)
        .expect("U1: leading 'until end of turn, ' head must reach the permission body");
    let (frequency, play_mode) = permission_mode(&def);
    assert_eq!(*frequency, CastFrequency::Unlimited);
    assert_eq!(*play_mode, CardPlayMode::Play);

    // PAIRED POSITIVE REACH-GUARD. The identical sentence WITHOUT the head
    // parses both before and after U1 — proving the body was always reachable
    // and the head was the only blocker, so V1's flip is attributable to U1
    // and not to the body suddenly starting to work.
    let headless = parse_static_line(WILL_PERMISSION_HEADLESS)
        .expect("reach-guard: the headless permission must parse pre- AND post-change");
    let (g_freq, g_mode) = permission_mode(&headless);
    assert_eq!(*g_freq, CastFrequency::Unlimited);
    assert_eq!(*g_mode, CardPlayMode::Play);
}

#[test]
fn v1a_hostile_head_reaches_the_cast_branch_not_only_the_play_branch() {
    // HOSTILE FIXTURE (a): a DIFFERENT body branch under the same head. If U1
    // were wired only into the "you may play " arm, this would still be `None`.
    let def =
        parse_static_line("Until end of turn, you may cast creature spells from your graveyard.")
            .expect("U1 must feed the cast branch too");
    let (_, play_mode) = permission_mode(&def);
    assert_eq!(*play_mode, CardPlayMode::Cast);
}

#[test]
fn v1b_hostile_head_without_graveyard_clause_still_declines() {
    // HOSTILE FIXTURE (b): the head is consumed, but the body's
    // `split_once_on(rest, " from your graveyard")` early-return still fires.
    // Proves U1 widened the head only, not the body's own requirements.
    assert!(
        parse_static_line("Until end of turn, you may play lands.").is_none(),
        "no ' from your graveyard' clause means the permission must still decline"
    );
}

#[test]
fn v1c_hostile_bare_head_declines() {
    // HOSTILE FIXTURE (c): a head with NO body reaches the
    // `nom_tag_lower(..., "you may cast ")?` return. Proves U1's head does not
    // manufacture a permission out of a duration phrase alone.
    assert!(
        parse_static_line("Until end of turn, ").is_none(),
        "a bare duration head must not produce a permission"
    );
}

// ── V2 ────────────────────────────────────────────────────────────────────
// DISCRIMINATING. U1 delegates to the single duration grammar rather than
// hard-coding one phrase. If it hard-coded `UntilEndOfTurn`, V1 would still
// pass and THIS test would fail.

#[test]
fn v2_the_whole_duration_grammar_reaches_the_permission_body() {
    for head in [
        "Until your next turn, ",
        "Until end of combat, ",
        "Until end of turn, ",
    ] {
        let text = format!("{head}you may play lands and cast spells from your graveyard.");
        let def = parse_static_line(&text)
            .unwrap_or_else(|| panic!("U1 must accept the duration head {head:?}"));
        let (_, play_mode) = permission_mode(&def);
        assert_eq!(*play_mode, CardPlayMode::Play, "head {head:?}");
    }

    // PAIRED POSITIVE REACH-GUARD: the PRE-EXISTING "during your turn, " head,
    // which parses both before and after U1. Proves the instrument and the
    // permission body are live independently of U1, so the three flips above
    // are U1's doing.
    let existing = parse_static_line(
        "During your turn, you may play lands and cast spells from your graveyard.",
    )
    .expect("reach-guard: the pre-existing 'during your turn, ' head parses either way");
    let (_, existing_mode) = permission_mode(&existing);
    assert_eq!(*existing_mode, CardPlayMode::Play);
}

#[test]
fn v2a_hostile_misspelled_head_declines() {
    // HOSTILE FIXTURE: a typo'd duration. `parse_duration`'s `alt` fails, the
    // head is NOT consumed, and the body's own `nom_tag_lower` then fails on
    // the leftover text. Proves the head is genuinely PARSED, not skipped by
    // scanning ahead for a comma.
    assert!(
        parse_static_line(
            "Untill end of turn, you may play lands and cast spells from your graveyard."
        )
        .is_none(),
        "an unparseable duration head must not be silently skipped"
    );
}

// ── V3 ────────────────────────────────────────────────────────────────────
// DISCRIMINATING. Pre-change: `replacements.len() == 0` (the `tag(", ")` fails
// on " this turn") and one `Duration_ThisTurn` swallow warning. Post-change:
// one replacement, `expiry == Some(EndOfTurn)`, and the warning gone.

const CASE_B: &str =
    "If a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const CASE_B_WINDOWLESS: &str =
    "If a card would be put into your graveyard from anywhere, exile that card instead.";

#[test]
fn v3_antecedent_window_is_captured_and_stamped_onto_expiry() {
    let parsed = parse_sorcery(CASE_B, "Window Probe");

    // THE discriminating assertions. Pre-change all three fail: 0 replacements,
    // no expiry to read, and warns == 1.
    assert_eq!(
        parsed.replacements.len(),
        1,
        "U2: the antecedent window must not block the replacement"
    );
    let def = &parsed.replacements[0];
    assert_eq!(def.event, ReplacementEvent::Moved);
    assert_eq!(def.destination_zone, Some(Zone::Graveyard));
    assert_eq!(
        def.expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "CR 514.2: 'this turn' must be STAMPED, not merely parsed — an unstamped \
         window is a permanent replacement under CR 611.2a"
    );
    assert_eq!(
        duration_this_turn_warnings(&parsed),
        0,
        "the window is now represented, so the swallow warning must be gone"
    );

    // PAIRED POSITIVE REACH-GUARD. The window-FREE sibling yields a
    // replacement with `expiry: None`. Without this row, `Some(EndOfTurn)`
    // above could be satisfied by an instrument that reports one value only —
    // this proves `None` is a distinguishable reading of the same field.
    let guard = parse_sorcery(CASE_B_WINDOWLESS, "Windowless Probe");
    assert_eq!(
        guard.replacements.len(),
        1,
        "reach-guard: window-free clause parses"
    );
    assert_eq!(
        guard.replacements[0].expiry, None,
        "CR 604.2: a clause stating no window must keep expiry: None"
    );
}

#[test]
fn v3a_leyline_of_the_void_is_unchanged() {
    // HOSTILE FIXTURE (a): the `Scope::Opponent` authority. Must be untouched
    // by the new `opt()`, and must keep `expiry: None`.
    let parsed = parse_sorcery(
        "If a card would be put into an opponent's graveyard from anywhere, exile it instead.",
        "Leyline of the Void",
    );
    assert_eq!(parsed.replacements.len(), 1);
    let def = &parsed.replacements[0];
    assert_eq!(
        def.expiry, None,
        "CR 604.2: a printed static states no window"
    );
    let props = match &def.valid_card {
        Some(TargetFilter::Typed(t)) => &t.properties,
        other => panic!("expected a typed valid_card filter, got {other:?}"),
    };
    assert!(
        props.contains(&FilterProp::Owned {
            controller: ControllerRef::Opponent
        }),
        "the opponent-owner scope must survive: {props:?}"
    );
}

#[test]
fn v3b_rest_in_peace_is_unchanged() {
    // HOSTILE FIXTURE (b): `Scope::Any` + `TokenScope::Unscoped` — the
    // no-valid_card authority.
    let parsed = parse_sorcery(
        "If a card or token would be put into a graveyard from anywhere, exile it instead.",
        "Rest in Peace",
    );
    assert_eq!(parsed.replacements.len(), 1);
    let def = &parsed.replacements[0];
    assert_eq!(
        def.valid_card, None,
        "unscoped subject means no valid_card filter"
    );
    assert_eq!(
        def.expiry, None,
        "CR 604.2: a printed static states no window"
    );
}

#[test]
fn v3c_shuffle_back_outcome_is_unchanged() {
    // HOSTILE FIXTURE (c): the OTHER outcome arm. Proves the new `opt()` sits
    // BEFORE outcome dispatch and does not perturb it.
    let parsed = parse_sorcery(
        "If a card would be put into your graveyard from anywhere, shuffle it into its owner's library instead.",
        "Shuffle Probe",
    );
    assert_eq!(parsed.replacements.len(), 1);
    assert_eq!(
        parsed.replacements[0].destination_zone,
        Some(Zone::Graveyard)
    );
    assert_eq!(parsed.replacements[0].expiry, None);
}

#[test]
fn v3d_dauthi_voidwalker_counter_rider_is_unchanged() {
    // HOSTILE FIXTURE (d): the `parse_exile_anaphor_clause` path with an
    // inline counter rider (CR 122.1).
    let parsed = parse_sorcery(
        "If a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.",
        "Dauthi Voidwalker",
    );
    assert_eq!(parsed.replacements.len(), 1);
    assert_eq!(parsed.replacements[0].expiry, None);
}

// ── V4 ────────────────────────────────────────────────────────────────────
// DISCRIMINATING — but only because of its reach-guard. `replacements == 0` is
// ALSO the pre-change value (the old `tag(", ")` simply failed on the window),
// so the bare count proves nothing on its own. The guard establishes that the
// window IS parsed at this position, which makes the 0 attributable to U2's
// `UntilNextTurnOf => return None` arm rather than to a failed tag.

#[test]
fn v4_unbindable_window_is_declined_not_silently_shortened() {
    // REACH-GUARD FIRST, and it is load-bearing: same grammar, same `opt()`,
    // same position, only the `Duration` variant differs. Pre-change this is
    // 0 / no expiry; post-change it is 1 / Some(EndOfTurn). It proves the
    // window at this position is genuinely parsed.
    let reachable = parse_sorcery(CASE_B, "Window Probe");
    assert_eq!(
        reachable.replacements.len(),
        1,
        "reach-guard: a BINDABLE window at this position is accepted"
    );
    assert_eq!(
        reachable.replacements[0].expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "reach-guard: and it is stamped"
    );

    // CR 611.2a + CR 500.4: a parsed static replacement has no installation
    // context binding "your next turn" to a player. Declining keeps coverage
    // honest; mapping it to EndOfTurn would SHORTEN the printed duration.
    let declined = parse_sorcery(
        "If a card would be put into your graveyard from anywhere until your next turn, exile that card instead.",
        "Unbindable Window Probe",
    );
    assert_eq!(
        declined.replacements.len(),
        0,
        "an unbindable window must decline the whole definition, not shorten it"
    );
}

#[test]
fn v4a_this_combat_window_stamps_end_of_combat() {
    // HOSTILE FIXTURE: a SECOND live accepting arm with a DIFFERENT expiry.
    // Together with V4's decline and V3's EndOfTurn, this proves the `match`
    // is a real classification over three outcomes, not a constant.
    let parsed = parse_sorcery(
        "If a card would be put into your graveyard from anywhere this combat, exile that card instead.",
        "Combat Window Probe",
    );
    assert_eq!(parsed.replacements.len(), 1);
    assert_eq!(
        parsed.replacements[0].expiry,
        Some(RestrictionExpiry::EndOfCombat),
        "CR 511.2: 'this combat' expires at the end of the combat phase"
    );
}

// ── V5 ────────────────────────────────────────────────────────────────────
// **REGRESSION GUARD — NOT A DISCRIMINATING TEST.** These four assertions hold
// identically before AND after U1/U2, by design: B1 makes zero cards supported.
// The row exists to prove B1 did not accidentally start claiming coverage it
// has not built. Both of its reach-guards are required, because every assertion
// here is a negative.

const YAWGMOTHS_WILL: &str = "Until end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const GAEAS_WILL: &str = "Suspend 4—{G}\nUntil end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const MAGUS_OF_THE_WILL: &str = "{2}{B}, {T}, Exile this creature: Until end of turn, you may play lands and cast spells from your graveyard. If a card would be put into your graveyard from anywhere this turn, exile that card instead.";

#[test]
fn v5_will_cycle_cards_remain_honestly_unsupported() {
    // MULTI-AUTHORITY hostile fixture: three different arrival shapes — a bare
    // sorcery, a sorcery preceded by a Suspend line, and a creature's activated
    // ability. All three must yield the SAME verdict, proving the outcome keys
    // on the body rather than on AbilityKind, cost presence, or line index.
    for (text, name, types, expected_replacements) in [
        // The two Sorceries expose their line-2 replacement to the card-level
        // scan; Magus does not, because its line 2 is inside an activated
        // ability's effect text.
        (YAWGMOTHS_WILL, "Yawgmoth's Will", &["Sorcery"][..], 1usize),
        (GAEAS_WILL, "Gaea's Will", &["Sorcery"][..], 1usize),
        (
            MAGUS_OF_THE_WILL,
            "Magus of the Will",
            &["Creature"][..],
            0usize,
        ),
    ] {
        let parsed = parse_with_types(text, name, types);

        // (i) coverage stays RED — the permission body is still unimplemented.
        assert!(
            parsed
                .abilities
                .iter()
                .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
            "{name}: must still report an Unimplemented effect"
        );
        // (ii) B1 fabricates no emblem.
        assert!(
            !parsed
                .abilities
                .iter()
                .any(|a| matches!(&*a.effect, Effect::CreateEmblem { .. })),
            "{name}: B1 must not manufacture an emblem"
        );
        // (iii) MEASURED, not assumed. The plan's pre-change baseline was
        // `replacements == 0` for all three cards; U2 CHANGES that for the two
        // Sorceries, and the honest assertion records the change rather than
        // the stale baseline.
        //
        // Yawgmoth's Will and Gaea's Will now emit their real line-2
        // replacement, correctly windowed: `expiry: Some(EndOfTurn)`. That is
        // CR 514.2-correct and is precisely U2's purpose — pre-change the
        // clause was DROPPED entirely (0 replacements + a `Duration_ThisTurn`
        // swallow warning). It is additionally inert at runtime: a Sorcery
        // never reaches the `[Battlefield, Command]` zone gate in
        // `game::replacement::object_replacement_candidate_applies`, so no
        // definition is ever consulted.
        //
        // Magus of the Will stays at 0 because its line 2 sits INSIDE an
        // activated ability's effect text, which the card-level replacement
        // scan does not reach.
        //
        // The load-bearing invariant is not "zero replacements" — it is that
        // NOTHING PERMANENT escapes. Every replacement these cards produce
        // must carry a window.
        assert_eq!(
            parsed.replacements.len(),
            expected_replacements,
            "{name}: card-level replacement count"
        );
        for r in &parsed.replacements {
            assert_eq!(
                r.expiry,
                Some(RestrictionExpiry::EndOfTurn),
                "{name}: CR 611.2a - this class must NEVER produce a permanent card-level replacement; the stated one-turn window must be stamped"
            );
        }
        // (iv) no duration-less permission static escapes U1's head.
        assert_eq!(
            parsed.statics.len(),
            0,
            "{name}: no permission static escapes"
        );
    }

    // REACH-GUARD (alpha) for assertion (ii): the instrument CAN count an
    // emblem. Without this, "zero emblems" is satisfied by a broken matcher.
    let emblem = parse_sorcery(
        "You get an emblem with \"Creatures you control get +1/+1.\"",
        "Emblem Guard",
    );
    assert!(
        emblem
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::CreateEmblem { .. })),
        "reach-guard: the emblem instrument must be able to see an emblem"
    );

    // REACH-GUARD (beta) for assertion (iii): the replacement path IS live for
    // this exact sentence in isolation, so the cards' 0 is a real absence
    // rather than a dead instrument.
    let live = parse_sorcery(CASE_B, "Window Probe");
    assert_eq!(
        live.replacements.len(),
        1,
        "reach-guard: the replacement path is live for this clause standalone"
    );
}

#[test]
fn v5b_the_same_grammar_on_a_permanent_host_is_stamped_not_permanent() {
    // HOST-TYPE hostile fixture, and the row that proves U2 earns its keep
    // BEYOND the three printed cards. The card-level replacement scan is
    // host-type-agnostic, so this shape on a Creature or an Enchantment host
    // yields a LIVE replacement. Pre-change it is live and PERMANENT
    // (`expiry: None`); post-change it self-expires at cleanup.
    for host in [&["Creature"][..], &["Enchantment"][..]] {
        let parsed = parse_with_types(CASE_B, "Permanent Host Probe", host);
        assert_eq!(
            parsed.replacements.len(),
            1,
            "{host:?}: replacement is live"
        );
        assert_eq!(
            parsed.replacements[0].expiry,
            Some(RestrictionExpiry::EndOfTurn),
            "{host:?}: CR 514.2 — a stated one-turn window must not install a \
             PERMANENT replacement on a permanent host"
        );
    }
}
