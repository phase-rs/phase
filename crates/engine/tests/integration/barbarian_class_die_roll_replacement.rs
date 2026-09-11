//! CR 706.6 + CR 614.1a — die-roll replacement effects that modify how many dice
//! are rolled and discard rolls.
//!
//! Barbarian Class (level 1), verified verbatim against Scryfall:
//! > If you would roll one or more dice, instead roll that many dice plus one
//! > and ignore the lowest roll.
//!
//! CR 706.1 scopes a die-roll event to the whole INSTRUCTION ("how many of those
//! dice to roll"), so a "roll a d20" instruction proposes one
//! `ProposedEvent::RollDice { count: 1 }`; the replacement raises it to 2 and
//! attaches `DieRollIgnoreRule::Lowest`. CR 706.6 then makes the ignored roll
//! "considered to have never happened", so it emits no `GameEvent::DieRolled`,
//! receives no modifier, runs no results branch, and contributes nothing to the
//! aggregate "equal to the result(s)" value.
//!
//! Every assertion here is ordered reach-guard FIRST: the failure value on this
//! path is 0 (no rolls found / a wiped die result), so an assertion of the form
//! "the value is not the ignored die's" would pass VACUOUSLY against 0. Each
//! test therefore asserts the effect happened AT ALL before asserting which
//! value it saw.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DieResultBranch, DieRollIgnoreRule, DieRollModifier, Effect,
    QuantityExpr, QuantityRef, ReplacementPlayerScope, ResolvedAbility,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

const BARBARIAN_CLASS_L1: &str = "If you would roll one or more dice, instead roll that many \
dice plus one and ignore the lowest roll.";

/// Parse an Oracle line with no MTGJSON keyword/type context — every clause
/// under test is a standalone replacement line.
fn parse(text: &str, name: &str) -> engine::parser::oracle::ParsedAbilities {
    engine::parser::oracle::parse_oracle_text(text, name, &[], &[], &[])
}

/// Count the `DieRolled` events in an event log.
fn die_rolls(events: &[GameEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(result),
                ..
            } => Some(*result),
            _ => None,
        })
        .collect()
}

// --- V2: the parser emits a real replacement, not an `Unimplemented` ability ---

/// V2 — the Oracle clause becomes a `RollDice` replacement carrying BOTH halves
/// of "instead roll that many dice plus one AND ignore the lowest roll".
///
/// Reach-guard: assert the replacement exists before asserting its shape, and
/// assert the line did NOT also leave an `Unimplemented` ability behind — the
/// pre-change baseline was exactly one `Unimplemented` ability and zero
/// replacements.
#[test]
fn barbarian_class_level_one_parses_as_a_roll_dice_replacement() {
    let parsed = parse(BARBARIAN_CLASS_L1, "Barbarian Class");

    assert_eq!(
        parsed.replacements.len(),
        1,
        "CR 614.1a: the clause must produce exactly one replacement definition, got {:?}",
        parsed.replacements
    );
    let def = &parsed.replacements[0];
    assert_eq!(def.event, ReplacementEvent::RollDice);
    // CR 614.1a: "If YOU would roll" is controller-scoped.
    assert_eq!(def.valid_player, Some(ReplacementPlayerScope::You));
    // CR 706.6: the ignore rule travels on the definition.
    assert_eq!(def.die_ignore_rule, Some(DieRollIgnoreRule::Lowest));

    // CR 614.1a: "that many dice plus one" — `Offset` over the event's own count.
    let execute = def.execute.as_deref().expect("replacement must execute");
    match &*execute.effect {
        Effect::RollDie { count, .. } => assert_eq!(
            count,
            &QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
                offset: 1,
            },
            "the raised count must be the event's own count plus one"
        ),
        other => panic!("expected a RollDie execute, got {other:?}"),
    }

    // The line must not ALSO fall through to an unimplemented ability.
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|ability| matches!(&*ability.effect, Effect::Unimplemented { .. })),
        "the replacement line must not also leave an Unimplemented ability: {:?}",
        parsed.abilities
    );
}

/// V2 sibling — the SAME clause behind a CR 207.2c ability word must parse
/// identically. Pixie Guide prints "Grant an Advantage — " ahead of the
/// antecedent, and the peel is only a no-op-safe `opt(...)` if that name is
/// actually in `ABILITY_WORD_NAMES`: an unlisted word leaves the prefix in
/// place, `tag("if you would roll ")` fails, and the whole card silently
/// degrades to `Effect::Unimplemented` while every doc still claims support.
///
/// This is the parse-level guard for that regression — the stacked runtime test
/// below needs Pixie Guide to parse before it can prove anything at all.
#[test]
fn pixie_guide_ability_word_prefix_is_peeled_before_the_antecedent() {
    let parsed = parse(PIXIE_GUIDE, "Pixie Guide");

    assert_eq!(
        parsed.replacements.len(),
        1,
        "CR 207.2c: the ability word carries no rules meaning and must be peeled, leaving exactly \
         one replacement, got {:?}",
        parsed.replacements
    );
    let def = &parsed.replacements[0];
    assert_eq!(def.event, ReplacementEvent::RollDice);
    assert_eq!(def.valid_player, Some(ReplacementPlayerScope::You));
    assert_eq!(def.die_ignore_rule, Some(DieRollIgnoreRule::Lowest));

    // The peel must not leave the ability-word line behind as an unimplemented
    // gap — the exact symptom an unlisted word produces.
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|ability| matches!(&*ability.effect, Effect::Unimplemented { .. })),
        "the peeled line must not leave an Unimplemented ability: {:?}",
        parsed.abilities
    );
}

/// V2 negative sibling — the coin-flip clause must still route to `CoinFlip`.
/// The die combinator is dispatched immediately after the Krark one, so a
/// mis-ordered or over-broad `alt()` arm would steal this line.
#[test]
fn krark_coin_flip_clause_still_routes_to_the_coin_flip_replacement() {
    let parsed = parse(
        "If you would flip a coin, instead flip two coins and ignore one.",
        "Krark's Thumb",
    );
    assert_eq!(parsed.replacements.len(), 1);
    assert_eq!(parsed.replacements[0].event, ReplacementEvent::CoinFlip);
}

/// V7 — the class free-rides: Wyll, Blade of Frontiers carries the byte-identical
/// clause and must produce the identical definition, not merely "parse".
#[test]
fn wyll_produces_the_same_die_roll_replacement_as_barbarian_class() {
    let barbarian = parse(BARBARIAN_CLASS_L1, "Barbarian Class");
    let wyll = parse(BARBARIAN_CLASS_L1, "Wyll, Blade of Frontiers");
    assert_eq!(wyll.replacements.len(), 1);
    assert_eq!(wyll.replacements[0].event, ReplacementEvent::RollDice);
    assert_eq!(
        wyll.replacements[0].die_ignore_rule,
        barbarian.replacements[0].die_ignore_rule
    );
    assert_eq!(
        wyll.replacements[0].valid_player,
        barbarian.replacements[0].valid_player
    );
}

/// V7 hostile — CR 706.7 excludes the planar die from every effect that refers
/// to a numerical die result, so the planar sibling (Ichor Elixir) must be left
/// UNMATCHED rather than matched-and-stubbed.
#[test]
fn planar_dice_variant_is_not_claimed_by_the_die_roll_combinator() {
    let parsed = parse(
        "If you would roll one or more planar dice, instead roll that many planar dice plus one \
         and ignore the lowest roll.",
        "Ichor Elixir",
    );
    assert!(
        !parsed
            .replacements
            .iter()
            .any(|def| def.event == ReplacementEvent::RollDice),
        "CR 706.7: the planar-dice clause must not produce a RollDice replacement"
    );
}

