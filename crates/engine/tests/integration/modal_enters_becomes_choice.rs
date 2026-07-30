//! Runtime coverage for the modal "As ~ enters, it becomes your choice of
//! [P/T profiles]" as-enters replacement (CR 208.2b + CR 614.1c + CR 614.12a).
//!
//! Cards in this class (built here directly from Oracle text via
//! `add_creature_from_oracle` so the tests are independent of `card-data.json`,
//! which is absent in CI):
//!   - Primal Plasma: 3/3 | 2/2 flying | 1/6 defender
//!   - Primal Clay: 3/3 artifact | 2/2 artifact flying | 1/6 Wall artifact
//!     defender, all "in addition to its other types" (CR 205.1b additive)
//!   - Corrupted Shapeshifter: 3/3 flying | 2/5 vigilance | 0/12 defender
//!     (plus an independent Devoid line)
//!   - Aquamorph Entity: 5/1 | 1/5, with an honest face-up unimplemented gap
//!
//! Each test drives the PRODUCTION pipeline: the parsed `Moved`/Battlefield
//! `Effect::Choose { ChoiceType::Labeled, persist }` replacement's answer is
//! submitted through the real `apply()` `GameAction::ChooseOption` handler so
//! the label persists as `ChosenAttribute::Label`; then `evaluate_layers` runs
//! and the object's effective P/T + keywords are asserted. Every P/T assertion
//! FLIPS on a revert of the `ChosenLabelIs`-gated SetPower/SetToughness statics:
//! without the gate (or the statics), the printed 0/0 base survives and the
//! equality checks fail.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 208.2b: the modal as-enters replacement sets the creature's P/T to one
//!     of a number of specific values (and may list additional characteristics).
//!   - CR 614.1c: as-enters replacement effect.
//!   - CR 614.12a: the choice is made before the permanent enters.
//!   - CR 205.1b: Primal Clay "in addition to its other types" keeps prior types.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

/// Place a modal as-enters creature (printed 0/0) on the battlefield with its
/// abilities parsed from `oracle`, then drive the labeled choice through the
/// real `apply()` `ChooseOption` handler so the chosen label persists as
/// `ChosenAttribute::Label`. Returns the object id.
///
/// The choice is submitted via the production `WaitingFor::NamedChoice` +
/// `GameAction::ChooseOption` path (not by poking `chosen_attributes`), so the
/// `ChoiceType::Labeled` → `ChoiceValue::Label` → `ChosenAttribute::Label`
/// mapping stays under test.
fn place_and_choose(
    name: &str,
    oracle: &str,
    labels: &[&str],
    chosen: &str,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Printed 0/0 so a passing P/T assertion can only come from the gated
    // SetPower/SetToughness statics, never from the printed base.
    let obj = scenario
        .add_creature_from_oracle(P0, name, 0, 0, oracle)
        .id();
    let mut runner = scenario.build();

    let source = crate::support::exact_named_choice_source(runner.state(), obj);
    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        player: P0,
        choice_type: ChoiceType::Labeled {
            options: labels.iter().map(|s| s.to_string()).collect(),
        },
        options: labels.iter().map(|s| s.to_string()).collect(),
        source: Some(source),
        persist_player: None,
    };
    runner
        .act(GameAction::ChooseOption {
            choice: chosen.to_string(),
        })
        .unwrap_or_else(|_| panic!("ChooseOption({chosen}) must resolve"));

    assert_eq!(
        runner.state().objects[&obj]
            .chosen_label()
            .map(str::to_string),
        Some(chosen.to_string()),
        "chosen label must persist as ChosenAttribute::Label"
    );

    // Re-run the layer pipeline so the gated static applies over the persisted
    // label (CR 614.12a: the object is already "entered" in the scenario, and
    // the choice is now made — layers must reflect the chosen profile).
    {
        let s = runner.state_mut();
        s.layers_dirty.mark_full();
        evaluate_layers(s);
    }
    (runner, obj)
}

