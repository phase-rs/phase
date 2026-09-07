//! Phase B2 — the Will cycle's coordinated land half.
//!
//! **What was actually missing.** "Until end of turn, you may play lands **and**
//! cast spells from your graveyard" (Yawgmoth's Will, Gaea's Will, Magus of the
//! Will) is ONE permission naming two actions. `"cast "` is a bare-`and` clause
//! starter, so the sequence splitter separates the halves. The CAST half keeps
//! the zone clause and ALREADY lowered correctly to `Effect::CastFromZone`
//! before this change — these cards were HALF supported, not unsupported. The
//! LAND half is left as the bare fragment `"play lands"`, which
//! `try_parse_cast_effect`'s CR 305.2a guard refuses, because a zone-less
//! "play lands" genuinely is not a grant.
//!
//! **The unit under test** (`parser/oracle.rs::recover_coordinated_land_play`)
//! runs after the ability chain is assembled, where both halves are adjacent,
//! and rebuilds the refused land half from its `CastFromZone` sibling — the
//! same lookback shape as `nearest_dig_rest_zone_in_ability`. The CR 305.2a
//! guard is CORRECT and is NOT weakened: it runs per-clause with no forward
//! view, so it cannot see the sibling that carries the zone.
//!
//! **Honesty statement.** This suite asserts PARSE SHAPE only. A parsed
//! `CastFromZone{mode: Play}` is a permission the engine already models; this
//! file does not re-test that runtime path, and it does not claim any card is
//! "supported" — the card-data coverage gate is the authority for that word.
//!
//! **Stack size.** `parse_oracle_text` overflows the default 8 MB test stack
//! and prints a convincing PARTIAL negative on the way down rather than
//! failing. Every body that calls it runs on 256 MB via `on_big_stack`.

use engine::parser::oracle::ParsedAbilities;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CardPlayMode, ControllerRef, Duration, Effect, FilterProp, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::zones::Zone;

/// See the module note on stack size — a blown stack looks like a partial parse.
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

fn parse(
    text: &'static str,
    name: &'static str,
    types: &'static [&'static str],
) -> ParsedAbilities {
    on_big_stack(move || {
        let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
        parse_oracle_text(text, name, &[], &types, &[])
    })
}

/// Walk the WHOLE ability chain, not just its root.
///
/// This helper is load-bearing and its absence is a documented failure mode:
/// the working cast half of every card in this file lives in `sub_ability`, so
/// an instrument that reads only `abilities[i].effect` reports these cards as
/// unparsed and is simply wrong. `kiora_self_library_peek_cast.rs` carries the
/// same recursion for the same reason.
fn walk<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
    out.push(def);
    if let Some(sub) = def.sub_ability.as_deref() {
        walk(sub, out);
    }
}

/// Every `AbilityDefinition` reachable in the parse — abilities and triggers,
/// each followed through its `sub_ability` chain.
fn all_defs(parsed: &ParsedAbilities) -> Vec<&AbilityDefinition> {
    let mut out = Vec::new();
    for a in &parsed.abilities {
        walk(a, &mut out);
    }
    for t in &parsed.triggers {
        if let Some(e) = t.execute.as_deref() {
            walk(e, &mut out);
        }
    }
    out
}

/// Every `CastFromZone` in the parse, as `(target, mode, duration)`.
fn grants(parsed: &ParsedAbilities) -> Vec<(&TargetFilter, CardPlayMode, Option<Duration>)> {
    all_defs(parsed)
        .into_iter()
        .filter_map(|d| match &*d.effect {
            Effect::CastFromZone {
                target,
                mode,
                duration,
                ..
            } => Some((target, *mode, duration.clone())),
            _ => None,
        })
        .collect()
}

/// The grants that let a player PLAY a card (the land half), as filters.
fn play_grants(parsed: &ParsedAbilities) -> Vec<&TargetFilter> {
    grants(parsed)
        .into_iter()
        .filter(|(_, mode, _)| *mode == CardPlayMode::Play)
        .map(|(t, _, _)| t)
        .collect()
}

