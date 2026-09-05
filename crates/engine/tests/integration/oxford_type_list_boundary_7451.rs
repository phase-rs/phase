//! Issue #7451 — the trigger condition/effect boundary must span the WHOLE
//! Oxford-comma type list in a trigger's EFFECT subject, not truncate to the
//! list's last item. `oracle_trigger.rs::is_new_sentence_not_type_continuation`
//! walks past the list's own commas instead of stopping at the first one, so
//! before the fix the effect handed downstream keeps only the FINAL list item.
//!
//! V1 drives the real Oracle parse -> `split_trigger` -> effect chain ->
//! `Effect::PumpAll` -> `evaluate_layers` pipeline and is revert-failing. V2
//! and V4 are PINS: their production seams are unaffected by this change
//! (protection lists and quantity counts are already correct at the arity the
//! issue complains about), included here so the new file documents the whole
//! issue rather than only the piece U1 repairs.
//!
//! Oracle text is verbatim, fetched from `client/public/card-data.json` at the
//! branch base.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::combat::AttackTarget;
use engine::game::effects::resolve_ability_chain;
use engine::game::keywords::source_matches_card_type;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ContinuousModification, Effect, TargetFilter, TypeFilter,
};
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const VALLEY_FLOODCALLER: &str = "Flash\nYou may cast noncreature spells as though they had flash.\nWhenever you cast a noncreature spell, Birds, Frogs, Otters, and Rats you control get +1/+1 until end of turn. Untap them.";

const VALLEY_ROTCALLER: &str = "Menace\nWhenever this creature attacks, each opponent loses X life and you gain X life, where X is the number of other Squirrels, Bats, Lizards, and Rats you control.";

const WHELMING_WAVE: &str = "Return all creatures to their owners' hands except for Krakens, Leviathans, Octopuses, and Serpents.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let object = &runner.state().objects[&id];
    (
        object.power.expect("creature has power"),
        object.toughness.expect("creature has toughness"),
    )
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// V1 — Valley Floodcaller's cast trigger must pump every listed subtype, not
/// just the last one in the Oxford-comma list. Revert-failing: before the fix,
/// `find_effect_boundary` walks past the list's own commas and lands on the
/// LAST one, so the effect handed to the effect parser is only
/// `"Rats you control get +1/+1 until end of turn"` — Bird, Frog, Otter and
/// the self-inclusive Floodcaller are all left unpumped.
#[test]
fn valley_floodcaller_pumps_every_listed_subtype() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bird = scenario
        .add_creature(P0, "Birdy", 2, 2)
        .with_subtypes(vec!["Bird"])
        .id();
    let frog = scenario
        .add_creature(P0, "Froggy", 2, 2)
        .with_subtypes(vec!["Frog"])
        .id();
    let otter = scenario
        .add_creature(P0, "Ottery", 2, 2)
        .with_subtypes(vec!["Otter"])
        .id();
    let rat = scenario
        .add_creature(P0, "Ratty", 2, 2)
        .with_subtypes(vec!["Rat"])
        .id();
    let bear = scenario.add_creature(P0, "Beary", 2, 2).id();
    let floodcaller = scenario
        .add_creature_from_oracle(P0, "Valley Floodcaller", 2, 2, VALLEY_FLOODCALLER)
        .with_subtypes(vec!["Otter", "Wizard"]) // CR 205.3m: the printed type line
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner: GameRunner = scenario.build();

    runner.cast(bolt).target_player(P1).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        effective_pt(&mut runner, bird),
        (3, 3),
        "Bird must be pumped"
    );
    assert_eq!(
        effective_pt(&mut runner, frog),
        (3, 3),
        "Frog must be pumped"
    );
    assert_eq!(
        effective_pt(&mut runner, otter),
        (3, 3),
        "Otter must be pumped"
    );
    assert_eq!(effective_pt(&mut runner, rat), (3, 3), "Rat must be pumped");
    assert_eq!(
        effective_pt(&mut runner, floodcaller),
        (3, 3),
        "Valley Floodcaller is itself an Otter (CR 205.3m) and the filter carries \
         no FilterProp::Another, so the source is inside its own pumped \
         population"
    );
    // Paired positive reach-guard: at least one creature's P/T actually changed
    // above, so this negative is not vacuous.
    assert_eq!(
        effective_pt(&mut runner, bear),
        (2, 2),
        "a plain creature outside all four listed subtypes must stay unpumped"
    );
}

