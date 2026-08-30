//! Regression for issues #7453 / #7454: paired block-restriction misparses where
//! the printed OBJECT of a `can't block` clause failed to bind to the right
//! combat operand.
//!
//! #7454 (`<subject> can't block or be blocked by <object>`) collapsed to a
//! blanket `CantBlock { affected: SelfRef }` — simultaneously inventing a
//! restriction the card lacks and dropping the one it prints. #7453
//! (`creatures with power <cmp> this creature's power can't block it`) already
//! parses and enforces correctly at the base commit; these tests are the runtime
//! regression pin that fix shipped without.
//!
//! https://github.com/phase-rs/phase/issues/7453
//! https://github.com/phase-rs/phase/issues/7454

use engine::game::combat::{can_block_pair, validate_blockers};
use engine::game::layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

// Verbatim Oracle text, read from the shipped card export — never paraphrased.
// A paraphrase can take a different parser branch and pass while the real card
// stays broken (`/card-test` foot-gun 4).

/// Sneaky Homunculus (1/1) — the only #7454 card that prints the clause directly.
const SNEAKY_HOMUNCULUS: &str =
    "This creature can't block or be blocked by creatures with power 2 or greater.";

/// Lost in the Spirit World — one of the 7 cards that GRANT the clause as token
/// rules text. Exercises the token path (`token.rs::push_parsed_statics`).
const LOST_IN_THE_SPIRIT_WORLD: &str = "Return up to one target creature to its owner's hand. Create a 1/1 colorless Spirit creature token with \"This token can't block or be blocked by non-Spirit creatures.\"";

/// Wandering Wolf (2/1) — #7453's `less than` direction.
const WANDERING_WOLF: &str = "Creatures with power less than this creature's power can't block it.";

/// Silumgar Assassin (2/1) — #7453's `greater than` direction. The full printed
/// text, Megamorph reminder and turned-face-up trigger included, so the pin also
/// proves those riders add no extra static.
const SILUMGAR_ASSASSIN: &str = "Creatures with power greater than this creature's power can't block it.\nMegamorph {2}{B} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its megamorph cost and put a +1/+1 counter on it.)\nWhen this creature is turned face up, destroy target creature with power 3 or less an opponent controls.";

/// Skarrgan Pit-Skulk (1/1) — #7453 with a live power modification, so the
/// threshold must MOVE with a `+1/+1` counter (CR 613.4c).
const SKARRGAN_PIT_SKULK: &str = "Bloodthirst 1 (If an opponent was dealt damage this turn, this creature enters with a +1/+1 counter on it.)\nCreatures with power less than this creature's power can't block it.";

/// The two modes a symmetric conjunction must produce, in printed clause order.
/// Asserted as a reach-guard on every #7454 fixture: before the fix each source
/// carried exactly ONE static, and it was the inverse `CantBlock`.
fn assert_symmetric_pair(state: &GameState, id: ObjectId, label: &str) {
    let defs = &state
        .objects
        .get(&id)
        .unwrap_or_else(|| panic!("{label} must exist"))
        .static_definitions;
    assert_eq!(
        defs.len(),
        2,
        "{label}: one static per direction (measured 1 before the fix): {:?}",
        defs.iter_unchecked().map(|d| &d.mode).collect::<Vec<_>>()
    );
    let modes: Vec<&StaticMode> = defs.iter_unchecked().map(|d| &d.mode).collect();
    assert!(
        matches!(modes[0], StaticMode::BlockRestriction { .. }),
        "{label}: the block half must be a blocker-side whitelist, got {:?}",
        modes[0]
    );
    assert!(
        matches!(modes[1], StaticMode::CantBeBlockedBy { .. }),
        "{label}: the be-blocked half must be attacker-side evasion, got {:?}",
        modes[1]
    );
}