/// Fragments the parser refused, lowercased.
fn unimplemented_descriptions(parsed: &ParsedAbilities) -> Vec<String> {
    all_defs(parsed)
        .into_iter()
        .filter_map(|d| match &*d.effect {
            Effect::Unimplemented { description, .. } => {
                description.as_deref().map(|s| s.to_ascii_lowercase())
            }
            _ => None,
        })
        .collect()
}

fn typed(filter: &TargetFilter) -> &TypedFilter {
    match filter {
        TargetFilter::Typed(t) => t,
        other => panic!("expected a Typed filter, got {other:?}"),
    }
}

fn is_graveyard_anchored(filter: &TypedFilter) -> bool {
    filter.properties.iter().any(|p| {
        matches!(
            p,
            FilterProp::InZone {
                zone: Zone::Graveyard,
                ..
            }
        )
    })
}

// ── Fixtures ──────────────────────────────────────────────────────────────
// Verbatim Scryfall Oracle text.

const YAWGMOTHS_WILL: &str = "Until end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const GAEAS_WILL: &str = "Suspend 4—{G}\nUntil end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const MAGUS_OF_THE_WILL: &str = "{2}{B}, {T}, Exile this creature: Until end of turn, you may play lands and cast spells from your graveyard. If a card would be put into your graveyard from anywhere this turn, exile that card instead.";

// HOSTILE — TARGETED permissions (CR 115.1). Same "cast … from your graveyard"
// shape, but each names objects chosen on announcement, so neither is a
// class-wide grant and neither has a land half to synthesize.
const SINS_OF_THE_PAST: &str = "Until end of turn, you may cast target instant or sorcery card from your graveyard without paying its mana cost. If that spell would be put into your graveyard, exile it instead. Exile Sins of the Past.";
const RAT_IN_THE_HAT: &str = "{T}, Sacrifice this creature: Until end of turn, you may cast target creature card that has a hat from your graveyard.";

// HOSTILE — the SAME coordinated "play lands and cast spells" grammar over a
// BATCH of specific cards ("from among cards exiled this way"), not a zone
// class. Prior art in `kiora_self_library_peek_cast.rs` pins their behaviour;
// this file's job is to prove the new pass does not disturb them.
const MAGUS_OF_THE_MIND: &str = "{U}, {T}, Sacrifice this creature: Shuffle your library, then exile the top X cards, where X is one plus the number of spells cast this turn. Until end of turn, you may play lands and cast spells from among cards exiled this way without paying their mana costs.";
const GIX_YAWGMOTH_PRAETOR: &str = "Whenever a creature deals combat damage to one of your opponents, its controller may pay 1 life. If they do, they draw a card.\n{4}{B}{B}{B}, Discard X cards: Exile the top X cards of target opponent's library. You may play lands and cast spells from among cards exiled this way without paying their mana costs.";

// HOSTILE — a DIFFERENT zone (library top) reached through a trigger, carrying
// the same coordinated grammar plus a look-at rider.
const THE_BELLIGERENT: &str = "Whenever The Belligerent attacks, create a Treasure token. Until end of turn, you may look at the top card of your library any time, and you may play lands and cast spells from the top of your library.";

// ── D — the discriminating rows ───────────────────────────────────────────