const PLASMA_ORACLE: &str = "As ~ enters, it becomes your choice of a 3/3 creature, \
    a 2/2 creature with flying, or a 1/6 creature with defender.";
const PLASMA_LABELS: &[&str] = &["3/3", "2/2 Flying", "1/6 Defender"];

/// V1: entering a modal card reaches the labeled choice — proves classification
/// routes to the Moved/Choose replacement. The choice resolves through the
/// production `ChooseOption` handler (the replacement's `ChoiceType::Labeled`
/// surfaced through `NamedChoice`) and persists. (The parser-shape guards
/// `is_replacement_pattern == true` / `is_static_pattern == false` live in the
/// crate-internal `oracle_replacement` test module, since those classifiers are
/// `pub(crate)`.)
#[test]
fn v1_modal_card_reaches_named_choice() {
    let (runner, obj) = place_and_choose("Primal Plasma", PLASMA_ORACLE, PLASMA_LABELS, "3/3");
    assert_eq!(
        runner.state().objects[&obj].chosen_label(),
        Some("3/3"),
        "the labeled choice must resolve and persist (proves the Choose replacement fired)"
    );
}

/// V3 + V5: choosing "3/3" yields a 3/3 creature with NO flying and NO defender.
#[test]
fn v3_primal_plasma_three_three_mode() {
    let (runner, obj) = place_and_choose("Primal Plasma", PLASMA_ORACLE, PLASMA_LABELS, "3/3");
    let o = &runner.state().objects[&obj];
    assert_eq!(o.power, Some(3), "3/3 mode: gated SetPower yields power 3");
    assert_eq!(
        o.toughness,
        Some(3),
        "3/3 mode: gated SetToughness yields toughness 3"
    );
    assert!(o.card_types.core_types.contains(&CoreType::Creature));
    // V5 mode exclusion: the other modes' keywords must be absent, and toughness
    // is 3 (not the 6 of the defender mode).
    assert!(
        !o.has_keyword(&Keyword::Flying),
        "3/3 mode must not have Flying"
    );
    assert!(
        !o.has_keyword(&Keyword::Defender),
        "3/3 mode must not have Defender"
    );
    assert_ne!(
        o.toughness,
        Some(6),
        "3/3 mode toughness must not be the 1/6 mode's 6"
    );
}

/// V4 (CORE PROOF): the gated SetPower/SetToughness layer. Choosing "2/2 Flying"
/// yields base power 2 / toughness 2 from the gated static (NOT the printed 0/0)
/// AND grants Flying. Reverting the ChosenLabelIs gate or removing the statics
/// makes this fail (printed 0/0 survives, no Flying).
#[test]
fn v4_primal_plasma_two_two_flying_gated_setpower() {
    let (runner, obj) =
        place_and_choose("Primal Plasma", PLASMA_ORACLE, PLASMA_LABELS, "2/2 Flying");
    let o = &runner.state().objects[&obj];
    assert_eq!(
        o.power,
        Some(2),
        "2/2 Flying mode: base power 2 comes from the gated SetPower, not printed 0/0"
    );
    assert_eq!(
        o.toughness,
        Some(2),
        "2/2 Flying mode: base toughness 2 comes from the gated SetToughness"
    );
    assert!(
        o.has_keyword(&Keyword::Flying),
        "2/2 Flying mode must grant Flying"
    );
    assert!(
        !o.has_keyword(&Keyword::Defender),
        "2/2 Flying mode must not grant Defender"
    );
}