/// CR 509.1b (#7454): ONE printed object binds BOTH combat operands. Sneaky
/// Homunculus prints `can't block or be blocked by creatures with power 2 or
/// greater`, so the same `power >= 2` filter must
///
///  * reject a power-3 creature BLOCKING the homunculus (the evasion half), and
///  * reject the homunculus BLOCKING a 3/3 (the restriction half),
///
/// while leaving both directions open for a power-1 creature.
///
/// Revert-failing: `can_block_pair(power3, homunculus)` is measured `true` before
/// the fix (the printed evasion was dropped entirely) and
/// `can_block_pair(homunculus, small)` is measured `false` (the blanket
/// `CantBlock` invented a restriction the card never prints). Both flip.
#[test]
fn issue_7454_symmetric_conjunction_binds_both_directions() {
    let mut s = GameScenario::new();
    let homunculus = s
        .add_creature_from_oracle(P0, "Sneaky Homunculus", 1, 1, SNEAKY_HOMUNCULUS)
        .id();
    // One pair of opponent creatures serves both directions: as blockers of the
    // homunculus and as attackers the homunculus would block.
    let big = s.add_vanilla(P1, 3, 3);
    let small = s.add_vanilla(P1, 1, 1);
    let runner = s.build();
    let state = runner.state();

    // Reach-guard: the parse actually produced two directed statics.
    assert_symmetric_pair(state, homunculus, "Sneaky Homunculus");

    // --- Evasion half (CR 509.1b): who may block the homunculus.
    assert!(
        !can_block_pair(state, big, homunculus),
        "a power-3 creature must not be able to block the homunculus (measured true before the fix)"
    );
    assert!(
        can_block_pair(state, small, homunculus),
        "a power-1 creature must still block it — this is a FILTER, not a blanket"
    );

    // --- Restriction half (CR 509.1b): what the homunculus may block.
    assert!(
        can_block_pair(state, homunculus, small),
        "the homunculus must be able to block a 1/1 (measured false before the fix)"
    );
    assert!(
        !can_block_pair(state, homunculus, big),
        "the homunculus still may not block a 3/3 — the block half bites"
    );
}

/// CR 509.1 (#7454): declaration validation is a SEPARATE seam from the per-pair
/// predicate — `validate_blockers_core` has its own `CantBeBlockedBy` and
/// `BlockRestriction` arms rather than wrapping `can_block_pair` — so it needs
/// its own assertions. One illegal pair invalidates the WHOLE declaration.
#[test]
fn issue_7454_symmetric_conjunction_invalidates_an_illegal_declaration() {
    let mut s = GameScenario::new();
    let homunculus = s
        .add_creature_from_oracle(P0, "Sneaky Homunculus", 1, 1, SNEAKY_HOMUNCULUS)
        .id();
    let other_attacker = s.add_vanilla(P0, 2, 2);
    let big = s.add_vanilla(P1, 3, 3);
    let small = s.add_vanilla(P1, 1, 1);
    let runner = s.build();
    let state = runner.state();

    assert!(
        validate_blockers(state, &[(big, homunculus)]).is_err(),
        "a power-3 blocker on the homunculus is an illegal declaration"
    );
    assert!(
        validate_blockers(state, &[(small, homunculus)]).is_ok(),
        "a power-1 blocker on the homunculus is legal — the reach-guard for the above"
    );
    // CR 509.1: the illegal pair poisons the entire declaration, even paired
    // with a legal block of a different attacker.
    assert!(
        validate_blockers(state, &[(big, homunculus), (small, other_attacker)]).is_err(),
        "one illegal pair must invalidate the whole declaration"
    );
}