#[test]
fn d1_will_cycle_gains_the_land_half_of_its_coordinated_grant() {
    // MULTI-AUTHORITY hostile fixture: three arrival shapes — a bare sorcery, a
    // sorcery preceded by a Suspend line, and a creature's ACTIVATED ability.
    // All three must reach the same verdict, proving the outcome keys on the
    // permission body rather than on AbilityKind, cost presence, or line index.
    for (text, name, types) in [
        (YAWGMOTHS_WILL, "Yawgmoth's Will", &["Sorcery"][..]),
        (GAEAS_WILL, "Gaea's Will", &["Sorcery"][..]),
        (MAGUS_OF_THE_WILL, "Magus of the Will", &["Creature"][..]),
    ] {
        let parsed = parse(text, name, types);

        // (i) The land half now exists. This is the whole point of the change:
        // before it, the fragment "play lands" was refused outright.
        let plays = play_grants(&parsed);
        assert_eq!(
            plays.len(),
            1,
            "{name}: CR 116.2a - the coordinated grant's land half must lower to exactly one Play grant"
        );

        // (ii) It grants LANDS, not the cast half's bare `Card`. A land half
        // that kept `Card` would silently widen "play lands" into "play
        // anything", a strictly worse bug than refusing the clause.
        let land = typed(plays[0]);
        assert_eq!(
            land.type_filters,
            vec![TypeFilter::Land],
            "{name}: CR 116.2a - the land half must be restricted to lands"
        );

        // (iii) It stays anchored to the zone the sentence named. A land half
        // with no zone anchor grants playing lands from anywhere.
        assert!(
            is_graveyard_anchored(land),
            "{name}: the land half must stay anchored to the graveyard, got {:?}",
            land.properties
        );

        // (iv) It is YOUR graveyard (CR 109.5), not every graveyard.
        assert_eq!(
            land.controller,
            Some(ControllerRef::You),
            "{name}: CR 109.5 - 'your graveyard' scopes to the permission's controller"
        );

        // (v) CR 611.2a: one stated window scopes BOTH halves. An unstamped
        // duration is not "missing", it lasts until the end of the GAME — so a
        // dropped window here makes the grant permanent, which is worse than
        // not parsing it at all.
        let windows: Vec<_> = grants(&parsed).into_iter().map(|(_, _, d)| d).collect();
        assert!(
            windows.iter().all(|d| *d == Some(Duration::UntilEndOfTurn)),
            "{name}: CR 611.2a - every half must carry the stated one-turn window, got {windows:?}"
        );

        // (vi) The refused fragment is gone. Its survival would mean the pass
        // added a grant while leaving the original refusal in the chain, so one
        // clause would report both a permission and a parse gap.
        let refused = unimplemented_descriptions(&parsed);
        assert!(
            !refused.iter().any(|d| d == "play lands"),
            "{name}: the recovered land half must replace the refused fragment, got {refused:?}"
        );
    }
}

#[test]
fn d2_the_cast_half_is_unchanged_and_was_never_the_gap() {
    // The honest record of what this change did NOT do. The cast half already
    // worked; a regression here would mean the pass replaced a working half
    // rather than adding the missing one — the exact failure mode of the
    // previous attempt at this task.
    for (text, name, types) in [
        (YAWGMOTHS_WILL, "Yawgmoth's Will", &["Sorcery"][..]),
        (GAEAS_WILL, "Gaea's Will", &["Sorcery"][..]),
        (MAGUS_OF_THE_WILL, "Magus of the Will", &["Creature"][..]),
    ] {
        let parsed = parse(text, name, types);
        let casts: Vec<_> = grants(&parsed)
            .into_iter()
            .filter(|(_, mode, _)| *mode == CardPlayMode::Cast)
            .collect();
        assert_eq!(
            casts.len(),
            1,
            "{name}: CR 601.2a - the cast half must survive untouched"
        );
        // The cast half stays CLASS-WIDE (`Card`): "cast spells from your
        // graveyard" is every card there, and narrowing it to match the land
        // half would break the card in the opposite direction.
        assert_eq!(
            typed(casts[0].0).type_filters,
            vec![TypeFilter::Card],
            "{name}: the cast half must remain the class-wide `Card` axis"
        );
    }
}