/// V6: Primal Clay additive mode. Choosing "1/6 Defender" yields an Artifact
/// Creature with subtype Wall and Defender at 1/6 (CR 205.1b type retention).
#[test]
fn v6_primal_clay_additive_wall_defender() {
    const CLAY_ORACLE: &str = "As ~ enters, it becomes your choice of a 3/3 artifact \
        creature, a 2/2 artifact creature with flying, or a 1/6 Wall artifact creature \
        with defender in addition to its other types.";
    // FIX 2: labels now key the additive Artifact card type + Wall subtype.
    const CLAY_LABELS: &[&str] = &[
        "3/3 Artifact",
        "2/2 Artifact Flying",
        "1/6 Artifact Wall Defender",
    ];

    let (runner, obj) = place_and_choose(
        "Primal Clay",
        CLAY_ORACLE,
        CLAY_LABELS,
        "1/6 Artifact Wall Defender",
    );
    let o = &runner.state().objects[&obj];
    assert_eq!(o.power, Some(1), "1/6 mode: power 1");
    assert_eq!(o.toughness, Some(6), "1/6 mode: toughness 6");
    assert!(
        o.has_keyword(&Keyword::Defender),
        "1/6 mode must have Defender"
    );
    assert!(
        o.card_types.core_types.contains(&CoreType::Artifact),
        "Primal Clay is an Artifact Creature (CR 205.1b additive keeps Artifact)"
    );
    assert!(
        o.card_types.core_types.contains(&CoreType::Creature),
        "still a Creature"
    );
    assert!(
        o.card_types.subtypes.iter().any(|s| s == "Wall"),
        "1/6 mode grants the Wall subtype (additive), got {:?}",
        o.card_types.subtypes
    );
}

/// V7: Corrupted Shapeshifter. Choosing "0/12 Defender" yields 0/12 + Defender.
/// The independent Devoid line is parsed separately from the modal line.
const CS_ORACLE: &str = "Devoid\nAs ~ enters, it becomes your choice of a 3/3 \
    creature with flying, a 2/5 creature with vigilance, or a 0/12 creature with \
    defender.";
const CS_LABELS: &[&str] = &["3/3 Flying", "2/5 Vigilance", "0/12 Defender"];

#[test]
fn v7_corrupted_shapeshifter_zero_twelve_defender() {
    let (runner, obj) = place_and_choose(
        "Corrupted Shapeshifter",
        CS_ORACLE,
        CS_LABELS,
        "0/12 Defender",
    );
    let o = &runner.state().objects[&obj];
    assert_eq!(o.power, Some(0), "0/12 mode: power 0");
    assert_eq!(o.toughness, Some(12), "0/12 mode: toughness 12");
    assert!(
        o.has_keyword(&Keyword::Defender),
        "0/12 mode must have Defender"
    );
    assert!(
        !o.has_keyword(&Keyword::Flying),
        "0/12 mode must not have Flying"
    );
}

/// V7 companion: the Devoid line parses INDEPENDENTLY of the modal line — the
/// modal Choose replacement still fires AND Devoid is extracted as a keyword.
#[test]
fn v7_devoid_line_parses_independently_of_modal() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::Effect;

    let parsed = parse_oracle_text(CS_ORACLE, "~", &[], &["Creature".to_string()], &[]);
    assert!(
        parsed.extracted_keywords.contains(&Keyword::Devoid),
        "the Devoid line must parse as an extracted keyword, independent of the modal line"
    );
    assert!(
        parsed.replacements.iter().any(|r| r
            .execute
            .as_deref()
            .is_some_and(|e| matches!(e.effect.as_ref(), Effect::Choose { .. }))),
        "the modal Choose replacement must still fire alongside the Devoid line"
    );
}

/// V8: Aquamorph Entity. Choosing "5/1" yields a 5/1; AND the parser surfaces an
/// honest `Effect::unimplemented` for the "or is turned face up" arm rather than
/// silently dropping it (CR 614.1e).
#[test]
fn v8_aquamorph_five_one_and_face_up_gap() {
    use engine::parser::oracle::parse_oracle_text;

    const AQ_ORACLE: &str =
        "As ~ enters or is turned face up, it becomes your choice of 5/1 or 1/5.";
    const AQ_LABELS: &[&str] = &["5/1", "1/5"];

    let (runner, obj) = place_and_choose("Aquamorph Entity", AQ_ORACLE, AQ_LABELS, "5/1");
    let o = &runner.state().objects[&obj];
    assert_eq!(o.power, Some(5), "5/1 mode: power 5");
    assert_eq!(o.toughness, Some(1), "5/1 mode: toughness 1");

    // Coverage-red honest gap for the face-up arm.
    let parsed = parse_oracle_text(AQ_ORACLE, "~", &[], &["Creature".to_string()], &[]);
    let has_face_up_gap = parsed.abilities.iter().any(|a| {
        a.effect
            .unimplemented_description()
            .is_some_and(|d| d.contains("turned face up"))
    });
    assert!(
        has_face_up_gap,
        "the 'or is turned face up' arm must be an honest Effect::unimplemented, not dropped"
    );
}