// --- V2 sibling rules: the ignore axis is parameterized, not a Lowest special case ---

/// The combinator covers exactly one point on the CR 706.6 ignore axis, because
/// exactly one is corpus-backed: `Lowest` ("and ignore the lowest roll"),
/// printed on Barbarian Class, Pixie Guide, and Wyll, Blade of Frontiers, each
/// of which reaches this parser through its real Oracle text.
///
/// Neither "the highest roll" nor "one" is parsed — a corpus check for the exact
/// grammar this parser accepts returns ZERO cards for both, so each stays an
/// honest gap like the rejected `plus N` form below rather than shipping an
/// unvalidated leaf.
///
/// "and ignore one" is the more tempting of the two, because the words ARE
/// printed — on Ichor Elixir, Krark's Other Thumb, Probability Flux, Bamboozling
/// Beeble and Squid Fire Knight. None of them is served by adding the arm:
/// Ichor Elixir is planar (CR 706.7), Krark's Other Thumb uses neither the
/// "that many dice plus one" count form nor the tail, Probability Flux is a
/// duration-bounded ANY-player form this controller-scoped antecedent does not
/// match, and the two activated abilities let the ability's CONTROLLER choose
/// rather than the roller — an axis `WaitingFor::DieKeepChoice` cannot express.
#[test]
fn ignore_rule_variants_parse_across_the_axis() {
    let parsed = parse(BARBARIAN_CLASS_L1, "Test Card");
    assert_eq!(
        parsed.replacements.len(),
        1,
        "failed to parse the printed form"
    );
    assert_eq!(
        parsed.replacements[0].die_ignore_rule,
        Some(DieRollIgnoreRule::Lowest)
    );

    // Both unprinted tails must stay gaps. Each differs from the accepted text
    // ONLY in the tail, so a rejection is attributable to the ignore-rule
    // combinator and nothing else.
    for unprinted in [
        "If you would roll one or more dice, instead roll that many dice plus one and ignore the          highest roll.",
        "If you would roll one or more dice, instead roll that many dice plus one and ignore one.",
    ] {
        assert!(
            parse(unprinted, "Test Card")
                .replacements
                .iter()
                .all(|def| def.event != ReplacementEvent::RollDice),
            "an unprinted ignore form must stay an honest gap, not a speculative parse:              {unprinted}"
        );
    }
}

/// CR 706.6 removes exactly ONE roll, and `DieRollIgnoreRule` carries no ignore
/// count -- so "plus N" for N > 1 would roll N extra dice while ignoring only
/// one, silently inflating every aggregate and results-table branch. The
/// combinator must reject that form outright so it stays an honest gap rather
/// than a wrong parse. Every printing of this class reads "plus one".
///
/// Each fixture below differs from `BARBARIAN_CLASS_L1` ONLY in the number word,
/// so a rejection can only be attributed to the plus-N guard -- never to
/// incidental malformed text.
#[test]
fn plus_n_greater_than_one_is_rejected_rather_than_misparsed() {
    // Positive control: the surrounding grammar parses, so the negatives below
    // are rejected for the number word and nothing else.
    assert!(
        parse(BARBARIAN_CLASS_L1, "Test Card")
            .replacements
            .iter()
            .any(|def| def.event == ReplacementEvent::RollDice),
        "positive control: the plus-one form must still parse"
    );

    for number in ["two", "three", "four"] {
        let text = BARBARIAN_CLASS_L1.replace("plus one", &format!("plus {number}"));
        assert_ne!(
            text, BARBARIAN_CLASS_L1,
            "fixture must differ from the control"
        );
        let parsed = parse(&text, "Test Card");
        assert!(
            !parsed
                .replacements
                .iter()
                .any(|def| def.event == ReplacementEvent::RollDice),
            "CR 706.6: plus-N (N > 1) must not produce a RollDice replacement, got a parse for: {text}"
        );
    }
}

/// The accepted "plus one" form must raise the count by exactly one -- the
/// offset the parser emits and the single roll CR 706.6 ignores are the same
/// invariant, so they must not drift apart.
#[test]
fn plus_one_raises_the_count_by_exactly_one() {
    let parsed = parse(BARBARIAN_CLASS_L1, "Test Card");
    let def = parsed
        .replacements
        .iter()
        .find(|def| def.event == ReplacementEvent::RollDice)
        .expect("CR 706.1: the plus-one form must parse");
    let ability = def.execute.as_deref().expect("execute ability");
    match &*ability.effect {
        Effect::RollDie { count, .. } => match count {
            QuantityExpr::Offset { offset, .. } => assert_eq!(
                *offset, 1,
                "CR 706.6: exactly one extra die is rolled, because exactly one roll is ignored"
            ),
            other => panic!("expected an Offset count, got {other:?}"),
        },
        other => panic!("expected RollDie, got {other:?}"),
    }
}

// --- CR 706.6: the ignore set is computed over NATURALS ---

/// V10 (unit half) — `ignorable_indices` ranks NATURAL results and returns every
/// index tied at the extreme, which is CR 706.6's second sentence.
#[test]
fn ignorable_indices_ranks_naturals_and_reports_every_tie() {
    // A unique lowest is not a choice: exactly one index.
    assert_eq!(
        DieRollIgnoreRule::Lowest.ignorable_indices(&[2, 5]),
        vec![0]
    );
    // V10 inverted-order hostile fixture: the lowest is not always first.
    assert_eq!(
        DieRollIgnoreRule::Lowest.ignorable_indices(&[5, 2]),
        vec![1]
    );
    // CR 706.6 2nd sentence: every tied-lowest roll is a legal choice.
    assert_eq!(
        DieRollIgnoreRule::Lowest.ignorable_indices(&[3, 7, 3]),
        vec![0, 2]
    );
    // No rolls → nothing to ignore (the `rolled_any == false` path).
    assert!(DieRollIgnoreRule::Lowest.ignorable_indices(&[]).is_empty());
}