/// CR 509.1b + CR 111.1 (#7454): the 7 token-granting cards need NO separate fix,
/// because `token.rs::push_parsed_statics` already calls the same multi-static
/// entry point. Drives the real cast pipeline (`/card-test` recipe) rather than
/// asserting a parsed AST shape.
///
/// Revert-failing pair (both measured wrong before the fix):
///  * `can_block_pair(token, spirit_attacker)` — measured `false`, must be `true`
///    (the blocker half must become a whitelist, not a blanket prohibition);
///  * `can_block_pair(nonspirit, token)` — measured `true`, must be `false`
///    (the evasion half must exist at all).
///
/// `can_block_pair(token, nonspirit_attacker)` is deliberately NOT claimed as
/// revert-failing: it is already `false` before the fix, because the blanket
/// `CantBlock` forbids it too. It is kept only as a consistency check.
#[test]
fn issue_7454_token_granted_conjunction_binds_to_the_token() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Lost in the Spirit World",
            false,
            LOST_IN_THE_SPIRIT_WORLD,
        )
        .id();
    // The bounce target, so the targeting slot really fires.
    let victim = s.add_vanilla(P1, 2, 2);
    // A non-Spirit and a Spirit opponent creature, serving as both attackers and
    // blockers for the two directions.
    let nonspirit = s.add_vanilla(P1, 2, 2);
    let spirit_attacker = s
        .add_creature(P1, "Spirit Attacker", 2, 2)
        .with_subtypes(vec!["Spirit"])
        .id();
    // HOSTILE (owner-vs-controller): the caster's OWN plain non-Spirit creature.
    // `affected: SelfRef` must bind the TOKEN, not the controller's board.
    let plain_own = s.add_vanilla(P0, 2, 2);
    let mut runner = s.build();

    let outcome = runner.cast(spell).target_object(victim).resolve();
    // Reach-guard: the target slot fired, so the resolution really happened.
    outcome.assert_zone(&[victim], Zone::Hand);

    let state = outcome.state();
    let token = state
        .objects
        .iter()
        .find(|(_, o)| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Spirit"))
        .map(|(id, _)| *id)
        .expect("resolving the sorcery must create a Spirit token");

    // Reach-guard: the granted statics landed ON the token, both directions.
    assert_symmetric_pair(state, token, "Spirit token");

    // --- Revert-failing: the blocker half is a whitelist, not a blanket.
    assert!(
        can_block_pair(state, token, spirit_attacker),
        "the token must be able to block a Spirit attacker (measured false before the fix)"
    );
    // --- Revert-failing: the evasion half must exist at all.
    assert!(
        !can_block_pair(state, nonspirit, token),
        "a non-Spirit creature must not block the token (measured true before the fix)"
    );
    // Consistency check only — already false before the fix, so NOT discriminating.
    assert!(
        !can_block_pair(state, token, nonspirit),
        "the token still may not block a non-Spirit attacker"
    );

    // --- HOSTILE reach-guard: the restriction is scoped to the token alone.
    assert!(
        can_block_pair(state, plain_own, nonspirit),
        "the caster's own plain creature must be unaffected — SelfRef binds the TOKEN, \
         not its controller's board nor the card that created it"
    );
}