/// V2 — protection-list arity PIN. This seam (`expand_protection_parts` /
/// `Keyword::Protection` / `source_matches_card_type`) is untouched by this
/// change; it is already correct at arity today. Scoped to arity + per-member
/// resolution of the three subtype members; makes no claim about a
/// non-card-type quality (Tinfoil Helm's "hybrid mana", a separate,
/// pre-existing gap).
#[test]
fn protection_from_subtype_list_keeps_every_member() {
    let parsed = parse_oracle_text(
        "This creature has protection from Krakens, Leviathans, and Serpents.",
        "Oxford Ward",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let qualities: Vec<String> = parsed
        .statics
        .iter()
        .flat_map(|d| d.modifications.iter())
        .filter_map(|m| match m {
            ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::CardType(q)),
            } => Some(q.clone()),
            ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Quality(q)),
            } => Some(q.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        qualities.len(),
        3,
        "expected exactly three Protection modifications, in printed order, got {qualities:?}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kraken = scenario
        .add_creature(P0, "Test Kraken", 1, 1)
        .with_subtypes(vec!["Kraken"])
        .id();
    let leviathan = scenario
        .add_creature(P0, "Test Leviathan", 1, 1)
        .with_subtypes(vec!["Leviathan"])
        .id();
    let serpent = scenario
        .add_creature(P0, "Test Serpent", 1, 1)
        .with_subtypes(vec!["Serpent"])
        .id();
    let runner: GameRunner = scenario.build();

    let kraken_obj = &runner.state().objects[&kraken];
    let leviathan_obj = &runner.state().objects[&leviathan];
    let serpent_obj = &runner.state().objects[&serpent];

    assert!(
        source_matches_card_type(kraken_obj, &qualities[0]),
        "the first listed quality must match a Kraken"
    );
    assert!(
        source_matches_card_type(leviathan_obj, &qualities[1]),
        "the second listed quality must match a Leviathan"
    );
    assert!(
        source_matches_card_type(serpent_obj, &qualities[2]),
        "the third listed quality must match a Serpent"
    );
    // Paired negative: per-member resolution, not a wildcard match.
    assert!(
        !source_matches_card_type(kraken_obj, &qualities[2]),
        "a Kraken must NOT match the third (Serpent) quality"
    );
}

/// V4 — quantity-count PIN. `QuantityRef::ObjectCount` / `game/quantity.rs` is
/// untouched by this change: Valley Rotcaller's own `"attacks,"` boundary
/// already isolates the effect well before the four-subtype list is reached,
/// so the count is already computed over the whole list today. The negative
/// sibling is the reach-guard: it proves the count is genuinely computed, not
/// a hard-coded four.
#[test]
fn rotcaller_counts_every_listed_subtype() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Squirrelly", 1, 1)
        .with_subtypes(vec!["Squirrel"]);
    scenario
        .add_creature(P0, "Batty", 1, 1)
        .with_subtypes(vec!["Bat"]);
    scenario
        .add_creature(P0, "Lizardy", 1, 1)
        .with_subtypes(vec!["Lizard"]);
    scenario
        .add_creature(P0, "Ratty", 1, 1)
        .with_subtypes(vec!["Rat"]);
    scenario.add_creature(P0, "Beary", 2, 2);
    let rotcaller = scenario
        .add_creature_from_oracle(P0, "Valley Rotcaller", 1, 3, VALLEY_ROTCALLER)
        .with_subtypes(vec!["Squirrel", "Warlock"]) // CR 205.3m: the printed type line
        .id();
    let mut runner: GameRunner = scenario.build();

    let p0_before = life(&runner, P0);
    let p1_before = life(&runner, P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(rotcaller, AttackTarget::Player(P1))])
        .expect("declaring the sole attacker should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1) - p1_before,
        -4,
        "each opponent must lose X life, X = the four OTHER listed-subtype creatures \
         (Rotcaller itself excluded by \"other\"; the Bear excluded by subtype)"
    );
    assert_eq!(
        life(&runner, P0) - p0_before,
        4,
        "the attacking player must gain the same X"
    );
}