/// CR 706.6 (building-block half) — `ignorable_indices_for_rules` composes the
/// single-roll rule once per INSTRUCTING EFFECT.
///
/// CR 706.6 applies per instructing effect, so N applied die-roll replacements
/// ignore N rolls. The rules run in order over a shrinking pool, which is what
/// stops two `Lowest` rules from both naming the same roll. Tested at the
/// building-block level rather than through one card, per the repo's
/// "test the building block, not the special case" rule.
#[test]
fn ignorable_indices_for_rules_ignores_one_roll_per_applied_replacement() {
    // No rules: CR 706.6 is inert, nothing is ignored, no prompt is owed.
    assert_eq!(
        DieRollIgnoreRule::ignorable_indices_for_rules(&[], &[4, 5, 9]),
        (vec![], 0)
    );

    // One rule with a unique extreme: a forced set (candidates == count), so
    // the caller resolves it with no prompt.
    assert_eq!(
        DieRollIgnoreRule::ignorable_indices_for_rules(&[DieRollIgnoreRule::Lowest], &[2, 5]),
        (vec![0], 1)
    );

    // TWO rules, distinct naturals: each takes the lowest of what REMAINS, so
    // the pair is {0, 1} — never index 0 twice. Candidates == count, so this is
    // still forced and prompts nobody.
    assert_eq!(
        DieRollIgnoreRule::ignorable_indices_for_rules(
            &[DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
            &[1, 3, 6]
        ),
        (vec![0, 1], 2)
    );

    // Two rules on an all-tied pool: every roll is a legal pick for either rule,
    // so the candidate set is wider than the count — a genuine CR 706.6
    // tie-break, extended to two picks.
    assert_eq!(
        DieRollIgnoreRule::ignorable_indices_for_rules(
            &[DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
            &[4, 4, 4]
        ),
        (vec![0, 1, 2], 2)
    );

    // More rules than dice: a rule facing an empty pool ignores nothing rather
    // than panicking or double-counting. The count is clamped to the candidates.
    assert_eq!(
        DieRollIgnoreRule::ignorable_indices_for_rules(
            &[DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
            &[7]
        ),
        (vec![0], 1)
    );

    // No rolls at all: nothing to ignore no matter how many rules apply.
    assert_eq!(
        DieRollIgnoreRule::ignorable_indices_for_rules(&[DieRollIgnoreRule::Lowest], &[]),
        (vec![], 0)
    );
}

// --- Runtime: the replacement pipeline actually raises the count ---

/// Roll `sides`-sided dice `count` times through `Effect::RollDie`, driving the
/// real resolver, and return the events it produced plus the final aggregate
/// die result.
fn resolve_roll(
    runner: &mut engine::game::scenario::GameRunner,
    source: engine::types::identifiers::ObjectId,
    count: i32,
    sides: u8,
    modifier: Option<DieRollModifier>,
    results: Vec<DieResultBranch>,
) -> (Vec<GameEvent>, Option<i32>) {
    let ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: count },
            sides,
            results,
            modifier,
        },
        vec![],
        source,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("die roll resolves");
    let die_result = runner.state().die_result_this_resolution;
    (events, die_result)
}

/// V1 — with no replacement registered, one instruction emits exactly one
/// `DieRolled`; with Barbarian Class out, two dice are rolled and exactly one
/// survives.
///
/// The control arm's `> 0` assertion is the reach-guard: it proves the resolver
/// ran at all before the replaced arm's count is compared against it.
#[test]
fn barbarian_class_raises_the_die_count_and_ignores_the_lowest() {
    // Control: no replacement.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    let mut runner = scenario.build();
    let (events, _) = resolve_roll(&mut runner, source, 1, 20, None, vec![]);
    let control_rolls = die_rolls(&events);
    assert_eq!(
        control_rolls.len(),
        1,
        "reach-guard: an unreplaced instruction must emit exactly one DieRolled"
    );

    // Replaced: Barbarian Class raises 1 → 2 and ignores the lowest.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // The scenario builder parses the oracle text and registers the CR 706.6
    // replacement itself, so this is the real parser → applier → resolver path.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();
    let (events, _) = resolve_roll(&mut runner, source, 1, 20, None, vec![]);
    let replaced_rolls = die_rolls(&events);
    // CR 706.6: two dice were rolled; the ignored one never happened, so exactly
    // one `DieRolled` reaches the log.
    assert_eq!(
        replaced_rolls.len(),
        1,
        "CR 706.6: exactly one surviving roll must be emitted, got {replaced_rolls:?}"
    );
}

/// V5 — `valid_player` is live, not silently inert. "If YOU would roll" must not
/// add a die to an opponent's rolls.
///
/// The controller arm is the paired positive control: it proves the replacement
/// is registered and firing, so the opponent arm's result is a real scoping
/// outcome rather than a replacement that never applied to anyone.
#[test]
fn die_roll_replacement_is_scoped_to_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p0_source = scenario.add_creature(P0, "Roller", 1, 1).id();
    let p1_source = scenario.add_creature(P1, "Opposing Roller", 1, 1).id();
    // The scenario builder parses the oracle text and registers the CR 706.6
    // replacement itself, so this is the real parser → applier → resolver path.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();

    // The two arms MUST diverge structurally, not just in survivor count: under
    // CR 706.6 a raised-then-ignored roll emits exactly ONE `DieRolled`, which is
    // the same count an unreplaced single roll emits. Asserting `len() == 1` on
    // both arms would pass unchanged if `valid_player` were deleted outright.
    //
    // A 1-sided die is the discriminator (same device as
    // `tied_lowest_rolls_open_a_choice_limited_to_the_tied_indices`): every
    // natural is 1, so IF the count was raised to two the two rolls tie for
    // lowest and resolution must suspend on `WaitingFor::DieKeepChoice`. An
    // unraised single roll has nothing to tie with and never prompts.
    let d1 = |source, controller| {
        ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 1,
                results: vec![],
                modifier: None,
            },
            vec![],
            source,
            controller,
        )
    };

    // Controller arm (positive control): the replacement applied, so two naturals
    // were rolled, they tie at 1, and the roller is prompted to break the tie.
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &d1(p0_source, P0), &mut events)
        .expect("controller roll resolves");
    match runner.state().waiting_for.clone() {
        WaitingFor::DieKeepChoice {
            player,
            results,
            ignorable_indices,
            ignore_count,
        } => {
            assert_eq!(player, P0);
            // CR 706.1: two naturals prove the count was actually raised — this
            // is the fact a survivor count cannot express.
            assert_eq!(
                results,
                vec![1, 1],
                "CR 706.1: the `You`-scoped replacement must raise the controller's count to two"
            );
            assert_eq!(ignorable_indices, vec![0, 1]);
            assert_eq!(ignore_count, 1);
        }
        other => panic!(
            "reach-guard: the controller's raised roll must tie and open a DieKeepChoice, got \
             {other:?}"
        ),
    }
    assert!(
        die_rolls(&events).is_empty(),
        "CR 706.6: nothing is emitted while the controller's ignore choice is open"
    );
    // Clear the suspension so the opponent arm starts from a clean state.
    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the roller may ignore a tied-lowest roll");
    assert_eq!(
        die_rolls(&result.events).len(),
        1,
        "CR 706.6: one survivor after the controller's choice"
    );

    // Opponent arm: "If YOU would roll" never applies, so the count stays at one,
    // there is nothing to tie with, and NO prompt opens. This is the assertion
    // that fails if `object_replacement_candidate_applies`'s `RollDice` player
    // guard is removed.
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &d1(p1_source, P1), &mut events)
        .expect("opponent roll resolves");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "CR 614.1a: a `You`-scoped replacement must not raise an opponent's roll (a raised d1 \
         would tie and prompt), got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        die_rolls(&events),
        vec![1],
        "CR 614.1a: the opponent rolls exactly one unreplaced die"
    );
}

/// V11 — the aggregate "equal to the result(s)" value is summed over SURVIVING
/// dice only (CR 706.4 + CR 706.6), and the `rolled_any == false` branch clears
/// it rather than stamping `Some(0)`.
#[test]
fn aggregate_die_result_covers_survivors_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    let mut runner = scenario.build();

    // Unreplaced: three dice, all surviving — the aggregate is their sum, so it
    // is strictly greater than any single d20 roll can be only when all three
    // are large; assert instead against the emitted events, which is exact.
    let (events, die_result) = resolve_roll(&mut runner, source, 3, 20, None, vec![]);
    let rolls = die_rolls(&events);
    assert_eq!(rolls.len(), 3, "reach-guard: three dice must be emitted");
    let expected: i32 = rolls.iter().map(|r| i32::from(*r)).sum();
    assert_eq!(
        die_result,
        Some(expected),
        "CR 706.4: the aggregate must be the sum of every surviving roll"
    );

    // CR 614.6 / the `rolled_any == false` branch: a zero-count instruction
    // clears the stamp rather than leaving `Some(0)`.
    let (events, die_result) = resolve_roll(&mut runner, source, 0, 20, None, vec![]);
    assert!(
        die_rolls(&events).is_empty(),
        "a zero-count instruction rolls nothing"
    );
    assert_eq!(
        die_result, None,
        "CR 706.6: with no surviving roll the die result must be cleared, not Some(0)"
    );
}