/// CR 509.1b + CR 611.3a (#7453): the power-threshold evasion resolves against
/// the SOURCE's live power, in both comparator directions.
///
/// Honest discrimination statement: #7453 was already fixed at the base commit,
/// so this test does NOT fail against reverting the #7454 work in this change. It
/// fails against reverting the `object_is_source` route that shipped in #7452. It
/// adds real coverage because the only existing runtime test on that route uses a
/// SUBTYPE object and therefore never exercises the source-relative quantity
/// chain (`resolve_filter_threshold` → `resolve_quantity` →
/// `resolve_object_pt(ObjectScope::Source)` with `ability = None`).
#[test]
fn issue_7453_power_threshold_evasion_tracks_live_source_power() {
    let mut s = GameScenario::new();
    // "less than this creature's power" on a 2/1 → blockers with power <= 1.
    let wolf = s
        .add_creature_from_oracle(P0, "Wandering Wolf", 2, 1, WANDERING_WOLF)
        .id();
    // "greater than this creature's power" on a 2/1 → blockers with power >= 3.
    let assassin = s
        .add_creature_from_oracle(P0, "Silumgar Assassin", 2, 1, SILUMGAR_ASSASSIN)
        .id();
    let other_attacker = s.add_vanilla(P0, 2, 2);
    let p1 = s.add_vanilla(P1, 1, 5);
    let p2 = s.add_vanilla(P1, 2, 5);
    let p3 = s.add_vanilla(P1, 3, 5);
    let enemy = s.add_vanilla(P1, 1, 1);
    let runner = s.build();
    let state = runner.state();

    // Reach-guard: exactly one attacker-side static on each source. On Silumgar
    // this also proves the Megamorph reminder and the turned-face-up trigger add
    // no extra or `Unimplemented` static.
    for (id, label) in [(wolf, "Wandering Wolf"), (assassin, "Silumgar Assassin")] {
        let defs = &state.objects.get(&id).unwrap().static_definitions;
        assert_eq!(
            defs.len(),
            1,
            "{label}: exactly one static: {:?}",
            defs.iter_unchecked().map(|d| &d.mode).collect::<Vec<_>>()
        );
        assert!(
            matches!(
                defs.iter_unchecked().next().unwrap().mode,
                StaticMode::CantBeBlockedBy { .. }
            ),
            "{label}: the restriction is attacker-side evasion, not a blanket CantBlock"
        );
    }

    // --- `less than` (threshold: power <= 1).
    assert!(
        !can_block_pair(state, p1, wolf),
        "power 1 is below the wolf's power 2 and must not block it"
    );
    assert!(
        can_block_pair(state, p2, wolf),
        "power 2 is not below the wolf's power 2"
    );
    assert!(
        can_block_pair(state, p3, wolf),
        "power 3 is not below the wolf's power 2"
    );

    // --- `greater than` (threshold: power >= 3) — the opposite comparator on the
    // same parameterized filter shape.
    assert!(
        can_block_pair(state, p1, assassin),
        "power 1 is not above the assassin's power 2"
    );
    assert!(
        can_block_pair(state, p2, assassin),
        "power 2 is not above the assassin's power 2"
    );
    assert!(
        !can_block_pair(state, p3, assassin),
        "power 3 is above the assassin's power 2 and must not block it"
    );

    // --- Reach-guards proving the restriction is ATTACKER-scoped, i.e. that this
    // is a `CantBeBlockedBy` on the source and not the inverse `CantBlock` both
    // issues report.
    assert!(
        can_block_pair(state, p1, other_attacker),
        "the power-1 creature may still block a DIFFERENT attacker"
    );
    assert!(
        can_block_pair(state, wolf, enemy),
        "the wolf itself remains a legal blocker"
    );

    // Declaration validation — the separate `validate_blockers_core` seam.
    assert!(validate_blockers(state, &[(p1, wolf)]).is_err());
    assert!(validate_blockers(state, &[(p2, wolf)]).is_ok());
    assert!(
        validate_blockers(state, &[(p1, wolf), (p2, other_attacker)]).is_err(),
        "CR 509.1: one illegal pair invalidates the whole declaration"
    );
}

/// CR 613.4c + CR 122.1a (#7453): the threshold is a LIVE read of the source's
/// power, so a `+1/+1` counter must MOVE it. Skarrgan Pit-Skulk is 1/1 printed
/// (threshold `power <= 0`, which no creature satisfies) and 2/2 with one counter
/// (threshold `power <= 1`, which excludes a power-1 blocker).
///
/// This is the only fixture that distinguishes three otherwise identical
/// implementations: a live `Power{Source}` read (correct), a `PtValueScope::Base`
/// read (CR 208.4b — would keep the threshold at `<= 0` and wrongly ACCEPT the
/// power-1 blocker), and a parse-time constant.
///
/// The `power == Some(2)` assertion is a mandatory POSITIVE CONTROL, not
/// decoration: the counter is measured NOT reflected in computed power after
/// `build()` alone, so without the explicit layer flush the whole fixture is
/// vacuous and would pass against a base-power implementation too.
#[test]
fn issue_7453_counter_moves_the_live_power_threshold() {
    let mut s = GameScenario::new();
    let skulk = s
        .add_creature_from_oracle(P0, "Skarrgan Pit-Skulk", 1, 1, SKARRGAN_PIT_SKULK)
        .id();
    s.with_counter(skulk, CounterType::Plus1Plus1, 1);
    let p1 = s.add_vanilla(P1, 1, 5);
    let p2 = s.add_vanilla(P1, 2, 5);
    let mut runner = s.build();

    // POSITIVE CONTROL FIRST: flush layers so the counter is reflected in the
    // computed power. Without this the fixture is vacuous (measured Some(1)).
    layers::mark_layers_full(runner.state_mut());
    layers::evaluate_layers(runner.state_mut());
    let state = runner.state();
    assert_eq!(
        state.objects.get(&skulk).unwrap().power,
        Some(2),
        "the +1/+1 counter must be reflected in computed power before any block \
         assertion is meaningful (CR 613.4c)"
    );

    // Threshold is now "power <= 1", not the printed "power <= 0".
    assert!(
        !can_block_pair(state, p1, skulk),
        "power 1 is below the buffed power 2 and must not block — a BASE power \
         read would keep the threshold at <= 0 and wrongly accept this blocker"
    );
    assert!(
        can_block_pair(state, p2, skulk),
        "power 2 is not below the buffed power 2"
    );
}