/// Negative sibling of [`rotcaller_counts_every_listed_subtype`] and its reach
/// guard: with only ONE listed-subtype creature on board, X must be exactly
/// 1, not the four-creature figure the row above pins.
#[test]
fn rotcaller_counts_exactly_one_when_only_one_listed_subtype_present() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Ratty", 1, 1)
        .with_subtypes(vec!["Rat"]);
    let rotcaller = scenario
        .add_creature_from_oracle(P0, "Valley Rotcaller", 1, 3, VALLEY_ROTCALLER)
        .with_subtypes(vec!["Squirrel", "Warlock"])
        .id();
    let mut runner: GameRunner = scenario.build();

    let p0_before = life(&runner, P0);
    let p1_before = life(&runner, P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(rotcaller, AttackTarget::Player(P1))])
        .expect("declaring the sole attacker should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(life(&runner, P1) - p1_before, -1, "X must be 1, not 4");
    assert_eq!(life(&runner, P0) - p0_before, 1, "X must be 1, not 4");
}

/// V3b — issue #7451 U2 ALONE (no U3): The Argent Etchings chapter III,
/// verbatim substring from its `card-data.json` back-face `oracle_text`
/// (`Elesh Norn // The Argent Etchings` is a transforming DFC; The Argent
/// Etchings is the BACK face, keyed under its own `"the argent etchings"`
/// entry): "Destroy all other permanents except for artifacts, lands, and
/// Phyrexians." This is the ADJACENT surface order — the except-for clause
/// immediately follows the type list, with no intervening destination
/// clause — so it exercises the pure U2 vocabulary-gate fix, isolated from
/// U3's post-destination remainder handling. Chapter-driving recipe follows
/// `issue_2425_fable_chapter_iii_transform.rs::fable_chapter_three_returns_transformed_not_as_saga`:
/// the Saga source is built directly via `create_object` (no saga/lore
/// helper exists on `GameScenario`), and the chapter's own sentence is fed to
/// `parse_effect_chain` -> `build_resolved_from_def` -> `resolve_ability_chain`
/// rather than through a cast pipeline.
///
/// Revert-failing: today the whole exclusion clause declines the moment the
/// unrecognised "Phyrexians" item is reached (the mixed core-type + subtype
/// list makes `parse_except_for_type_list_suffix` reject wholesale), so the
/// artifact, the lands and the Phyrexian creature are destroyed alongside the
/// Bears instead of being spared. Positive reach-guard: the parsed effect is
/// asserted to be `Effect::DestroyAll` over a `Typed` filter carrying all
/// three exclusions BEFORE the board is checked, so this cannot pass on a
/// `None` parse, an `Effect::Unimplemented`, or a renamed variant.
#[test]
fn argent_etchings_iii_spares_artifacts_lands_and_phyrexians() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let artifact = scenario
        .add_artifact_from_oracle(P0, "Argent Etchings Artifact", "")
        .id();
    let land = scenario
        .add_land_from_oracle(P0, "Argent Etchings Land", "")
        .id();
    let phyrexian = scenario
        .add_creature(P0, "Phyrexian Test Creature", 2, 2)
        .with_subtypes(vec!["Phyrexian"])
        .id();
    let p0_bear = scenario.add_creature(P0, "P0 Beary", 2, 2).id();

    let p1_bear = scenario.add_creature(P1, "P1 Beary", 2, 2).id();
    let p1_land = scenario.add_land_from_oracle(P1, "P1 Land", "").id();

    let mut runner: GameRunner = scenario.build();

    let saga_id = {
        let state = runner.state_mut();
        let id = create_object(
            state,
            CardId(1000),
            P0,
            "The Argent Etchings".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.base_card_types = obj.card_types.clone();
        id
    };

    let execute = parse_effect_chain(
        "Destroy all other permanents except for artifacts, lands, and Phyrexians.",
        AbilityKind::Spell,
    );

    // Positive reach-guard: confirm this text actually reaches the
    // except-for vocabulary gate — and keeps all three exclusions — rather
    // than declining or landing on a different effect shape, before touching
    // the board at all.
    match &*execute.effect {
        Effect::DestroyAll { target, .. } => {
            match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                    assert!(tf
                        .type_filters
                        .contains(&TypeFilter::Non(Box::new(TypeFilter::Artifact))));
                    assert!(tf
                        .type_filters
                        .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
                    assert!(tf.type_filters.contains(&TypeFilter::Non(Box::new(
                        TypeFilter::Subtype("Phyrexian".to_string())
                    ))));
                }
                other => panic!("expected a Typed target filter, got {other:?}"),
            }
        }
        other => panic!("expected Effect::DestroyAll, got {other:?}"),
    }

    let resolved = build_resolved_from_def(&execute, saga_id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter III's destroy clause resolves");

    let zone_of = |id: ObjectId, runner: &GameRunner| runner.state().objects[&id].zone;

    assert_eq!(
        zone_of(p0_bear, &runner),
        Zone::Graveyard,
        "P0's plain Bear must be destroyed"
    );
    assert_eq!(
        zone_of(p1_bear, &runner),
        Zone::Graveyard,
        "P1's plain Bear must be destroyed"
    );
    assert_eq!(
        zone_of(artifact, &runner),
        Zone::Battlefield,
        "the artifact must be spared"
    );
    assert_eq!(
        zone_of(land, &runner),
        Zone::Battlefield,
        "P0's land must be spared"
    );
    assert_eq!(
        zone_of(p1_land, &runner),
        Zone::Battlefield,
        "P1's land must be spared"
    );
    assert_eq!(
        zone_of(phyrexian, &runner),
        Zone::Battlefield,
        "the Phyrexian creature must be spared"
    );
}