/// V10 (runtime half) — the modifier applies ONLY to surviving dice, and the
/// ignore decision is made on the naturals.
///
/// With a `+100` modifier, an emitted roll is `natural + 100`. If the ignore set
/// had been computed on post-modifier values, or if the modifier had been
/// applied to an ignored die, the emitted value would not be in the expected
/// window.
#[test]
fn modifier_applies_only_to_the_surviving_roll() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // The scenario builder parses the oracle text and registers the CR 706.6
    // replacement itself, so this is the real parser → applier → resolver path.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();
    let (events, die_result) = resolve_roll(
        &mut runner,
        source,
        1,
        6,
        Some(DieRollModifier::Add {
            value: QuantityExpr::Fixed { value: 100 },
        }),
        vec![],
    );
    let rolls = die_rolls(&events);
    // Reach-guard FIRST: 0 is the failure value on every path here.
    assert_eq!(
        rolls.len(),
        1,
        "reach-guard: exactly one surviving roll must be emitted"
    );
    let emitted = i32::from(rolls[0]);
    assert!(
        emitted > 100,
        "CR 706.2: the modifier must have been applied to the survivor, got {emitted}"
    );
    assert!(
        (101..=106).contains(&emitted),
        "CR 706.2: the emitted value must be a natural 1..=6 plus 100, got {emitted}"
    );
    // CR 706.4 + CR 706.6: the aggregate covers the survivor alone — a second
    // (ignored) die would have pushed this above 106.
    assert_eq!(
        die_result,
        Some(emitted),
        "the aggregate must equal the single surviving roll's actual result"
    );
}

/// V6 — a tie among lowest rolls opens a prompt offering exactly the tied
/// indices; a unique lowest opens no prompt at all.
///
/// The 1-sided die guarantees a tie deterministically without depending on the
/// RNG seed: every roll is 1, so both dice are tied for the lowest.
#[test]
fn tied_lowest_rolls_open_a_choice_limited_to_the_tied_indices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // The scenario builder parses the oracle text and registers the CR 706.6
    // replacement itself, so this is the real parser → applier → resolver path.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();
    // A d1 makes every natural 1, so the two rolls tie for lowest.
    let (events, _) = resolve_roll(&mut runner, source, 1, 1, None, vec![]);

    match runner.state().waiting_for.clone() {
        WaitingFor::DieKeepChoice {
            player,
            results,
            ignorable_indices,
            ignore_count,
        } => {
            // CR 706.6: the roller chooses.
            assert_eq!(player, P0);
            // CR 706.2: the offered results are the NATURALS, in roll order.
            assert_eq!(results, vec![1, 1]);
            // CR 706.6 2nd sentence: both tied rolls are legal choices.
            assert_eq!(ignorable_indices, vec![0, 1]);
            assert_eq!(ignore_count, 1);
        }
        other => panic!("CR 706.6: a tie must open a DieKeepChoice, got {other:?}"),
    }
    // CR 706.6: nothing is emitted until the ignore is submitted.
    assert!(
        die_rolls(&events).is_empty(),
        "no DieRolled may be emitted while the ignore choice is open"
    );

    // Submitting the choice emits exactly one surviving roll.
    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the roller may ignore a tied-lowest roll");
    assert_eq!(
        die_rolls(&result.events).len(),
        1,
        "CR 706.6: exactly one surviving roll is emitted after the choice"
    );
}

/// V6 hostile — a submission naming a roll that is NOT among the tied-lowest set
/// must be rejected. Without the `ignorable_indices` check, a client could
/// ignore the highest roll instead.
#[test]
fn ignoring_a_non_lowest_roll_is_rejected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    let mut runner = scenario.build();

    // Drive the frame directly with a hand-built pending roll whose naturals
    // have a unique lowest at index 0 but which offers a two-element choice —
    // the shape a tie produces, with a deliberately illegal submission.
    let pending = engine::types::resolution::PendingDieRoll {
        source_id: source,
        controller: P0,
        roller: P0,
        targets: vec![],
        sides: 6,
        results: vec![2, 2, 6],
        ignore_rules: vec![DieRollIgnoreRule::Lowest],
        results_table: vec![],
        modifier: None,
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
    };
    runner.state_mut().push_die_roll_frame(pending);
    runner.state_mut().waiting_for = WaitingFor::DieKeepChoice {
        player: P0,
        results: vec![2, 2, 6],
        ignorable_indices: vec![0, 1],
        ignore_count: 1,
    };

    // Index 2 (the 6) is not tied for the lowest.
    let rejected = runner.act(GameAction::SelectDieRolls {
        ignore_indices: vec![2],
    });
    assert!(
        rejected.is_err(),
        "CR 706.6: only a roll tied for the lowest may be ignored"
    );

    // V3 empty/decline path: submitting nothing when one roll must be ignored.
    let rejected = runner.act(GameAction::SelectDieRolls {
        ignore_indices: vec![],
    });
    assert!(
        rejected.is_err(),
        "CR 706.6: exactly `ignore_count` rolls must be ignored"
    );

    // The legal submission is accepted, and emits the two survivors.
    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![1],
        })
        .expect("a tied-lowest index is legal");
    let rolls = die_rolls(&result.events);
    assert_eq!(
        rolls.len(),
        2,
        "CR 706.6: the two non-ignored rolls survive, got {rolls:?}"
    );
    assert_eq!(
        rolls,
        vec![2, 6],
        "CR 706.6: the survivors keep their roll order and values"
    );
}

/// V13 — a suspended `DieKeepChoice` leaves the resolution stack non-empty, so
/// `priority_checkpoint_is_settled` already returns false through its existing
/// `resolution_stack.is_empty()` clause. No new clause was added, and
/// `die_result_this_resolution` must NOT be cleared to satisfy it.
///
/// The post-drain assertion is the positive control: without it, a test that
/// merely observed "not settled" would also pass if the frame never popped.
#[test]
fn suspended_die_choice_leaves_the_resolution_stack_occupied() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // The scenario builder parses the oracle text and registers the CR 706.6
    // replacement itself, so this is the real parser → applier → resolver path.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();
    let _ = resolve_roll(&mut runner, source, 1, 1, None, vec![]);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "reach-guard: the tie must have opened a choice"
    );
    assert!(
        !runner.state().resolution_stack.is_empty(),
        "the parked die-roll frame keeps the resolution stack occupied"
    );

    runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("submit the ignore choice");
    assert!(
        runner.state().resolution_stack.is_empty(),
        "positive control: the frame must pop once the choice is submitted"
    );
}