/// CR 509.1b (#7453): "Different evasion abilities are cumulative." Two
/// independent evasion statics on one attacker must BOTH apply, so the
/// enforcement loop cannot stop at the first one. Uses the
/// `static_definitions.push(..)` idiom the existing #7452 unit test established.
#[test]
fn issue_7453_evasion_abilities_are_cumulative() {
    let mut s = GameScenario::new();
    let wolf = s
        .add_creature_from_oracle(P0, "Wandering Wolf", 2, 1, WANDERING_WOLF)
        .id();
    // A power-3 non-Coward, a power-3 Coward, and a power-1 non-Coward.
    let big_plain = s.add_vanilla(P1, 3, 3);
    let big_coward = s
        .add_creature(P1, "Coward", 3, 3)
        .with_subtypes(vec!["Coward"])
        .id();
    let small_plain = s.add_vanilla(P1, 1, 1);
    let mut runner = s.build();

    // Layer a SECOND, independent evasion ability onto the same attacker.
    runner
        .state_mut()
        .objects
        .get_mut(&wolf)
        .unwrap()
        .static_definitions
        .push(
            engine::parser::oracle_static::parse_static_line("Cowards can't block it.")
                .expect("the subtype evasion clause must parse"),
        );
    let state = runner.state();

    // Reach-guard: both statics are present, so the assertions below are about
    // cumulation rather than about one static doing all the work.
    assert_eq!(
        state.objects.get(&wolf).unwrap().static_definitions.len(),
        2,
        "the fixture needs two independent evasion abilities"
    );

    assert!(
        can_block_pair(state, big_plain, wolf),
        "a power-3 non-Coward satisfies both restrictions"
    );
    assert!(
        !can_block_pair(state, big_coward, wolf),
        "the SECOND evasion ability must also apply — the loop cannot stop at the first"
    );
    assert!(
        !can_block_pair(state, small_plain, wolf),
        "the FIRST evasion ability must still apply"
    );
}

/// A deliberately SYNTHETIC grammar probe — the one fixture in this file that is
/// not verbatim printed text, and labelled as such. No printed card carries a
/// leading `"As long as <condition>, "` gate on the symmetric conjunction (census
/// of every face in `AtomicCards.json`), so there is no Oracle line to quote. What
/// is under test is the BUILDING BLOCK — the gate composed with the conjunction —
/// which is exactly the axis the parser now handles generically, and the runtime
/// question it raises cannot be answered by a parse-shape assertion.
const GATED_SYMMETRIC_CONJUNCTION: &str = "As long as you control a Wall, this creature can't block or be blocked by creatures with power 2 or greater.";

/// Build the gated fixture with and without the permanent the gate names, so the
/// two arms differ ONLY in whether the printed condition holds.
///
/// Returns `(runner, gated, big, small)`. Layers are flushed before the caller
/// asserts, mirroring `issue_7453_counter_moves_the_live_power_threshold`: the
/// subtype the gate keys on is a computed characteristic, so a fixture that reads
/// it without a flush is asserting on pre-layer state.
fn gated_symmetric_fixture(
    controls_a_wall: bool,
) -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut s = GameScenario::new();
    let gated = s
        .add_creature_from_oracle(P0, "Gated Homunculus", 1, 1, GATED_SYMMETRIC_CONJUNCTION)
        .id();
    if controls_a_wall {
        // CR 604.1: "you control a Wall" is relative to the STATIC's controller,
        // so the Wall belongs to P0, not to the creatures under restriction.
        s.add_creature(P0, "Test Wall", 0, 4)
            .with_subtypes(vec!["Wall"]);
    }
    let big = s.add_vanilla(P1, 3, 3);
    let small = s.add_vanilla(P1, 1, 1);
    let mut runner = s.build();
    layers::mark_layers_full(runner.state_mut());
    layers::evaluate_layers(runner.state_mut());
    (runner, gated, big, small)
}