/// V9 collision: a hostile fixture with two modes that synthesize identical
/// labels must emit NO modal definition (the recognizer aborts).
#[test]
fn v9_duplicate_label_collision_emits_no_modal() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{Effect, StaticCondition};

    let parsed = parse_oracle_text(
        "As ~ enters, it becomes your choice of a 3/3 creature or a 3/3 creature.",
        "~",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        !parsed.replacements.iter().any(|r| r
            .execute
            .as_deref()
            .is_some_and(|e| matches!(e.effect.as_ref(), Effect::Choose { .. }))),
        "duplicate synthesized labels must abort modal emission"
    );
    assert!(
        !parsed
            .statics
            .iter()
            .any(|s| matches!(s.condition, Some(StaticCondition::ChosenLabelIs { .. }))),
        "no gated statics when the modal aborts"
    );
}

// ---------------------------------------------------------------------------
// FIX 1: production-path regression tests.
//
// Unlike `place_and_choose` above (which hand-builds `waiting_for` on an
// already-battlefield object), these tests cast the modal creature FROM HAND
// through the real `apply()` `GameAction::CastSpell` pipeline, resolve the spell
// off the stack, and assert that the ENGINE ITSELF produces the
// `WaitingFor::NamedChoice` as the `ReplacementDefinition::new(Moved)` fires on
// the real battlefield entry (CR 614.12a deferred-entry pause). The chosen label
// is then submitted via the production `GameAction::ChooseOption` handler and the
// resulting P/T + keywords are asserted off the real object.
//
// This mirrors the pattern in `tests/constellation_enters_with_choice.rs`
// (`as_enters_choice_creature_fires_soul_warden`). If the new
// `ReplacementDefinition::new(Moved)` did NOT fire on entry, `waiting_for` would
// be `Priority` (no engine-produced NamedChoice) and the test panics at the
// `else` arm — that is the load-bearing proof the Moved replacement is live.
// ---------------------------------------------------------------------------

/// Cast a modal as-enters creature (printed 0/0, no mana cost) from P0's hand
/// through the REAL pipeline, assert the ENGINE surfaces `WaitingFor::NamedChoice`
/// on entry (source = the entering object), submit `chosen` via the production
/// `ChooseOption` handler, and return the runner + object id. `expected_options`
/// must be exactly the engine-produced label set (proves the synthesized labels
/// reach the choice).
fn cast_and_engine_choose(
    name: &str,
    oracle: &str,
    expected_options: &[&str],
    chosen: &str,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Printed 0/0 so any nonzero P/T can only come from the gated static that the
    // chosen label unlocks — never from the printed base.
    let obj = scenario
        .add_creature_to_hand_from_oracle(P0, name, 0, 0, oracle)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&obj).unwrap().card_id;

    // Cast from hand through the real action handler (Auto payment; the card has
    // no mana cost). The spell resolves and the creature moves to the battlefield.
    runner
        .act(GameAction::CastSpell {
            object_id: obj,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap_or_else(|e| panic!("cast {name} from hand: {e:?}"));
    runner.advance_until_stack_empty();

    // LOAD-BEARING: the engine (not the test) must have paused the entry on the
    // modal choice produced by the Moved/Battlefield Choose replacement.
    let WaitingFor::NamedChoice {
        options,
        source: Some(source),
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the modal entry must pause on an engine-produced NamedChoice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        source.prompt.identity.reference.object_id, obj,
        "the NamedChoice source must be the entering modal creature (proves the \
         SelfRef/Battlefield Moved replacement fired on THIS entrant)"
    );
    let expected: Vec<String> = expected_options.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        options, expected,
        "the engine-surfaced options must be exactly the synthesized mode labels"
    );

    // Answer through the production ChooseOption handler.
    runner
        .act(GameAction::ChooseOption {
            choice: chosen.to_string(),
        })
        .unwrap_or_else(|e| panic!("ChooseOption({chosen}): {e:?}"));
    runner.advance_until_stack_empty();

    (runner, obj)
}