/// V3a — issue #7451 U3: the POST-DESTINATION `except for` exclusion
/// (Whelming Wave, Slinn Voda, Cyclone Summoner class). Whelming Wave's
/// destination phrase ("to their owners' hands") sits BETWEEN the type list
/// and the exclusion clause, so `dest_remainder` — not `target_text` — carries
/// "except for Krakens, Leviathans, Octopuses, and Serpents." Before U3,
/// nothing but the battlefield attach-host probe ever read `dest_remainder`,
/// so the exclusion was silently dropped and every creature was bounced,
/// including the four exempted subtypes.
///
/// Owner-vs-controller hostile fixture: `Effect::BounceAll`'s population
/// carries no controller restriction (`controller: None`), so `P1`'s
/// exempted Kraken must ALSO survive, not just `P0`'s.
///
/// Revert-failing: before the fix every creature (both Bears AND all five
/// exempted creatures) is bounced to its owner's hand.
#[test]
fn whelming_wave_spares_every_exempted_subtype() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let p0_kraken = scenario
        .add_creature(P0, "P0 Test Kraken", 4, 4)
        .with_subtypes(vec!["Kraken"])
        .id();
    let p0_leviathan = scenario
        .add_creature(P0, "P0 Test Leviathan", 4, 4)
        .with_subtypes(vec!["Leviathan"])
        .id();
    let p0_octopus = scenario
        .add_creature(P0, "P0 Test Octopus", 4, 4)
        .with_subtypes(vec!["Octopus"])
        .id();
    let p0_serpent = scenario
        .add_creature(P0, "P0 Test Serpent", 4, 4)
        .with_subtypes(vec!["Serpent"])
        .id();
    let p0_bear = scenario.add_creature(P0, "P0 Beary", 2, 2).id();

    let p1_kraken = scenario
        .add_creature(P1, "P1 Test Kraken", 4, 4)
        .with_subtypes(vec!["Kraken"])
        .id();
    let p1_bear = scenario.add_creature(P1, "P1 Beary", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Whelming Wave", false, WHELMING_WAVE)
        .id();

    let mut runner: GameRunner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    // Positive reach-guard: the hand count actually increased by 2 (one Bear
    // per owner), so the exemption negatives below cannot pass vacuously on a
    // spell that failed to resolve, or resolved as a no-op.
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "P0's Bear must return to P0's hand"
    );
    assert_eq!(
        outcome.hand_drawn(P1),
        1,
        "P1's Bear must return to P1's hand"
    );

    assert_eq!(
        outcome.zone_of(p0_bear),
        Zone::Hand,
        "P0's Bear must be bounced"
    );
    assert_eq!(
        outcome.zone_of(p1_bear),
        Zone::Hand,
        "P1's Bear must be bounced"
    );

    for (id, label) in [
        (p0_kraken, "P0's Kraken"),
        (p0_leviathan, "P0's Leviathan"),
        (p0_octopus, "P0's Octopus"),
        (p0_serpent, "P0's Serpent"),
        (
            p1_kraken,
            "P1's Kraken (owner vs controller: the population is uncontrolled)",
        ),
    ] {
        assert_eq!(
            outcome.zone_of(id),
            Zone::Battlefield,
            "{label} must be exempted from the bounce"
        );
    }
}