/// CR 611.3a + CR 509.1b (#7454, round 4): a leading `"As long as <condition>, "`
/// gate must GATE BOTH halves at runtime, not merely be recorded on the parsed
/// statics. CR 611.3a makes clause orientation semantically irrelevant, so the
/// gated line's enforcement must equal the bare line's enforcement whenever the
/// condition holds, and must vanish entirely when it does not.
///
/// The two arms differ ONLY in whether P0 controls a Wall, which makes each arm
/// the other's control:
///
///  * gate FALSE — both directions must be fully open. This is the arm that fails
///    if the condition is dropped on either half, or typed as
///    `StaticCondition::Unrecognized` (which evaluates to `true`, so both
///    restrictions would apply unconditionally).
///  * gate TRUE — both directions must bite exactly as the ungated form does,
///    and still as a FILTER: the power-1 creature stays legal in both directions,
///    so a regression to a blanket prohibition fails here rather than passing.
///
/// Revert-failing against the state this PR was opened in: the gated line then
/// lowered to ONE inert `Continuous` static with no modifications, so the
/// `gate TRUE` arm's two `!can_block_pair` assertions were both `true` — the card
/// enforced nothing while reading as parsed.
#[test]
fn issue_7454_leading_gate_toggles_both_directions_at_runtime() {
    // --- Reach-guard: the gated line parses to the same two directed statics as
    // the bare form, each carrying the gate. Without this the arms below could
    // agree for the wrong reason (e.g. nothing parsed at all).
    let (runner, gated, _, _) = gated_symmetric_fixture(true);
    assert_symmetric_pair(runner.state(), gated, "gated symmetric conjunction");
    let defs = &runner
        .state()
        .objects
        .get(&gated)
        .expect("the gated creature must exist")
        .static_definitions;
    for def in defs.iter_unchecked() {
        assert!(
            def.condition.is_some(),
            "each half must carry the gate independently (CR 509.1b: the two \
             restrictions are cumulative, so each is gated on its own): {:?}",
            def.mode
        );
    }

    // --- Gate FALSE: no Wall, so neither printed restriction applies.
    let (runner, gated, big, _small) = gated_symmetric_fixture(false);
    let state = runner.state();
    assert!(
        can_block_pair(state, big, gated),
        "gate false: the evasion half must not apply — a power-3 creature blocks freely"
    );
    assert!(
        can_block_pair(state, gated, big),
        "gate false: the blocker half must not apply — the creature may block a 3/3"
    );

    // --- Gate TRUE: P0 controls a Wall, so both halves bite.
    let (runner, gated, big, small) = gated_symmetric_fixture(true);
    let state = runner.state();
    assert!(
        !can_block_pair(state, big, gated),
        "gate true: the evasion half must apply (measured true before this round — \
         the line lowered to an inert static and enforced nothing)"
    );
    assert!(
        !can_block_pair(state, gated, big),
        "gate true: the blocker half must apply (measured true before this round)"
    );
    // Still a FILTER, not a blanket — in BOTH directions.
    assert!(
        can_block_pair(state, small, gated),
        "gate true: a power-1 creature must still block it — the evasion half is a filter"
    );
    assert!(
        can_block_pair(state, gated, small),
        "gate true: it must still block a 1/1 — the blocker half is a filter, and a \
         regression to the blanket `CantBlock` inversion fails here"
    );

    // CR 509.1: the declaration seam is separate from the per-pair predicate, so
    // the gate must reach it too.
    assert!(
        validate_blockers(state, &[(big, gated)]).is_err(),
        "gate true: a power-3 blocker is an illegal declaration"
    );
    assert!(
        validate_blockers(state, &[(small, gated)]).is_ok(),
        "gate true: a power-1 blocker is legal — the reach-guard for the above"
    );
}