#[test]
fn g5_a_sibling_with_no_stated_window_gains_no_land_half() {
    // MEASURED COLLATERAL. An earlier revision of this pass flipped Shaman's
    // Trance to "supported", and an earlier version of this test claimed the
    // flip was earned. It was not, and the claim was wrong on two axes.
    //
    // WINDOW. The card prints "this turn", but its cast sibling lowered with
    // `duration: None`. CR 611.2a: "If no duration is stated, it lasts until the
    // end of the game." Copying that absence synthesizes a PERMANENT land
    // permission — the failure this suite's own d1 row (v) calls out as worse
    // than not parsing at all.
    //
    // GRAVEYARD. The sibling carries `controller: Some(You)`, which selects
    // objects in YOUR graveyard: `casting.rs::graveyard_lands_playable_by_permission`
    // iterates `player_data.graveyard` for the acting player and applies the
    // filter to those objects, so `controller` picks WHICH OBJECTS MATCH, not
    // who acts. That contradicts the card's "other players' graveyards".
    // Corpus census over graveyard `CastFromZone` grants: 47 `You`, 15 `None`,
    // zero `Opponent` — every genuine cross-player card (Chancellor of the
    // Spires, Memory Plunder, Havengul Lich) sits in the `None` bucket.
    //
    // So the pass declines on the missing window, and the card stays honestly
    // unsupported. Making it genuinely correct needs a runtime widening to other
    // players' graveyards plus the "this turn" window — a separate change, not a
    // filter copy.
    let parsed = parse(
        "Other players can't play lands or cast spells from their graveyards this turn. You may play lands and cast spells from other players' graveyards this turn as though those cards were in your graveyard.",
        "Shaman's Trance",
        &["Instant"],
    );
    assert!(
        play_grants(&parsed).is_empty(),
        "CR 611.2a: a sibling with no stated window must not yield a permanent land grant, got {:?}",
        play_grants(&parsed)
    );

    // REACH-GUARD (alpha): the fixture really does reach the pass — it still
    // carries the refused fragment the pass keys on, so the emptiness above is
    // the duration requirement declining rather than the input never arriving.
    assert!(
        unimplemented_descriptions(&parsed)
            .iter()
            .any(|d| d == "play lands"),
        "reach-guard: the fixture must still carry the refused fragment"
    );

    // WHY THERE IS NO "same sentence minus the window" MINIMAL PAIR HERE.
    // MEASURED: dropping the leading window from this grammar does not produce
    // an un-windowed `CastFromZone` sibling — it changes WHICH PARSER handles
    // the sentence. "You may play lands and cast spells from your graveyard."
    // lowers to a STATIC permission (`statics == 1`, zero abilities), so it
    // never reaches this pass at all. That is B1's `v1` result from the other
    // direction: the headless form was always the static parser's, and the
    // leading duration head is what routes the sentence through the effect path.
    //
    // So the duration guard's discriminating evidence is the pair of REAL cards
    // below and in `d1` — Shaman's Trance (eligible filter, no effect duration →
    // refused) against the Will cycle (same filter shape, window present →
    // recovered) — not a synthetic sentence pair that cannot exist.

    // REACH-GUARD (beta): the same grammar WITH a stated window does produce a
    // land half, so the decline keys on the missing duration and not on some
    // unrelated property of this sentence.
    let windowed = parse(
        "Until end of turn, you may play lands and cast spells from your graveyard.",
        "Windowed Probe",
        &["Sorcery"],
    );
    assert_eq!(
        play_grants(&windowed).len(),
        1,
        "reach-guard: the same grammar WITH a stated window must produce the land half"
    );
}

// ── G — regression guards ─────────────────────────────────────────────────
// Every row below is a NEGATIVE. Each carries a positive reach-guard, because
// a negative assertion is also satisfied by a dead instrument.

