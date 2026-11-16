//! #9662: focused enters-with counter declines and paid-X choice regressions.
//! The implementation preserves Clause declines at the general replacement boundary,
//! the replacement-sentence sequence, and the Class replacement-decline fallback;
//! earlier successful parses and independent container/effect routes remain outside
//! this guarantee. All issue fixtures below are synthetic grammar witnesses.
use engine::game::coverage::card_face_gaps;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// The list the reader already accepted: the harness-live control.
const CONJOINED_CONTROL: &str =
    "This creature enters with two +1/+1 counters and a lifelink counter on it.";

fn charge() -> CounterType {
    parse_counter_type("charge")
}

fn lifelink() -> CounterType {
    CounterType::Keyword(KeywordKind::Lifelink)
}

fn free_creature(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, name, 0, 1, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn cast_all(runner: &mut GameRunner, ids: &[ObjectId]) {
    for &id in ids {
        runner.cast(id).resolve();
        runner.advance_until_stack_empty();
    }
}

/// Reach guard: the object resolved onto the battlefield.
fn counters(runner: &GameRunner, id: ObjectId) -> std::collections::BTreeMap<CounterType, u32> {
    let obj = &runner.state().objects[&id];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach guard: {} resolved",
        obj.name
    );
    obj.counters.iter().map(|(k, v)| (k.clone(), *v)).collect()
}