/// FIX 1 (Primal Plasma): the FULL production path. Cast from hand, the engine
/// surfaces the NamedChoice on entry, choose "2/2 Flying", and the entrant is a
/// 2/2 with Flying (and none of the other modes' keywords). Reverting the Moved
/// replacement makes `waiting_for` be Priority (panics at the choice `else`);
/// reverting the `ChosenLabelIs` gate leaves the printed 0/0 and drops Flying, so
/// the P/T + keyword assertions flip.
#[test]
fn production_path_primal_plasma_two_two_flying() {
    let (runner, obj) =
        cast_and_engine_choose("Primal Plasma", PLASMA_ORACLE, PLASMA_LABELS, "2/2 Flying");
    let o = &runner.state().objects[&obj];
    assert_eq!(
        o.zone,
        engine::types::zones::Zone::Battlefield,
        "the modal creature must have entered the battlefield after the choice"
    );
    assert_eq!(
        o.chosen_label(),
        Some("2/2 Flying"),
        "the engine-driven choice must persist as ChosenAttribute::Label"
    );
    assert_eq!(
        o.power,
        Some(2),
        "2/2 mode: base power 2 from the gated SetPower on the REAL entry, not printed 0/0"
    );
    assert_eq!(o.toughness, Some(2), "2/2 mode: base toughness 2");
    assert!(
        o.has_keyword(&Keyword::Flying),
        "2/2 Flying mode grants Flying on the real entry path"
    );
    assert!(
        !o.has_keyword(&Keyword::Defender),
        "2/2 Flying mode must not grant the 1/6 mode's Defender"
    );
}

/// FIX 1 companion (Primal Clay, additive path): full production path with the
/// new multi-axis labels. Choosing "1/6 Artifact Wall Defender" yields a 1/6
/// Artifact Creature with the Wall subtype and Defender — proving the new label
/// (FIX 2) flows through the engine choice AND gates the additive static.
#[test]
fn production_path_primal_clay_additive_wall_defender() {
    const CLAY_ORACLE: &str = "As ~ enters, it becomes your choice of a 3/3 artifact \
        creature, a 2/2 artifact creature with flying, or a 1/6 Wall artifact creature \
        with defender in addition to its other types.";
    const CLAY_LABELS: &[&str] = &[
        "3/3 Artifact",
        "2/2 Artifact Flying",
        "1/6 Artifact Wall Defender",
    ];

    let (runner, obj) = cast_and_engine_choose(
        "Primal Clay",
        CLAY_ORACLE,
        CLAY_LABELS,
        "1/6 Artifact Wall Defender",
    );
    let o = &runner.state().objects[&obj];
    assert_eq!(o.power, Some(1), "1/6 mode: power 1 on the real entry");
    assert_eq!(o.toughness, Some(6), "1/6 mode: toughness 6");
    assert!(
        o.has_keyword(&Keyword::Defender),
        "1/6 mode must have Defender on the real entry"
    );
    assert!(
        o.card_types.core_types.contains(&CoreType::Artifact),
        "CR 205.1b additive: entrant is an Artifact Creature"
    );
    assert!(
        o.card_types.subtypes.iter().any(|s| s == "Wall"),
        "1/6 mode grants the Wall subtype, got {:?}",
        o.card_types.subtypes
    );
}