#[test]
fn g1_targeted_permissions_gain_no_land_half() {
    // CR 115.1: these name objects chosen on announcement. Synthesizing a land
    // half would invent a class-wide "play any land from your graveyard" the
    // card never granted — the most damaging way this pass could fail, because
    // it would read as new coverage rather than as a bug.
    for (text, name, types) in [
        (SINS_OF_THE_PAST, "Sins of the Past", &["Sorcery"][..]),
        (RAT_IN_THE_HAT, "Rat in the Hat", &["Creature"][..]),
    ] {
        let parsed = parse(text, name, types);
        assert!(
            play_grants(&parsed).is_empty(),
            "{name}: CR 115.1 - a targeted permission must not gain a class-wide land half"
        );
    }

    // REACH-GUARD: the instrument CAN see a Play grant. Without this, "no play
    // grants" is equally satisfied by a `play_grants` that never matches.
    let reach = parse(YAWGMOTHS_WILL, "Yawgmoth's Will", &["Sorcery"]);
    assert_eq!(
        play_grants(&reach).len(),
        1,
        "reach-guard: play_grants must be able to see a Play grant"
    );
}

#[test]
fn g2_batch_grants_over_a_fixed_card_set_gain_no_graveyard_half() {
    // "from among cards exiled this way" is a BATCH over specific objects, not
    // a zone class, and The Belligerent names the library top. Prior art
    // (`kiora_self_library_peek_cast.rs`) owns their behaviour; this row's only
    // claim is that the new pass did not disturb it. None of the three names
    // the graveyard, so a graveyard-anchored play grant here could only have
    // been invented by this pass.
    for (text, name, types) in [
        (MAGUS_OF_THE_MIND, "Magus of the Mind", &["Creature"][..]),
        (
            GIX_YAWGMOTH_PRAETOR,
            "Gix, Yawgmoth Praetor",
            &["Creature", "Legendary"][..],
        ),
        (THE_BELLIGERENT, "The Belligerent", &["Artifact"][..]),
    ] {
        let parsed = parse(text, name, types);
        for filter in play_grants(&parsed) {
            assert!(
                !is_graveyard_anchored(typed(filter)),
                "{name}: no graveyard land grant may be synthesized here, got {:?}",
                typed(filter).properties
            );
        }
    }

    // REACH-GUARD: the graveyard detector CAN fire, so the negatives above are
    // the pass being correctly scoped rather than a predicate that never matches.
    let reach = parse(YAWGMOTHS_WILL, "Yawgmoth's Will", &["Sorcery"]);
    assert!(
        play_grants(&reach)
            .into_iter()
            .any(|f| is_graveyard_anchored(typed(f))),
        "reach-guard: is_graveyard_anchored must be able to fire"
    );
}

#[test]
fn g3_a_zoneless_play_lands_stays_refused() {
    // The CR 305.2a guard's own case, pinned. The pass must recover the land
    // half ONLY when a sibling supplies the zone; a bare "play lands" with no
    // zone clause has no grant to recover and must stay refused, or the pass
    // would be inventing permissions rather than reassembling one.
    let parsed = parse(
        "Until end of turn, you may play lands.",
        "Zoneless Probe",
        &["Sorcery"],
    );
    assert!(
        play_grants(&parsed).is_empty(),
        "CR 305.2a: a zone-less 'play lands' must not become a grant"
    );

    // REACH-GUARD: the same sentence WITH a zone does produce one, so the
    // negative above is the guard working rather than the parser being dead to
    // this whole grammar.
    let anchored = parse(
        "Until end of turn, you may play lands and cast spells from your graveyard.",
        "Anchored Probe",
        &["Sorcery"],
    );
    assert_eq!(
        play_grants(&anchored).len(),
        1,
        "reach-guard: the same grammar WITH a zone must produce the land half"
    );
}