fn count(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> Option<u32> {
    counters(runner, id).get(kind).copied()
}

fn has_unimplemented_ability(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .abilities
        .iter()
        .any(|a| matches!(*a.effect, Effect::Unimplemented { .. }))
}

/// CR 614.1c + CR 122.6 + CR 608.2c: a placement the parser cannot read places
/// nothing and surfaces as an explicit gap. BASE published the first placement
/// alone (probed: P1P1 present, the second placement absent, the line reported
/// supported), handing a dynamic count across placements and dropping the rest.
/// The where-X lists published the cost X (0 on a free spell) where the
/// definition belongs.
#[test]
fn unread_placements_place_nothing_and_surface_a_gap() {
    const WITNESSES: [(&str, &str); 9] = [
        (
            "Dynamic Second Witness",
            "This creature enters with a +1/+1 counter on it and a number of charge counters on \
             it equal to the number of creatures you control.",
        ),
        (
            "Foreign Recipient Witness",
            "This creature enters with a +1/+1 counter on it and a lifelink counter on target \
             creature.",
        ),
        (
            "Oxford Witness",
            "This creature enters with a +1/+1 counter on it, a lifelink counter on it, and a \
             trample counter on it.",
        ),
        (
            "Both X Witness",
            "This creature enters with X +1/+1 counters on it and X charge counters on it, where \
             X is the number of lands you control.",
        ),
        (
            "List Where X Witness",
            "This creature enters with a +1/+1 counter and X charge counters on it, where X is \
             the number of lands you control.",
        ),
        (
            "List Third Placement Witness",
            "This creature enters with two +1/+1 counters and a lifelink counter on it and a \
             trample counter on it.",
        ),
        (
            "Interstitial Rounding Witness",
            "This creature enters with half X +1/+1 counters on it, rounded up, and a lifelink \
             counter on it, where X is the number of lands you control.",
        ),
        (
            "List Rounding Witness",
            "This creature enters with half X +1/+1 counters and a lifelink counter on it, \
             rounded up.",
        ),
        (
            "Equal To Tail Witness",
            "This creature enters with X +1/+1 counters on it and X charge counters on it equal \
             to the number of lands you control.",
        ),
    ];
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let twin = free_creature(&mut scenario, "Readable Twin", CONJOINED_CONTROL);
    let ids: Vec<ObjectId> = WITNESSES
        .iter()
        .map(|(name, oracle)| free_creature(&mut scenario, name, oracle))
        .collect();
    let mut runner = scenario.build();
    cast_all(&mut runner, &[twin]);
    cast_all(&mut runner, &ids);

    // Reach guard: the readable twin placed both counters in this same game.
    assert_eq!(count(&runner, twin, &CounterType::Plus1Plus1), Some(2));
    assert_eq!(count(&runner, twin, &lifelink()), Some(1));
    assert!(!has_unimplemented_ability(&runner, twin));
    for (id, (name, _)) in ids.iter().zip(WITNESSES) {
        assert!(
            counters(&runner, *id).is_empty(),
            "{name}: no partial counter list, got {:?}",
            runner.state().objects[id].counters
        );
        assert!(
            has_unimplemented_ability(&runner, *id),
            "{name}: the line is an explicit gap"
        );
    }
}

/// CR 107.3m + CR 614.12a: the `X` of a "your choice of X ..." option is the X paid
/// for the spell that became the permanent. BASE kept an unbound `Variable("X")`:
/// the chosen branch placed nothing (X paid 3).
fn cast_x_choice(branch: usize, oracle: &str, x: u32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_to_hand_from_oracle(P0, "Choice Witness", 0, 1, oracle)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..=x)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(id).x(x).resolve();
    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch { branches, .. } => assert_eq!(branches.len(), 2),
        other => panic!("expected the counter choice on entry, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseBranch { index: branch })
        .expect("choosing a branch");
    runner.advance_until_stack_empty();
    (runner, id)
}

#[test]
fn choice_options_read_the_paid_x() {
    const CHOICE: &str =
        "This creature enters with your choice of X +1/+1 counters or X charge counters on it.";
    let (plus_game, plus) = cast_x_choice(0, CHOICE, 3);
    assert_eq!(count(&plus_game, plus, &CounterType::Plus1Plus1), Some(3));
    assert_eq!(count(&plus_game, plus, &charge()), None);
    let (charge_game, charged) = cast_x_choice(1, CHOICE, 3);
    assert_eq!(count(&charge_game, charged, &charge()), Some(3));
    assert_eq!(count(&charge_game, charged, &CounterType::Plus1Plus1), None);
    // X = 0 is a legal announcement and places nothing.
    let (zero_game, zero) = cast_x_choice(0, CHOICE, 0);
    assert!(counters(&zero_game, zero).is_empty());
}

/// CR 614.12: a distributive "your choice of" line names a general subset of
/// permanents, which the choice replacement (built for the entering permanent
/// alone) does not carry. With the option counts reading the paid X, a
/// subject-blind choice would give the SOURCE itself X counters; the line is a
/// gap instead and the source enters with none.
#[test]
fn distributive_choice_gives_the_source_no_counters() {
    for distributive in [
        "Each other creature you control enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Each other commander you control enters with your choice of X +1/+1 counters or X charge counters on it.",
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let id = scenario
            .add_creature_to_hand_from_oracle(P0, "Distributive Witness", 0, 1, distributive)
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::X, ManaCostShard::Green],
            })
            .id();
        scenario.with_mana_pool(
            P0,
            (0..=3)
                .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        runner.cast(id).x(3).resolve();
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseOneOfBranch { .. }
            ),
            "{distributive}: no counter choice is offered for the source itself"
        );
        runner.advance_until_stack_empty();
        // `counters` is the reach guard: the X=3 spell resolved to the battlefield.
        assert!(
            counters(&runner, id).is_empty(),
            "{distributive}: the source is not in its own subset: got {:?}",
            runner.state().objects[&id].counters
        );
        assert!(has_unimplemented_ability(&runner, id), "{distributive}");
    }
    // Paired positive control: the self-subject line with the same options and
    // the same X places the chosen X counters (`choice_options_read_the_paid_x`
    // pins every branch; this pins that X=3 reaches a self choice).
    let (self_game, self_id) = cast_x_choice(
        0,
        "This creature enters with your choice of X +1/+1 counters or X charge counters on it.",
        3,
    );
    assert_eq!(
        count(&self_game, self_id, &CounterType::Plus1Plus1),
        Some(3)
    );
}