/// CR 205.3a: a REJECTED `except for` tail must not emit a mass return at all.
///
/// `parse_except_for_type_list_suffix` declines a card name or designation
/// ("except for Mageta") rather than emitting a vacuous `Non(Subtype("Mageta"))`.
/// The applicator used to treat that `None` identically to "no tail present" and
/// hand back the unchanged population — so this line produced an UNRESTRICTED
/// mass return that would bounce Mageta itself, while still reporting supported.
/// `swallow_check` has no "except for" detector, so nothing downstream flagged it.
/// Raised in review of PR #8336.
///
/// The positive control is the sibling row below: the identical grammar with an
/// ACCEPTED tail does produce `BounceAll`, so a failure here cannot be blamed on
/// the sentence shape.
#[test]
fn rejected_except_for_tail_does_not_emit_an_unrestricted_mass_return() {
    let def = parse_effect_chain(
        "Return all creatures to their owners' hands except for Mageta.",
        AbilityKind::Spell,
    );

    // Assert the HONEST outcome, not merely the absence of `BounceAll`: a bare
    // negative would also pass if a refactor routed this onto some other wrong
    // effect. `Unimplemented` is the coverage-honesty marker the blocker is about
    // — the card must report unsupported, not supported-and-widened.
    assert!(
        matches!(&*def.effect, Effect::Unimplemented { .. }),
        "a rejected exception tail must leave the card unsupported, got {:?}",
        def.effect
    );

    // Positive control: the same shape with an ACCEPTED tail still parses.
    let ok = parse_effect_chain(
        "Return all creatures to their owners' hands except for Krakens.",
        AbilityKind::Spell,
    );
    let Effect::BounceAll { target, .. } = &*ok.effect else {
        panic!(
            "control: an accepted tail must still produce BounceAll, got {:?}",
            ok.effect
        );
    };
    let TargetFilter::Typed(tf) = target else {
        panic!("control: expected a Typed population, got {target:?}");
    };
    assert!(
        tf.type_filters.iter().any(|f| matches!(
            f,
            TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Subtype(ref s) if s == "Kraken")
        )),
        "control: the accepted tail must narrow the population, got {tf:?}"
    );
}