/// V14 — a chained sub-ability ("Roll a d20. <effect> equal to the result") must
/// not resolve while the keep-choice is open, and must read the SURVIVING
/// aggregate once it settles.
///
/// This is the assertion that carries both invisible events-slice consumers:
/// `recent_roll_difference` (`game/contraptions.rs`) and
/// `snapshot_resolution_context_quantity` (`game/effects/effect.rs`) depend on
/// the same suspension for the same reason.
#[test]
fn chained_result_effect_waits_for_the_ignore_choice_and_reads_the_survivor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // The scenario builder parses the oracle text and registers the CR 706.6
    // replacement itself, so this is the real parser → applier → resolver path.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    // CR 706.3a: a results table branch is the in-resolution consumer of a die
    // result. A d1 forces a tie, so the branch must not run until the ignore is
    // submitted.
    let branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            player: engine::types::ability::TargetFilter::Controller,
        },
    );
    let (_events, _) = resolve_roll(
        &mut runner,
        source,
        1,
        1,
        None,
        vec![DieResultBranch {
            min: 1,
            max: 1,
            effect: Box::new(branch),
        }],
    );

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "reach-guard: the tie must have opened a choice"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "CR 608.2c: the results branch must NOT resolve while the choice is open"
    );

    runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("submit the ignore choice");

    let life_after = runner.state().players[P0.0 as usize].life;
    // Reach-guard FIRST: a dropped die result yields a +0 gain, which would
    // vacuously satisfy any "not the ignored value" assertion.
    assert!(
        life_after > life_before,
        "reach-guard: the chained result effect must have fired at all (life {life_before} → \
         {life_after})"
    );
    // CR 706.6: exactly one d1 survives, so exactly 1 life is gained — not 2
    // (both dice) and not 0 (a wiped die result).
    assert_eq!(
        life_after - life_before,
        1,
        "CR 706.4 + CR 706.6: the branch must read the SURVIVING roll alone"
    );
}

// --- CR 616.1: TWO die-roll replacements on the same instruction ---

/// Pixie Guide, verified verbatim against Scryfall (the CR 207.2c ability word
/// "Grant an Advantage" carries no rules meaning and is peeled by the parser):
/// > Grant an Advantage — If you would roll one or more dice, instead roll that
/// > many dice plus one and ignore the lowest roll.
const PIXIE_GUIDE: &str = "Grant an Advantage — If you would roll one or more dice, instead \
roll that many dice plus one and ignore the lowest roll.";

/// CR 616.1 (regression): with TWO applicable `You`-scoped die-roll
/// replacements, the affected player orders them — and the roll must actually
/// happen afterwards.
///
/// This is the marquee interaction of the whole card class: Barbarian Class and
/// Pixie Guide are both legal in the same Commander dice-matters deck. Before
/// the fix, the resume arm for `ProposedEvent::RollDice` was a `debug_assert!`
/// no-op on the premise that mandatory replacements never surface a CR 616.1
/// ordering choice. They do — mandatory means "cannot be declined", not "cannot
/// compete" — so this panicked in debug and, in release, silently dropped the
/// entire roll: no `DieRolled`, no branches, and a stale die result.
///
/// Reach-guard FIRST: the failure value here is "zero rolls", which would
/// vacuously satisfy any "the count is not inflated" assertion. So assert the
/// ordering prompt was reached, then that dice were emitted AT ALL, and only
/// then how many.
#[test]
fn two_stacked_die_roll_replacements_still_roll_the_dice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // Both parse to the same `You`-scoped RollDice replacement, so both apply to
    // the same instruction and CR 616.1 makes P0 order them.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    scenario.add_enchantment_from_oracle(P0, "Pixie Guide", PIXIE_GUIDE);
    let mut runner = scenario.build();

    let (proposal_events, _) = resolve_roll(&mut runner, source, 1, 20, None, vec![]);

    // Reach-guard: the CR 616.1 ordering prompt must actually have opened, or
    // the rest of this test proves nothing about the stacked path.
    let WaitingFor::ReplacementChoice {
        candidate_count, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 616.1: two applicable die-roll replacements must surface an ordering choice, got \
             {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidate_count, 2,
        "CR 616.1: both die-roll replacements must be offered"
    );
    // Nothing may have been rolled yet — CR 614.1a routes the instruction
    // through the pipeline BEFORE the RNG.
    assert!(
        die_rolls(&proposal_events).is_empty(),
        "CR 614.1a: no die may be rolled before the ordering choice is settled"
    );

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("CR 616.1: ordering one of two die-roll replacements must be accepted");

    // The choice may itself have opened a CR 706.6 tie-break; drain it by
    // ignoring the offered rolls.
    let mut rolls = die_rolls(&result.events);
    if let WaitingFor::DieKeepChoice {
        ignorable_indices,
        ignore_count,
        ..
    } = runner.state().waiting_for.clone()
    {
        let follow_up = runner
            .act(GameAction::SelectDieRolls {
                ignore_indices: ignorable_indices.into_iter().take(ignore_count).collect(),
            })
            .expect("CR 706.6: the tie-break submission must be accepted");
        rolls.extend(die_rolls(&follow_up.events));
    }

    // Reach-guard: dice were emitted at all. This is the assertion the old
    // no-op arm failed — in release it emitted nothing and reported success.
    assert!(
        !rolls.is_empty(),
        "CR 616.1 + CR 706.2: the ordered instruction must still roll its dice"
    );
    // CR 706.6: three dice are rolled (1 → 2 → 3, each replacement composing its
    // `Offset { EventContextAmount, +1 }`) and TWO are ignored — CR 706.6
    // applies once per instructing effect — leaving exactly one survivor.
    assert_eq!(
        rolls.len(),
        1,
        "CR 706.6: two applied replacements must ignore two rolls, leaving one survivor, got \
         {rolls:?}"
    );
}