fn parsed_face(oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, "Probe Card", &[], &["Creature".to_string()], &[]);
    CardFace {
        name: "Probe Card".to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// The gap name the Priority-8 route records for a line, if its only ability is
/// an `Effect::Unimplemented`.
fn sole_gap_name(face: &CardFace) -> Option<&str> {
    match face.abilities.as_slice() {
        [only] => match &*only.effect {
            Effect::Unimplemented { name, .. } => Some(name.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// SHAPE: CR 614.12: property-only and union distributive subjects must not
/// publish the self-only counter choice. Each self-subject twin is a reach guard.
#[test]
fn untyped_distributive_choice_is_an_explicit_gap() {
    const SELF_CHOICE: &str =
        "This creature enters with your choice of X +1/+1 counters or X charge counters on it.";
    for oracle in [
        "Each other commander you control enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Other commander you control enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Each commander you control enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Commanders you control enter with your choice of X +1/+1 counters or X charge counters on it.",
        "Each other token you control enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Each other artifact or creature you control enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Each other creature or planeswalker you control enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Tokens you control enter with your choice of X +1/+1 counters or X charge counters on them.",
    ] {
        let control = parsed_face(SELF_CHOICE);
        assert_eq!(control.replacements.len(), 1, "{oracle}: self twin reaches choice");
        assert!(matches!(
            &*control.replacements[0].execute.as_ref().expect("choice execute").effect,
            Effect::ChooseOneOf { branches, .. } if branches.len() == 2
        ));
        assert!(card_face_gaps(&control).is_empty(), "{oracle}: readable twin");

        let face = parsed_face(oracle);
        assert_eq!(sole_gap_name(&face), Some("enters_with_clause"), "{oracle}: one explicit gap");
        assert!(face.replacements.is_empty(), "{oracle}: no self replacement");
        assert!(face.triggers.is_empty(), "{oracle}: no trigger fragment");
        assert!(face.static_abilities.is_empty(), "{oracle}: no static fragment");
        assert!(!card_face_gaps(&face).is_empty(), "{oracle}: unsupported");
    }
}

const COUNTER_DOUBLER: &str = "If one or more counters would be put on this creature, twice that \
                               many of those counters are put on it instead.";

/// SHAPE: For the general-route witnesses below, the quoted-rider and
/// sibling-sentence declines retain one explicit gap (CR 614.1c + CR 122.6 +
/// CR 608.2c). BASE published a fragment of these lines instead.
#[test]
fn unread_placement_lines_are_one_gap_through_the_whole_parse() {
    const QUOTED_COUNTERS: &str = "\"If one or more counters would be put on this creature, \
                                   twice that many of those counters are put on it instead.\"";
    const QUOTED_TOKENS: &str = "\"If one or more tokens would be created under your control, \
                                 twice that many of those tokens are created instead.\"";
    let witnesses = [
        // A distributive choice must not hand its quoted rider to a later parser.
        format!(
            "Each other commander you control enters with your choice of X +1/+1 counters or \
             X charge counters on it and with {QUOTED_COUNTERS}"
        ),
        format!(
            "Each other creature you control enters with your choice of X +1/+1 counters or \
             X charge counters on it and with {QUOTED_COUNTERS}"
        ),
        // #9662: a placement after the first placement's recipient.
        format!(
            "This creature enters with a +1/+1 counter on it and a lifelink counter on target \
             creature and with {QUOTED_COUNTERS}"
        ),
        format!(
            "This creature enters with a +1/+1 counter on it and a number of charge counters on \
             it equal to the number of creatures you control and with {QUOTED_COUNTERS}"
        ),
        format!(
            "This creature enters with a +1/+1 counter on it and a lifelink counter on it and \
             with {QUOTED_COUNTERS}"
        ),
        format!(
            "This creature enters with a +1/+1 counter on it and a lifelink counter on it and \
             with {QUOTED_TOKENS}"
        ),
        format!(
            "{COUNTER_DOUBLER} This creature enters with a +1/+1 counter on it and a lifelink \
             counter on it."
        ),
        format!(
            "{COUNTER_DOUBLER} This creature enters with a +1/+1 counter on it and a lifelink \
             counter on target creature."
        ),
        // #9880's placement directly after the counter noun, same exposure.
        format!(
            "This creature enters with a +1/+1 counter and a lifelink counter on him and with \
             {QUOTED_COUNTERS}"
        ),
        format!(
            "{COUNTER_DOUBLER} This creature enters with a +1/+1 counter and a lifelink counter \
             on him."
        ),
        // A counter list followed by a further placement.
        format!(
            "This creature enters with two +1/+1 counters and a lifelink counter on it and a \
             trample counter on it and with {QUOTED_COUNTERS}"
        ),
    ];
    let mut unexpected = Vec::new();
    for oracle in &witnesses {
        let face = parsed_face(oracle);
        if sole_gap_name(&face) != Some("enters_with_clause") {
            unexpected.push((oracle, face));
            continue;
        }
        assert!(
            face.replacements.is_empty()
                && face.triggers.is_empty()
                && face.static_abilities.is_empty(),
            "{oracle}: nothing taken from a fragment, got {:?} / {:?} / {:?}",
            face.replacements,
            face.triggers,
            face.static_abilities
        );
        assert!(!card_face_gaps(&face).is_empty(), "{oracle}: unsupported");
    }
    assert!(
        unexpected.is_empty(),
        "expected one enters_with_clause gap, got fragments: {unexpected:#?}"
    );

    // Reach guards: the same lines' readable parts still parse where nothing
    // is unread, so the gap above is the unread placement's, not a route the
    // fixtures never reach.
    let readable =
        parsed_face("This creature enters with two +1/+1 counters and a lifelink counter on it.");
    assert!(readable.abilities.is_empty(), "{:?}", readable.abilities);
    assert_eq!(readable.replacements.len(), 1);
    assert!(card_face_gaps(&readable).is_empty());
    let self_choice = parsed_face(
        "This creature enters with your choice of X +1/+1 counters or X charge counters on it.",
    );
    assert_eq!(self_choice.replacements.len(), 1);
    assert!(matches!(
        &*self_choice.replacements[0].execute.as_ref().expect("choice execute").effect,
        Effect::ChooseOneOf { branches, .. } if branches.len() == 2
    ));
    assert!(card_face_gaps(&self_choice).is_empty());
    // The guard does not suppress a genuine counter or token replacement line.
    for line in [
        COUNTER_DOUBLER,
        "If one or more tokens would be created under your control, twice that many of those \
         tokens are created instead.",
    ] {
        let face = parsed_face(line);
        assert_eq!(face.replacements.len(), 1, "{line}: {:?}", face.abilities);
        assert!(card_face_gaps(&face).is_empty(), "{line}");
    }
    // A valid two-sentence sibling keeps BOTH replacements.
    let two = parsed_face(&format!(
        "{COUNTER_DOUBLER} This creature enters with two +1/+1 counters and a lifelink counter on it."
    ));
    let events: Vec<_> = two.replacements.iter().map(|r| r.event.clone()).collect();
    assert_eq!(
        events,
        [ReplacementEvent::AddCounter, ReplacementEvent::Moved],
        "{:?}",
        two.abilities
    );
    assert!(card_face_gaps(&two).is_empty());
}

/// SHAPE: a later sentence's Clause decline is preserved by the sentence sequence.
/// CR 614.1c + CR 122.6: a paragraph of several "enters with" sentences is read
/// sentence by sentence; when one sentence prints a placement the parser cannot
/// read, the paragraph is one gap. BASE published the readable sentence and
/// the unread sentence's first placement; a plain sentence decline let the
/// whole-paragraph retry read only the FIRST sentence and drop the other one
/// while reporting the card supported.
#[test]
fn unread_placement_in_a_later_sentence_is_one_gap() {
    let mut unexpected = Vec::new();
    for oracle in [
        "This creature enters with a +1/+1 counter on it. This creature enters with a charge \
         counter on it and a lifelink counter on it.",
        "This creature enters with a charge counter on it and a lifelink counter on it. This \
         creature enters with a +1/+1 counter on it.",
        "This creature enters with a +1/+1 counter on it. This creature enters with a charge \
         counter and a lifelink counter on target creature.",
        "This creature enters with a +1/+1 counter on it. This creature enters with two charge \
         counters and a lifelink counter on it and a trample counter on it.",
        // CR 614.12: an unsupported distributive choice keeps no readable sibling.
        "This creature enters with a +1/+1 counter on it. Each other creature you control \
         enters with your choice of X +1/+1 counters or X charge counters on it.",
        "Each other creature you control enters with your choice of X +1/+1 counters or \
         X charge counters on it. This creature enters with a +1/+1 counter on it.",
        "This creature enters with a +1/+1 counter on it. Each other commander you control \
         enters with your choice of X +1/+1 counters or X charge counters on it.",
    ] {
        let face = parsed_face(oracle);
        if sole_gap_name(&face) != Some("enters_with_clause") {
            unexpected.push((oracle, face));
            continue;
        }
        assert!(
            face.replacements.is_empty(),
            "{oracle}: {:?}",
            face.replacements
        );
        assert!(!card_face_gaps(&face).is_empty(), "{oracle}: unsupported");
    }
    assert!(
        unexpected.is_empty(),
        "expected one enters_with_clause gap, got partial paragraphs: {unexpected:#?}"
    );
    // Reach guards: a paragraph whose sentences are all read keeps one
    // replacement per sentence.
    for (oracle, expected) in [
        (
            "This creature enters with a +1/+1 counter on it. This creature enters with a charge \
             counter and a lifelink counter on it.",
            2,
        ),
        (
            "This creature enters with a +1/+1 counter on it. It enters tapped.",
            2,
        ),
        (
            "This creature enters with a +1/+1 counter on it. This creature enters with your \
             choice of X +1/+1 counters or X charge counters on it.",
            2,
        ),
    ] {
        let face = parsed_face(oracle);
        assert_eq!(
            face.replacements.len(),
            expected,
            "{oracle}: {:?}",
            face.abilities
        );
        assert!(face.abilities.is_empty(), "{oracle}: {:?}", face.abilities);
        assert!(card_face_gaps(&face).is_empty(), "{oracle}");
    }
}

/// SHAPE: scanning continues after an ordinary failure to find a later Clause decline.
/// CR 614.1c + CR 122.6: an ordinary sentence failure must not stop the scan
/// before a later sentence's Clause decline is found.
#[test]
fn ordinary_sentence_failure_keeps_a_later_clause_decline() {
    for (oracle, readable, expected_replacements) in [
        (
            "This creature enters with a +1/+1 counter on it. This creature enters with up to \
             two shoe counters on it. This creature enters with a charge counter on it and a \
             lifelink counter on it.",
            "This creature enters with a +1/+1 counter on it. This creature enters with two \
             shoe counters on it. This creature enters with a charge counter and a lifelink \
             counter on it.",
            3,
        ),
        (
            "This creature enters with X +1/+1 counters on it, where X is the mystery total. \
             This creature enters with a charge counter on it and a lifelink counter on it.",
            "This creature enters with X +1/+1 counters on it, where X is the number of lands \
             you control. This creature enters with a charge counter and a lifelink counter \
             on it.",
            2,
        ),
    ] {
        let control = parsed_face(readable);
        assert_eq!(
            control.replacements.len(),
            expected_replacements,
            "{readable}"
        );
        assert!(
            control.abilities.is_empty(),
            "{readable}: {:?}",
            control.abilities
        );
        assert!(card_face_gaps(&control).is_empty(), "{readable}");

        let face = parsed_face(oracle);
        assert_eq!(sole_gap_name(&face), Some("enters_with_clause"), "{oracle}");
        assert!(
            face.replacements.is_empty(),
            "{oracle}: {:?}",
            face.replacements
        );
        assert!(face.triggers.is_empty(), "{oracle}: {:?}", face.triggers);
        assert!(
            face.static_abilities.is_empty(),
            "{oracle}: {:?}",
            face.static_abilities
        );
        assert!(!card_face_gaps(&face).is_empty(), "{oracle}: unsupported");
    }
}

fn class_face(level_one: &str, level_two: &str, level_three: &str) -> CardFace {
    let oracle = format!(
        "(Gain the next level as a sorcery to add its ability.)\n{level_one}\n{{G}}: Level \
         2\n{level_two}\n{{3}}{{G}}: Level 3\n{level_three}"
    );
    let parsed = parse_oracle_text(
        &oracle,
        "Probe Class",
        &[],
        &["Enchantment".to_string()],
        &["Class".to_string()],
    );
    CardFace {
        name: "Probe Class".to_string(),
        oracle_text: Some(oracle),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// SHAPE: the Class fallback records exactly one replacement-structure gap.
/// CR 716 + CR 614.1c + CR 122.6: the Class preprocessor reports a Clause
/// decline with the standard `replacement_structure` unsupported IR category;
/// it does not reach the general route's `enters_with_clause` gap constructor.
/// A plain decline let the Class effect fallback publish the quoted rider as
/// the section's ability (`AddTargetReplacement`) and report the card
/// supported (BASE: the first placement alone, unsupported).
#[test]
fn unread_placement_in_a_class_section_is_one_gap() {
    const LEVEL_TWO: &str = "Permanents you control with counters on them have ward {1}.";
    const LEVEL_THREE: &str = "Whenever you cast a legendary spell, exile the top two cards of \
                               your library. You may play them this turn.";
    const COMBAT: &str = "At the beginning of combat on your turn, put a +1/+1 counter on \
                          target creature you control.";
    let witness = format!(
        "This creature enters with a +1/+1 counter on it and a lifelink counter on target \
         creature and with \"{COUNTER_DOUBLER}\""
    );
    for face in [
        class_face(&witness, "", ""),
        class_face("", &witness, ""),
        class_face(
            "This creature enters with a +1/+1 counter on it and a lifelink counter on it.",
            "",
            "",
        ),
    ] {
        let gaps: Vec<_> = face
            .abilities
            .iter()
            .filter_map(|ability| match &*ability.effect {
                Effect::Unimplemented { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            gaps,
            ["replacement_structure"],
            "one explicit gap, got {:?}",
            face.abilities
        );
        assert!(
            !face
                .abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::AddTargetReplacement { .. })),
            "no quoted rider published as an ability: {:?}",
            face.abilities
        );
        assert!(face.replacements.is_empty(), "{:?}", face.replacements);
        assert!(face.triggers.is_empty(), "{:?}", face.triggers);
        assert!(
            face.static_abilities.is_empty(),
            "{:?}",
            face.static_abilities
        );
        assert!(!card_face_gaps(&face).is_empty(), "unsupported");
    }
    // Reach guards: the Class preprocessor still reads a readable section
    // enters-with line and a genuine level-gated replacement (Innkeeper's
    // Talent's level 3), with no gap.
    let readable = class_face(
        "Legendary creatures you control enter with an additional +1/+1 counter on them.",
        LEVEL_TWO,
        LEVEL_THREE,
    );
    assert_eq!(readable.replacements.len(), 1, "{:?}", readable.abilities);
    assert!(card_face_gaps(&readable).is_empty());
    let gated = class_face(
        COMBAT,
        LEVEL_TWO,
        "If you would put one or more counters on a permanent or player, put twice that many of \
         each of those kinds of counters on that permanent or player instead.",
    );
    assert_eq!(gated.replacements.len(), 1, "{:?}", gated.abilities);
    assert_eq!(gated.replacements[0].event, ReplacementEvent::AddCounter);
    assert!(
        gated.replacements[0].condition.is_some(),
        "level-gated (CR 716.2a)"
    );
    assert!(card_face_gaps(&gated).is_empty());
}