#[test]
fn g4_a_grant_scoped_to_a_chosen_pile_gains_no_land_half() {
    // MEASURED COLLATERAL, pinned. Brilliant Ultimatum carries the same
    // coordinated "you may play lands and cast spells" grammar, but scopes it to
    // "one of those piles" — a chosen subset of exiled cards, not a zone. Its
    // `CastFromZone` therefore carries no `InZone` property.
    //
    // Copying that empty property list would synthesize a land grant with NO
    // zone anchor, which reads as "play lands from anywhere" — strictly broader
    // than the card, and the one way this pass could invent a permission rather
    // than reassemble one. `land_half_filter` requires an explicit zone, so the
    // fragment stays honestly refused instead.
    //
    // TWO FIXTURES, because the printed card cannot isolate the zone axis. Its
    // sibling also carries no duration, so the pass short-circuits on the CR
    // 611.2a window check (see `g5`) before `land_half_filter` is ever reached —
    // measured: removing the `InZone` requirement entirely leaves the printed-card
    // row passing. The windowed variant below is eligible on every OTHER axis, so
    // it is the row that actually exercises the zone requirement.
    //
    // The printed card stays as a corpus-truth row: verbatim Oracle text, real
    // parse, and the outcome the coverage gate sees.
    let parsed = parse(
        "Exile the top five cards of your library. An opponent separates those cards into two piles. You may play lands and cast spells from one of those piles. If you cast a spell this way, you cast it without paying its mana cost.",
        "Brilliant Ultimatum",
        &["Sorcery"],
    );
    // The load-bearing assertion: NO land half at all. Iterating the grants and
    // checking a property of each would pass vacuously here, because the
    // expected result is an empty list and a `for` over it never runs its body.
    assert!(
        play_grants(&parsed).is_empty(),
        "CR 116.2a: a chosen-pile grant must not gain a synthesized land half, got {:?}",
        play_grants(&parsed)
    );

    // REACH-GUARD (alpha): the fixture really does reach the pass — it still
    // carries the refused fragment the pass keys on, so the emptiness above is
    // a guard declining rather than the input never arriving.
    assert!(
        unimplemented_descriptions(&parsed)
            .iter()
            .any(|d| d == "play lands"),
        "reach-guard: the fixture must still carry the refused fragment"
    );

    // THE ROW THAT ACTUALLY EXERCISES THE ZONE REQUIREMENT. Same chosen-pile
    // scope, but WITH a stated window, so it clears the CR 611.2a check and
    // reaches `land_half_filter` — where the missing `InZone` is the only reason
    // it can decline. Without this row, removing the zone requirement outright
    // leaves the whole test passing.
    let windowed_pile = parse(
        "Exile the top five cards of your library. An opponent separates those cards into two piles. Until end of turn, you may play lands and cast spells from one of those piles.",
        "Windowed Pile Probe",
        &["Sorcery"],
    );
    assert!(
        play_grants(&windowed_pile).is_empty(),
        "CR 116.2a: a chosen-pile grant must not gain a land half even WITH a window, got {:?}",
        play_grants(&windowed_pile)
    );
    assert!(
        unimplemented_descriptions(&windowed_pile)
            .iter()
            .any(|d| d == "play lands"),
        "reach-guard: the windowed pile fixture must still carry the refused fragment"
    );

    // REACH-GUARD (beta): the same sentence with a real ZONE in place of the
    // pile does produce a land half, so the emptiness above is
    // `land_half_filter`'s zone requirement declining this input and not
    // `play_grants` being blind to grants on this card shape.
    //
    // The window is load-bearing in this fixture: the pass also requires a
    // stated duration (CR 611.2a, see `g5`), so a probe without one would
    // decline for that reason instead and prove nothing about the zone axis.
    let anchored = parse(
        "Exile the top five cards of your library. Until end of turn, you may play lands and cast spells from your graveyard. If you cast a spell this way, you cast it without paying its mana cost.",
        "Pile Probe Reach Guard",
        &["Sorcery"],
    );
    assert_eq!(
        play_grants(&anchored).len(),
        1,
        "reach-guard: the same grammar over a real zone must still produce the land half"
    );
}