/// CR 706.6 (regression): two applied replacements ignore TWO rolls, so the
/// aggregate "equal to the result(s)" reads ONE die — not two.
///
/// Before the fix, `roll_dice_applier` collapsed the two ignore rules with
/// `.or()` and the ignore count was pinned to 1, so three dice were rolled and
/// only one was ignored: every aggregate, every results-table branch, and every
/// d20 outcome was inflated by an extra surviving die.
///
/// A d1 makes this exact rather than statistical: every natural is 1, so the
/// CR-correct aggregate is exactly 1 and the buggy two-survivor aggregate is
/// exactly 2. Reach-guard FIRST — a dropped roll yields 0, which would
/// vacuously satisfy "the aggregate is not 2".
#[test]
fn two_stacked_die_roll_replacements_ignore_two_rolls() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    scenario.add_enchantment_from_oracle(P0, "Wyll, Blade of Frontiers", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();

    // CR 706.3a: a results-table branch is an in-resolution consumer of the
    // aggregate, so it reads the survivors-only value BEFORE the action boundary
    // clears `die_result_this_resolution`. A d1 forces every natural to 1, so
    // the CR-correct life gain is exactly 1 (one survivor) and the pre-fix
    // two-survivor bug would gain exactly 2.
    let life_before = runner.state().players[P0.0 as usize].life;
    let branch = DieResultBranch {
        min: 1,
        max: 1,
        effect: Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: engine::types::ability::TargetFilter::Controller,
            },
        )),
    };
    resolve_roll(&mut runner, source, 1, 1, None, vec![branch]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: two applicable replacements must surface a CR 616.1 ordering choice"
    );

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("ordering must be accepted");
    let mut rolls = die_rolls(&result.events);
    if let WaitingFor::DieKeepChoice {
        ignorable_indices,
        ignore_count,
        ..
    } = runner.state().waiting_for.clone()
    {
        // CR 706.6: with every d1 natural tied, the roller breaks the tie — and
        // the prompt must ask for TWO picks, one per applied replacement.
        assert_eq!(
            ignore_count, 2,
            "CR 706.6: each applied replacement instructs the roller to ignore one roll"
        );
        let follow_up = runner
            .act(GameAction::SelectDieRolls {
                ignore_indices: ignorable_indices.into_iter().take(ignore_count).collect(),
            })
            .expect("the two-pick tie-break must be accepted");
        rolls.extend(die_rolls(&follow_up.events));
    }

    // Reach-guard: the instruction produced a result at all.
    assert!(
        !rolls.is_empty(),
        "reach-guard: the stacked instruction must emit a surviving roll"
    );
    // CR 706.6: three d1s rolled, two ignored — one survivor, aggregate 1.
    // CR 706.6: three d1s rolled, two ignored — exactly one survivor reaches
    // the log. The aggregate `die_result_this_resolution` is deliberately NOT
    // asserted here: it is resolution-scoped and `apply()` clears it at the
    // action boundary this test crosses. The emitted survivors ARE the
    // observable proof that the aggregate summed one die and not two, because
    // `resume_after_ignore` derives both from the same survivor loop.
    assert_eq!(
        rolls,
        vec![1],
        "CR 706.6: exactly one d1 may survive two stacked ignore instructions"
    );

    // CR 706.3a + CR 706.6: each SURVIVING die consults the results table
    // independently, so the branch fires once — not three times (no ignore) and
    // not twice (the pre-fix single-ignore bug).
    let gained = runner.state().players[P0.0 as usize].life - life_before;
    assert!(
        gained > 0,
        "reach-guard: the results branch must have fired at all"
    );
    assert_eq!(
        gained, 1,
        "CR 706.6: two applied replacements ignore two rolls, so the table runs for ONE survivor"
    );
}

/// CR 706.6 + #6942 (regression) — the AI candidate enumerator must offer a
/// legal submission at EVERY `ignore_count`, not just 1.
///
/// `AiDecisionContract::contains_action` falls through to the candidate list, so
/// an empty candidate set also rejects `phase-ai`'s `fallback_action` rescue —
/// the AI seat then has no legal action at all and the game softlocks. That is
/// exactly the empty-selection class #6942 fixed, and `ignore_count == 2` is
/// reachable from two cards legal in the same deck (Barbarian Class + Wyll).
///
/// Asserted at the building-block level over the enumerator itself rather than
/// through one card, and the `ignore_count == 1` row is included so a
/// regression that empties the WHOLE arm cannot pass by matching only the
/// stacked row.
#[test]
fn ai_candidates_cover_every_ignore_count() {
    for (ignorable, ignore_count, expected_combinations) in [
        // One replacement, a two-way tie: two single-index submissions.
        (vec![0_usize, 1], 1_usize, 2_usize),
        // TWO stacked replacements over three tied naturals: C(3, 2) = 3.
        (vec![0, 1, 2], 2, 3),
        // Two stacked replacements with only two candidates: C(2, 2) = 1.
        (vec![0, 1], 2, 1),
        // Beyond the shared selection pool cap the enumerator degrades to the
        // single deterministic take-`ignore_count` submission rather than
        // enumerating C(20, 2) — still NON-EMPTY, which is the property that
        // keeps the AI seat unstuck.
        ((0..20).collect::<Vec<usize>>(), 2, 1),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature(P0, "Roller", 1, 1).id();
        let mut runner = scenario.build();

        let results: Vec<u8> = vec![4; ignorable.len().max(1)];
        runner
            .state_mut()
            .push_die_roll_frame(engine::types::resolution::PendingDieRoll {
                source_id: source,
                controller: P0,
                roller: P0,
                targets: vec![],
                sides: 6,
                results: results.clone(),
                ignore_rules: vec![DieRollIgnoreRule::Lowest; ignore_count],
                results_table: vec![],
                modifier: None,
                die_result: None,
                next_index: 0,
                running_total: 0,
                rolled_any: false,
                forced_ignored: vec![],
            });
        runner.state_mut().waiting_for = WaitingFor::DieKeepChoice {
            player: P0,
            results,
            ignorable_indices: ignorable.clone(),
            ignore_count,
        };

        let actions = engine::ai_support::legal_actions(runner.state());
        // Reach-guard: the failure mode under test is an EMPTY set, which every
        // "no illegal action is offered" assertion would pass vacuously.
        assert!(
            !actions.is_empty(),
            "#6942: an AI seat must never face an empty legal-action set \
             (ignorable={ignorable:?}, ignore_count={ignore_count})"
        );

        let submissions: Vec<Vec<usize>> = actions
            .iter()
            .filter_map(|action| match action {
                GameAction::SelectDieRolls { ignore_indices } => Some(ignore_indices.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            submissions.len(),
            expected_combinations,
            "CR 706.6: the enumerator must offer every C(ignorable, ignore_count) pick \
             below the selection cap, and the forced pick above it \
             (ignorable={ignorable:?}, ignore_count={ignore_count})"
        );

        // Every offered submission must be one the engine actually accepts:
        // the right cardinality, drawn only from the engine-narrowed pool, with
        // no index repeated.
        for submission in &submissions {
            assert_eq!(
                submission.len(),
                ignore_count,
                "CR 706.6: a submission must ignore exactly `ignore_count` rolls"
            );
            assert!(
                submission.iter().all(|index| ignorable.contains(index)),
                "CR 706.6: only an engine-narrowed index may be ignored"
            );
            let mut deduped = submission.clone();
            deduped.sort_unstable();
            deduped.dedup();
            assert_eq!(
                deduped.len(),
                submission.len(),
                "CR 706.6: one roll cannot be ignored twice"
            );
        }
    }
}

/// CR 706.3a + CR 608.2c - a MULTI-die instruction whose results-table branch is
/// itself interactive must run that branch for EVERY surviving die, not just the
/// first one.
///
/// Path-divergence guard. Every other fixture in this file rolls a single
/// surviving die, so the per-die results loop never takes a second iteration and
/// a mid-loop suspension that abandoned the remainder could not manifest. This
/// test is deliberately count=2 with a branch that raises its own `WaitingFor`:
/// the first die suspends the loop, and the instruction is only correct if the
/// second die still runs its branch after that choice settles.
///
/// Assertions are ordered reach-guard FIRST - the failure mode is "the second
/// branch never ran", whose signature is an UNCHANGED hand size, exactly what a
/// naive "the hand shrank" assertion would also see after only the first
/// discard.
#[test]
fn every_surviving_die_runs_its_interactive_branch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // Four cards so both discards have a real choice to make; a forced single
    // card could auto-resolve and hide a missing prompt.
    scenario.with_cards_in_hand(P0, &["Card A", "Card B", "Card C", "Card D"]);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        4,
        "reach-guard: the discard branch needs a stocked hand"
    );

    // CR 706.3a: each die consults the table independently. A d1 makes both
    // naturals 1, so both dice take this branch - and `DiscardCard` opens a
    // `WaitingFor` per die, which is the suspension under test.
    let branch = DieResultBranch {
        min: 1,
        max: 1,
        effect: Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DiscardCard {
                count: 1,
                target: engine::types::ability::TargetFilter::Controller,
            },
        )),
    };
    let (_events, _) = resolve_roll(&mut runner, source, 2, 1, None, vec![branch]);

    // Reach-guard: die 1's branch must have suspended, or the rest of this test
    // proves nothing about resuming.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach-guard: die 1's interactive branch must have opened a choice, got {:?}",
        runner.state().waiting_for
    );

    // Settle die 1's discard.
    let first_card = runner.state().players[P0.0 as usize].hand[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![first_card],
        })
        .expect("die 1's discard resolves");
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        3,
        "reach-guard: die 1's branch actually discarded"
    );

    // THE REGRESSION: die 2 still owes its branch. Without the resume cursor the
    // loop returned mid-iteration with no way back in, so die 2's branch was
    // silently abandoned and the hand stayed at 3.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "CR 706.3a: die 2 must still run its branch after die 1's choice settles \
         - a mid-loop suspension must not abandon the remaining dice"
    );
    let second_card = runner.state().players[P0.0 as usize].hand[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![second_card],
        })
        .expect("die 2's discard resolves");
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        2,
        "CR 706.3a: BOTH surviving dice ran their results branch"
    );
}