/// CR 205.3a + CR 608.2c: the exclusion reaches every leg of a DISJUNCTIVE mass
/// population, through the real parse pipeline.
///
/// "Return all artifacts and enchantments to their owners\' hands" (Reduce to
/// Dreams) is the attested shape that parses to `BounceAll` over a
/// `TargetFilter::Or`; this drives the same grammar with an `except for` tail.
/// Before PR #8336\'s review the helper returned any non-`Typed` population
/// untouched, so the spell would have bounced the exempted permanents while still
/// reporting supported. Companion to the helper-level
/// `except_for_exclusion_reaches_every_leg_of_a_disjunctive_population`; this row
/// proves the narrowing survives `parse_effect_chain`, not just a direct call.
///
/// Asserts the POSITIVE shape first — `BounceAll` over an `Or` with both legs
/// intact — so the per-leg exclusion assertions cannot pass vacuously on an
/// `Effect::Unimplemented`, a `None` parse, or a collapsed single-leg filter.
#[test]
fn disjunctive_mass_return_applies_the_exclusion_to_every_leg() {
    let def = parse_effect_chain(
        "Return all artifacts and enchantments to their owners' hands except for Clues.",
        AbilityKind::Spell,
    );

    let Effect::BounceAll { target, .. } = &*def.effect else {
        panic!("expected BounceAll, got {:?}", def.effect);
    };
    let TargetFilter::Or { filters } = target else {
        panic!("expected a disjunctive population to survive, got {target:?}");
    };
    assert_eq!(filters.len(), 2, "both legs must survive: {target:?}");

    for (i, leg) in filters.iter().enumerate() {
        let TargetFilter::Typed(tf) = leg else {
            panic!("leg {i} is not Typed: {leg:?}");
        };
        assert!(
            tf.type_filters.iter().any(|f| matches!(
                f,
                TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Subtype(ref s) if s == "Clue")
            )),
            "leg {i} must carry Non(Subtype(\"Clue\")), got {tf:?}"
        );
    }
}

/// A graveyard-origin mass return carries its `except for` exclusion too.
///
/// This arm builds `ReturnAllToZone` rather than `ReturnAll`, and originally did
/// not apply `apply_except_for_type_list_exclusion` — so the clause was parsed,
/// reported as supported, and then silently dropped, returning the very cards it
/// named. Raised in review of PR #8336. The exclusion is a POPULATION filter
/// (CR 205.3a + CR 608.2c), so it narrows the same set whichever zone the
/// resolver scans; "no attested printing" was a statement about reach, not
/// correctness, and reach is the wrong test for a clause the parser already
/// accepts.
///
/// Asserts the POSITIVE shape first — a `ChangeZoneAll` graveyard->hand form over
/// a `Typed` creature-card population — so the exclusion assertion cannot pass
/// vacuously on an `Effect::Unimplemented`, a `None` parse, or a renamed variant.
#[test]
fn graveyard_origin_mass_return_applies_the_exclusion() {
    let def = parse_effect_chain(
        "Return all creature cards from your graveyard to your hand except for Zombies.",
        AbilityKind::Spell,
    );

    let Effect::ChangeZoneAll {
        origin: Some(Zone::Graveyard),
        destination: Zone::Hand,
        target,
        ..
    } = &*def.effect
    else {
        panic!(
            "expected a graveyard-to-hand ChangeZoneAll, got {:?}",
            def.effect
        );
    };
    let TargetFilter::Typed(tf) = target else {
        panic!("expected a Typed creature-card population, got {target:?}");
    };
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "population must be creature cards, got {tf:?}"
    );

    // The exclusion must be APPLIED here, exactly as on the implicit-origin
    // sibling. An "except for" clause is a POPULATION filter; it narrows the same
    // set whichever zone the resolver scans. This arm used to drop it silently and
    // return the exempted cards anyway (PR review of issue #7451).
    assert!(
        tf.type_filters.iter().any(|f| matches!(
            f,
            TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Subtype(ref s) if s == "Zombie")
        )),
        "the graveyard-origin arm must apply the except-for exclusion, got {tf:?}"
    );
}