/// CR 706.6 - a stacked run of "ignore the lowest" must ignore the N LOWEST
/// rolls as one determined set, never a union of each rule's tied set.
///
/// Barbarian Class's own Gatherer ruling states the multi-copy case: "if you
/// have multiple Barbarian Class cards, you roll that many additional dice and
/// ignore that many of the lowest rolls." Over naturals `[4, 7, 7]` with two
/// copies, the 4 is DETERMINED - it is not the roller's to keep - and only the
/// tie between the two 7s is a real choice.
///
/// The bug this guards: accumulating each rule's tied set into a flat union
/// offered `{0, 1, 2}` for two picks, which let the roller ignore BOTH 7s and
/// keep the 4 - a roll CR 706.6 does not permit them to drop, inflating the
/// surviving aggregate.
#[test]
fn stacked_lowest_rules_force_the_lowest_rolls_and_only_tie_break_the_boundary() {
    let two_lowest = [DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest];

    // Fully determined: the two lowest are distinct values, so no prompt at all.
    let (candidates, ignore_count) =
        DieRollIgnoreRule::ignorable_indices_for_rules(&two_lowest, &[4, 4, 7]);
    assert_eq!(
        (candidates.as_slice(), ignore_count),
        (&[0usize, 1][..], 2),
        "CR 706.6: with two rules over [4, 4, 7] both 4s go and the 7 survives"
    );
    let outcome = DieRollIgnoreRule::ignore_outcome_for_rules(&two_lowest, &[4, 4, 7]);
    assert!(
        !outcome.needs_choice(),
        "CR 706.6: a determined set must raise NO prompt - the ruling says \
         'ignore that many of the lowest rolls', with no player decision"
    );
    assert_eq!(outcome.forced, vec![0, 1]);

    // A genuine boundary tie: the 4 is forced, and the roller picks ONE of the
    // two 7s. The forced roll must NOT be offered as a candidate to pick around.
    let outcome = DieRollIgnoreRule::ignore_outcome_for_rules(&two_lowest, &[4, 7, 7]);
    assert!(
        outcome.needs_choice(),
        "reach-guard: the tie at the boundary is a real CR 706.6 choice"
    );
    assert_eq!(
        outcome.forced,
        vec![0],
        "CR 706.6: the 4 is the lowest and is not the roller's to keep"
    );
    assert_eq!(
        outcome.tied,
        vec![1, 2],
        "CR 706.6: only the tied 7s are offered - the forced 4 must not appear, \
         or the roller could ignore both 7s and keep it"
    );
    assert_eq!(
        outcome.picks_from_tied(),
        1,
        "CR 706.6: one pick remains once the forced roll is accounted for"
    );
}

/// CR 706.6 - the runtime honors the forced/tied split: with two stacked
/// "ignore the lowest" rules over `[4, 7, 7]`, whichever 7 the roller picks, the
/// 4 is ALWAYS gone.
///
/// The end-to-end counterpart of the unit test above. Asserting the surviving
/// event log (not just the candidate math) is what proves the forced half is
/// actually re-unioned in on the resume path rather than dropped with the
/// prompt.
#[test]
fn a_forced_lowest_roll_cannot_be_kept_by_picking_around_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .push_die_roll_frame(engine::types::resolution::PendingDieRoll {
            source_id: source,
            controller: P0,
            roller: P0,
            targets: vec![],
            sides: 20,
            results: vec![4, 7, 7],
            ignore_rules: vec![DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
            results_table: vec![],
            modifier: None,
            die_result: None,
            next_index: 0,
            running_total: 0,
            rolled_any: false,
            // The 4 is determined; only the tied 7s were offered to the roller.
            forced_ignored: vec![0],
        });
    runner.state_mut().waiting_for = WaitingFor::DieKeepChoice {
        player: P0,
        results: vec![4, 7, 7],
        ignorable_indices: vec![1, 2],
        ignore_count: 1,
    };

    // The roller ignores a 7 - the pick that, under the flat-union bug, would
    // have combined with the other 7 to leave the 4 alive.
    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![1],
        })
        .expect("submit the tie-break");

    let rolls = die_rolls(&result.events);
    assert_eq!(
        rolls.len(),
        1,
        "reach-guard: exactly one of three rolls survives two ignore rules"
    );
    assert_eq!(
        rolls,
        vec![7],
        "CR 706.6: the forced 4 is ignored alongside the roller's chosen 7 - it \
         must not survive by being left out of the prompt"
    );
}

/// CR 706.4 + CR 608.2c - a `SelectDieRolls` submission whose results-table
/// branch RE-SUSPENDS mid-loop must leave the die context in place for the
/// re-parked frame, not restore the pre-roll value over it.
///
/// The `SelectDieRolls` handler saves `die_result_this_resolution` before
/// `resume_after_ignore` and restores it after, because `apply()` clears the
/// field at the start of every action. That restore is correct ONLY when the
/// instruction actually finished. When a branch re-suspends, the frame re-parks
/// with the cursor advanced and still owes every remaining die - restoring
/// `prev` there wipes the die context out from under the parked frame, and the
/// `QuantityRef::EventContextAmount` cascade in `game/quantity.rs` then reads an
/// absent value and silently yields 0 instead of erroring.
/// `drain_active_die_roll` documents the same contract for the priority-time
/// drain: restore only on the completed arm.
///
/// Path-divergence guard: `every_surviving_die_runs_its_interactive_branch`
/// already covers the mid-loop re-park, but it reaches that re-park WITHOUT a
/// CR 706.6 prompt, so it never enters the `SelectDieRolls` handler and a wiped
/// context is invisible to it. This fixture is deliberately the intersection -
/// a three-way tie routes through `SelectDieRolls`, AND the branch suspends.
///
/// The assertion is on `die_result_this_resolution` directly rather than on a
/// downstream consumer: the field IS the context every `EventContextAmount`
/// reader shares, and reading it here is exact rather than inferential.
#[test]
fn select_die_rolls_keeps_the_die_context_when_a_branch_re_suspends() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    scenario.with_cards_in_hand(P0, &["Card A", "Card B", "Card C", "Card D"]);
    // Raises the instruction 2 -> 3 and attaches `DieRollIgnoreRule::Lowest`.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        4,
        "reach-guard: the discard branch needs a stocked hand"
    );

    // CR 706.3a: a d1 makes every natural 1, so the "lowest" is a three-way tie
    // the roller must break by hand (the CR 706.6 prompt), and every surviving
    // die takes this one branch. `DiscardCard` opens a `WaitingFor` per die,
    // which is the mid-loop suspension under test.
    let branch = DieResultBranch {
        min: 1,
        max: 1,
        effect: Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DiscardCard {
                count: 1,
                target: engine::types::ability::TargetFilter::Controller,
            },
        )),
    };
    let (_events, _) = resolve_roll(&mut runner, source, 2, 1, None, vec![branch]);

    // Reach-guard: the three-way tie must have opened the CR 706.6 prompt, or
    // this test never exercises the `SelectDieRolls` handler at all.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "reach-guard: a three-way tie must open the CR 706.6 keep-choice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("submit the ignore choice");

    // Reach-guard: die 1's branch must have suspended on its discard - that
    // re-park is the whole subject of this test.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "reach-guard: die 1's branch must have re-suspended for a discard, got {:?}",
        runner.state().waiting_for
    );

    // THE REGRESSION: restoring `prev_die_result` on the re-suspend arm left
    // this `None`, so every `EventContextAmount` read by the re-parked frame's
    // remaining branches resolved against an absent die context and silently
    // produced 0 rather than erroring.
    assert_eq!(
        runner.state().die_result_this_resolution,
        Some(1),
        "CR 706.4: the re-parked frame's die context must survive the \
         `SelectDieRolls` submission, not be restored over"
    );

    // And the instruction genuinely continues: die 2 still owes its branch.
    let first_card = runner.state().players[P0.0 as usize].hand[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![first_card],
        })
        .expect("die 1's discard resolves");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "CR 706.3a: die 2 must still run its branch after die 1's choice settles"
    );
    let second_card = runner.state().players[P0.0 as usize].hand[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![second_card],
        })
        .expect("die 2's discard resolves");
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        2,
        "CR 706.6: exactly two of the three d1s survive, and BOTH ran their branch"
    );
}

/// CR 706.3a + CR 608.2c (regression): a results-table branch that suspends on
/// a STACK-RESIDENT direct-choice owner must not be buried by the die-roll
/// frame's re-park.
///
/// Path-divergence guard, and the whole reason this fixture exists alongside
/// `select_die_rolls_keeps_the_die_context_when_a_branch_re_suspends`: that test
/// suspends on a `DiscardChoice`, which owns no resolution frame, so the re-park
/// reaches the EMPTY-stack fallback and a blind push is harmless there. An
/// OPTIONAL branch effect installs a real `ResolutionFrame` whose gate is
/// `FrameGate::DirectChoice`, and `ResolutionStack::validate` requires a
/// direct-choice owner to be the TOP frame (`buried_direct_choice`). Past cursor
/// 0 the die-roll frame is an `AfterChild` owner, so it belongs BELOW that
/// child. The former `if replace(..).is_err() { push(..) }` collapsed
/// `Empty` and `UnexpectedTop` into one branch and pushed the die-roll frame on
/// top of the live direct-choice owner, violating that invariant.
///
/// A d1 makes every natural 1, so all three rolls tie for lowest and the roller
/// breaks the tie by hand — routing through the `SelectDieRolls` handler, which
/// is the path that re-parks.
#[test]
fn a_branch_suspending_on_a_resident_direct_choice_is_not_buried_by_the_re_park() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Roller", 1, 1).id();
    // Raises the instruction 2 -> 3 and attaches `DieRollIgnoreRule::Lowest`.
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();

    // An OPTIONAL branch effect: resolving it installs a direct-choice frame and
    // parks a `WaitingFor` for the "you may" decision, unlike the discard
    // fixture's frameless prompt.
    let mut optional_gain = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: engine::types::ability::TargetFilter::Controller,
        },
    );
    optional_gain.optional = true;
    let branch = DieResultBranch {
        min: 1,
        max: 1,
        effect: Box::new(optional_gain),
    };
    let (_events, _) = resolve_roll(&mut runner, source, 2, 1, None, vec![branch]);

    // Reach-guard: without the three-way tie this never enters `SelectDieRolls`,
    // which is the handler that performs the re-park under test.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "reach-guard: a three-way tie must open the CR 706.6 keep-choice, got {:?}",
        runner.state().waiting_for
    );

    // Submitting the ignore choice runs the branch for the first survivor, which
    // suspends on its own optional prompt and forces the mid-loop re-park.
    runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("submit the ignore choice");

    // Reach-guard: the branch must actually have suspended on its own prompt.
    // If it resolved inline there is no resident direct-choice owner and this
    // test proves nothing.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach-guard: the optional branch must have suspended on its own prompt, got {:?}",
        runner.state().waiting_for
    );

    // THE REGRESSION: the stack must still be structurally valid. Under the old
    // blind push the die-roll frame sat ON TOP of the live direct-choice owner,
    // burying it. Answering the prompt is what exercises that: the engine
    // matches the active prompt against the top frame's gate, so a buried owner
    // either rejects the action or resumes the wrong frame.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("CR 608.2c: the branch's own prompt must still be answerable");
}

/// CR 706.6 — a run that must ignore EVERY remaining roll is fully determined,
/// so it reports no choice rather than raising a vacuous prompt offering all of
/// them.
///
/// `ignore_outcome_for_rules` folds a fully-determined tie into `forced`, which
/// is what keeps `needs_choice()` in agreement with the caller's CR 706.6 prompt
/// precondition in `roll_die::resolve` (`picks > 0 && tied.len() > picks`).
/// Without the fold, `ignore_count == naturals.len()` would leave
/// `tied.len() == picks`: `needs_choice()` returns true, the debug assert trips,
/// and a release build asks the roller to "choose" every roll they had.
///
/// Not reachable from today's parser — every parsed rule arrives bundled with a
/// `+1` count raise, keeping `naturals.len() > ignore_count` — so the invariant
/// is pinned at the type level here instead.
#[test]
fn a_run_ignoring_every_roll_is_determined_not_a_vacuous_prompt() {
    // Two rules over two tied rolls: every roll is owed, so nothing is left to
    // choose between.
    let outcome = DieRollIgnoreRule::ignore_outcome_for_rules(
        &[DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
        &[4, 4],
    );
    assert!(
        !outcome.needs_choice(),
        "CR 706.6: with every roll owed there is no decision to offer, got {outcome:?}"
    );
    assert_eq!(
        outcome.forced,
        vec![0, 1],
        "CR 706.6: determined rolls belong in `forced`, not `tied`"
    );
    assert!(
        outcome.tied.is_empty(),
        "CR 706.6: nothing is left to tie-break once every roll is forced"
    );
    // The prompt precondition in `roll_die::resolve` must agree by construction.
    assert!(
        !(outcome.picks_from_tied() > 0 && outcome.tied.len() > outcome.picks_from_tied()),
        "`needs_choice()` and the CR 706.6 prompt precondition must never disagree"
    );

    // Contrast: with a roll to spare the tie is real and the roller breaks it.
    let outcome =
        DieRollIgnoreRule::ignore_outcome_for_rules(&[DieRollIgnoreRule::Lowest], &[4, 4, 7]);
    assert!(
        outcome.needs_choice(),
        "CR 706.6: with a roll to spare the roller genuinely chooses, got {outcome:?}"
    );
    assert_eq!(
        outcome.tied,
        vec![0, 1],
        "CR 706.6: only the rolls tied for the lowest are legal picks"
    );
}
