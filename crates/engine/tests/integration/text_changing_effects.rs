//! CR 612: Text-changing effects — word replacement. Runtime regressions driving
//! the real cast pipeline (parse → cast → resolve → `WaitingFor::TextWordReplacement`
//! → `GameAction::ChooseTextWordReplacement` → Layer-3 continuous effect).

use engine::game::layers::{flush_layers, prune_end_of_turn_effects};
use engine::game::scenario::{GameScenario, P0};
use engine::game::text_substitution::collect_present_words;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, BasicLandType, Duration, Effect, TextWord, TextWordCategory,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{TextWordReplacementOption, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const SLEIGHT_OF_MIND: &str = "Change the text of target spell or permanent by replacing all instances of one color word with another.";
const ARTIFICIAL_EVOLUTION: &str = "Change the text of target permanent by replacing all instances of one creature type with another.";

/// Find the index of the option matching `(from, to)` in the current
/// `WaitingFor::TextWordReplacement`, panicking with a useful message otherwise.
fn choose_index(
    runner: &engine::game::scenario::GameRunner,
    from: TextWord,
    to: TextWord,
) -> usize {
    match &runner.state().waiting_for {
        WaitingFor::TextWordReplacement { options, .. } => options
            .iter()
            .position(|o| o.from == from && o.to == to)
            .unwrap_or_else(|| panic!("no option {from:?}->{to:?} among {options:?}")),
        other => panic!("expected TextWordReplacement, got {other:?}"),
    }
}

/// CR 612.2: a color word used in a keyword parameter (`protection from red`) is
/// text-changed. Revert guard: deleting the walker's `Keyword::Protection` /
/// `ProtectionTarget::Color` arm leaves the keyword `red` and flips this test.
#[test]
fn color_word_in_protection_keyword_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario
        .add_creature(P0, "Ruby Sentinel", 2, 2)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).target_object(creature).resolve();

    let index = choose_index(
        &runner,
        TextWord::Color(ManaColor::Red),
        TextWord::Color(ManaColor::Blue),
    );
    runner
        .act(GameAction::ChooseTextWordReplacement { index })
        .expect("submit text-word choice");
    flush_layers(runner.state_mut());

    let keywords = &runner.state().objects[&creature].keywords;
    assert!(
        keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Blue
        ))),
        "protection should now be from blue: {keywords:?}"
    );
    assert!(
        !keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Red
        ))),
        "protection from red must be gone: {keywords:?}"
    );
}

/// CR 612.2 structural exclusion: a text-change never rewrites a card name even
/// when it contains a color/type substring. Positive reach-guard: a real color
/// keyword on the same object DID change, proving the input reached the walker.
#[test]
fn card_name_is_not_text_changed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario
        .add_creature(P0, "Whitemane Lion", 2, 2)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(
            ManaColor::White,
        )))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).target_object(creature).resolve();

    let index = choose_index(
        &runner,
        TextWord::Color(ManaColor::White),
        TextWord::Color(ManaColor::Blue),
    );
    runner
        .act(GameAction::ChooseTextWordReplacement { index })
        .expect("submit text-word choice");
    flush_layers(runner.state_mut());

    let obj = &runner.state().objects[&creature];
    // Name (and base name) untouched even though it contains "white".
    assert_eq!(obj.name, "Whitemane Lion");
    assert_eq!(obj.base_name, "Whitemane Lion");
    // Positive reach-guard: the real color keyword ref DID change.
    assert!(
        obj.keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Blue
            ))),
        "the rules-text 'white' ref should have become blue: {:?}",
        obj.keywords
    );
}

/// CR 612.2 + CR 205.3: a creature-type word on the type line is text-changed.
/// Revert guard: dropping the `card_types.subtypes` walk root leaves "Zombie".
#[test]
fn creature_type_on_type_line_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario
        .add_creature(P0, "Shambler", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();

    let mut runner = scenario.build();
    // CR 205.3m: the legal creature-type words come from the live type set.
    runner.state_mut().all_creature_types =
        vec!["Zombie".to_string(), "Elf".to_string(), "Wall".to_string()];

    runner.cast(spell).target_object(creature).resolve();

    let index = choose_index(
        &runner,
        TextWord::CreatureType("Zombie".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );
    runner
        .act(GameAction::ChooseTextWordReplacement { index })
        .expect("submit text-word choice");
    flush_layers(runner.state_mut());

    let subtypes = &runner.state().objects[&creature].card_types.subtypes;
    assert!(
        subtypes.iter().any(|s| s == "Elf"),
        "expected Elf: {subtypes:?}"
    );
    assert!(
        !subtypes.iter().any(|s| s == "Zombie"),
        "Zombie must be gone: {subtypes:?}"
    );
}

/// CR 609.3: when the target has no word of the chosen category, the effect does
/// nothing — no `WaitingFor::TextWordReplacement`, no continuous effect. Paired
/// positive: the color test above proves the same pipeline DOES pause when a word
/// is present, so this negative is not vacuous.
#[test]
fn no_color_word_present_is_a_no_op() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature: ObjectId = scenario.add_creature(P0, "Grey Ogre", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).target_object(creature).resolve();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TextWordReplacement { .. }
        ),
        "no color word present must not raise a replacement choice: {:?}",
        runner.state().waiting_for
    );
    // The vanilla creature is unchanged.
    assert!(runner.state().objects[&creature].keywords.is_empty());
}

// Verbatim (reminder-stripped) Oracle text driven through the real parser.
const CRYSTAL_SPRAY: &str = "Change the text of target spell or permanent by \
    replacing all instances of one color word with another or one basic land \
    type with another until end of turn.\nDraw a card.";
const MAGICAL_HACK: &str = "Change the text of target spell or permanent by \
    replacing all instances of one basic land type with another.";
// CR 612.2: the excluded-`to` rider is a second sentence; Task-1's continuation
// absorber must push Wall into `excluded_to`.
const ARTIFICIAL_EVOLUTION_FULL: &str = "Change the text of target spell or \
    permanent by replacing all instances of one creature type with another. \
    The new creature type can't be Wall.";

/// Submit the chosen replacement and re-derive layers. Panics with a useful
/// message if the expected `(from, to)` option is not offered.
fn apply_replacement(
    runner: &mut engine::game::scenario::GameRunner,
    from: TextWord,
    to: TextWord,
) {
    let index = choose_index(runner, from, to);
    runner
        .act(GameAction::ChooseTextWordReplacement { index })
        .expect("submit text-word choice");
    flush_layers(runner.state_mut());
}

/// CR 611.2b + CR 514.2 (plan 5): an "until end of turn" text change (Crystal
/// Spray) installs a Layer-3 TCE that is pruned at cleanup, while an indefinite
/// change (Sleight of Mind) persists past cleanup. Revert guard: if Crystal
/// Spray's duration were mis-wired to `Permanent`, the post-cleanup assertion
/// that protection reverts to red would fail.
#[test]
fn until_end_of_turn_change_expires_indefinite_persists() {
    // --- Crystal Spray: expires at cleanup. ---
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Ruby Sentinel", 2, 2)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Crystal Spray", true, CRYSTAL_SPRAY)
        .id();
    // Crystal Spray's trailing "Draw a card" must draw from a non-empty library,
    // else the caster decks out (CR 104.3c) and the game ends before the swap.
    scenario.with_library_top(P0, &["Plains", "Plains"]);
    let mut runner = scenario.build();
    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Red),
        TextWord::Color(ManaColor::Blue),
    );
    // The swap took effect this turn.
    assert!(
        runner.state().objects[&creature]
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Blue
            ))),
        "protection should be blue during the turn: {:?}",
        runner.state().objects[&creature].keywords
    );
    // CR 514.2: cleanup prunes the UntilEndOfTurn TCE; re-derive layers.
    prune_end_of_turn_effects(runner.state_mut());
    flush_layers(runner.state_mut());
    let keywords = &runner.state().objects[&creature].keywords;
    assert!(
        keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Red
        ))),
        "the until-end-of-turn change must be GONE after cleanup: {keywords:?}"
    );
    assert!(
        !keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Blue
        ))),
        "blue must not persist past cleanup: {keywords:?}"
    );

    // --- Sleight of Mind: persists past cleanup (indefinite). ---
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Ruby Sentinel", 2, 2)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Red),
        TextWord::Color(ManaColor::Blue),
    );
    prune_end_of_turn_effects(runner.state_mut());
    flush_layers(runner.state_mut());
    let keywords = &runner.state().objects[&creature].keywords;
    assert!(
        keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Blue
        ))),
        "indefinite change must PERSIST past cleanup: {keywords:?}"
    );
}

/// CR 612.2 (plan 6): a color word in a static ability's `affected` filter (an
/// anthem — "Black creatures get +1/+1") is text-changed. Revert guard: dropping
/// the `walk_static_definition` → `affected` recursion in the walker means the
/// black color word is neither collected (reach guard fails) nor rewritten.
#[test]
fn color_word_in_static_affected_filter_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let anthem = scenario
        .add_creature(P0, "Bad Moon", 1, 1)
        .from_oracle_text("Black creatures get +1/+1.")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();
    let mut runner = scenario.build();

    // Positive reach-guard: the anthem's only color word lives in its static's
    // `affected` filter, so the walker seeing black proves it descends there.
    let before = collect_present_words(
        &runner.state().objects[&anthem],
        TextWordCategory::ColorWord,
    );
    assert!(
        before.contains(&TextWord::Color(ManaColor::Black)),
        "anthem's static affected filter should carry the black color word: {before:?}"
    );

    runner.cast(spell).target_object(anthem).resolve();
    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Black),
        TextWord::Color(ManaColor::Red),
    );

    let after = collect_present_words(
        &runner.state().objects[&anthem],
        TextWordCategory::ColorWord,
    );
    assert!(
        after.contains(&TextWord::Color(ManaColor::Red)),
        "the static's affected color word should now be red: {after:?}"
    );
    assert!(
        !after.contains(&TextWord::Color(ManaColor::Black)),
        "black must be gone from the static filter: {after:?}"
    );
}

/// CR 612.2: a color word that lives ONLY inside an ability's effect target
/// filter (an activated "{T}: Destroy target red creature") is text-changed.
/// This is the sub-class the walker previously under-applied: `walk_effect`
/// classified `Destroy` as a leaf no-op and never descended into its `target`
/// `TargetFilter`, so the `red` instance was neither offered nor rewritten.
///
/// Revert guard: without the `walk_effect` → `Effect::target_filter_mut()` →
/// `walk_target_filter` recursion, the vanilla creature carries no other color
/// word, so `collect_present_words` returns empty. The positive reach-guard
/// (`before` contains red) then fails, and — because no color word is present —
/// the cast raises no `WaitingFor::TextWordReplacement`, so `apply_replacement`
/// panics. Both flip red→green only with the recursion in place.
#[test]
fn color_word_in_effect_target_filter_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Gatekeeper", 2, 2)
        .from_oracle_text("{T}: Destroy target red creature.")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();
    let mut runner = scenario.build();

    // Positive reach-guard: the creature's ONLY color word lives in its activated
    // ability's `Destroy { target }` filter, so the walker seeing red proves it
    // now descends into the effect target filter (empty pre-fix).
    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::ColorWord,
    );
    assert!(
        before.contains(&TextWord::Color(ManaColor::Red)),
        "the effect target filter should carry the red color word: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Red),
        TextWord::Color(ManaColor::Blue),
    );

    // The only color carrier is the effect target filter, so re-collecting proves
    // that filter now reads blue and no longer reads red (CR 612.2 completeness).
    let after = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::ColorWord,
    );
    assert!(
        after.contains(&TextWord::Color(ManaColor::Blue)),
        "the effect target filter's color word should now be blue: {after:?}"
    );
    assert!(
        !after.contains(&TextWord::Color(ManaColor::Red)),
        "red must be gone from the effect target filter: {after:?}"
    );
}

/// CR 612.2 + CR 702.14 (plan 4): a basic land type in a landwalk keyword is
/// text-changed (Magical Hack: Mountain → Island). Revert guard: dropping the
/// `walk_keyword` `Landwalk` arm leaves Mountainwalk.
#[test]
fn basic_land_type_in_landwalk_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Mountain Strider", 2, 2)
        .with_keyword(Keyword::Landwalk("Mountain".to_string()))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Magical Hack", true, MAGICAL_HACK)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::BasicLandType(BasicLandType::Mountain),
        TextWord::BasicLandType(BasicLandType::Island),
    );
    let keywords = &runner.state().objects[&creature].keywords;
    assert!(
        keywords.contains(&Keyword::Landwalk("Island".to_string())),
        "landwalk should now be Islandwalk: {keywords:?}"
    );
    assert!(
        !keywords.contains(&Keyword::Landwalk("Mountain".to_string())),
        "Mountainwalk must be gone: {keywords:?}"
    );
}

/// CR 612.2 category isolation (plan 4 NEGATIVE): a creature-type text change
/// must NOT touch a basic-land-type carrier. Artificial Evolution (creature
/// type) on a Zombie with Mountainwalk changes Zombie → Elf (positive reach
/// guard) but leaves the Mountain landwalk untouched.
#[test]
fn creature_type_change_does_not_touch_basic_land_landwalk() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Zombie Strider", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .with_keyword(Keyword::Landwalk("Mountain".to_string()))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Zombie".to_string(), "Elf".to_string()];
    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Zombie".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );
    let obj = &runner.state().objects[&creature];
    // Positive reach guard: the creature-type change DID apply.
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Elf"),
        "Zombie should have become Elf: {:?}",
        obj.card_types.subtypes
    );
    // The basic-land-type landwalk is a DIFFERENT category — untouched.
    assert!(
        obj.keywords
            .contains(&Keyword::Landwalk("Mountain".to_string())),
        "a creature-type change must not touch Mountainwalk: {:?}",
        obj.keywords
    );
}

/// CR 613.7 (plan 9): two sequential text changes on one permanent compose by
/// timestamp order — black → blue, then blue → red, yields red. Revert guard: if
/// each TCE's operands were not latched per-effect (or the timestamp order were
/// reversed), the final protection would read blue.
#[test]
fn sequential_text_changes_compose_by_timestamp() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Onyx Sentinel", 2, 2)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Black,
        )))
        .id();
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();
    let mut runner = scenario.build();

    runner.cast(first).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Black),
        TextWord::Color(ManaColor::Blue),
    );
    // The second change reads the now-blue live word (proving per-TCE operands).
    runner.cast(second).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Blue),
        TextWord::Color(ManaColor::Red),
    );

    let keywords = &runner.state().objects[&creature].keywords;
    assert!(
        keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Red
        ))),
        "final protection must be red (CR 613.7 timestamp order): {keywords:?}"
    );
    assert!(
        !keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Blue
        ))),
        "the intermediate blue must not survive the second change: {keywords:?}"
    );
}

/// CR 608.2c (plan 10): Crystal Spray's trailing "Draw a card" continuation
/// resolves after the replacement choice, and control returns to Priority.
/// Revert guard: if the choice handler dropped the parked continuation, the
/// hand-size delta would be zero and/or the game would remain stuck off Priority.
#[test]
fn text_change_continuation_draws_and_returns_to_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Ruby Sentinel", 2, 2)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Crystal Spray", true, CRYSTAL_SPRAY)
        .id();
    // A non-empty library so the trailing "Draw a card" succeeds (drawing from an
    // empty library would deck the caster out — CR 104.3c).
    scenario.with_library_top(P0, &["Plains", "Plains"]);
    let mut runner = scenario.build();

    runner.cast(spell).target_object(creature).resolve();
    let hand_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.hand.len())
        .expect("P0 exists");

    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Red),
        TextWord::Color(ManaColor::Blue),
    );

    let hand_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.hand.len())
        .expect("P0 exists");
    assert_eq!(
        hand_after,
        hand_before + 1,
        "Crystal Spray's 'Draw a card' continuation must draw exactly one card"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "control must return to Priority after the continuation: {:?}",
        runner.state().waiting_for
    );
}

/// Recursively collect every `Effect::ChangeTextWords` in an ability tree
/// (top-level, modal `ChooseOneOf` branches, mode abilities, sub/else chains).
fn collect_change_text<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
    if matches!(&*def.effect, Effect::ChangeTextWords { .. }) {
        out.push(&def.effect);
    }
    if let Effect::ChooseOneOf { branches, .. } = &*def.effect {
        for branch in branches {
            collect_change_text(branch, out);
        }
    }
    if let Some(sub) = &def.sub_ability {
        collect_change_text(sub, out);
    }
    if let Some(els) = &def.else_ability {
        collect_change_text(els, out);
    }
    for mode in &def.mode_abilities {
        collect_change_text(mode, out);
    }
}

/// Parse `oracle` and return each `ChangeTextWords`'s
/// `(allowed_categories, excluded_to, duration)`.
#[allow(clippy::type_complexity)]
fn change_text_snapshots(
    name: &str,
    oracle: &str,
) -> Vec<(Vec<TextWordCategory>, Vec<TextWord>, Option<Duration>)> {
    let parsed = parse_oracle_text(oracle, name, &[], &["Instant".to_string()], &[]);
    let mut effects = Vec::new();
    for def in &parsed.abilities {
        collect_change_text(def, &mut effects);
    }
    effects
        .into_iter()
        .map(|e| match e {
            Effect::ChangeTextWords {
                allowed_categories,
                excluded_to,
                duration,
                ..
            } => (
                allowed_categories.clone(),
                excluded_to.clone(),
                duration.clone(),
            ),
            _ => unreachable!("filtered to ChangeTextWords above"),
        })
        .collect()
}

/// CR 612.1 + CR 612.2 (plan 11): every card in the text-changing class lowers to
/// `Effect::ChangeTextWords` with the correct `allowed_categories`, `excluded_to`,
/// and `duration`. Parser snapshot (shape) test — the runtime semantics are
/// covered by the cast-pipeline tests above; this pins the lowering surface.
#[test]
fn parser_snapshots_for_text_changing_class() {
    use TextWordCategory::{BasicLandType as BLand, ColorWord, CreatureType};

    // Single-category, indefinite.
    assert_eq!(
        change_text_snapshots("Sleight of Mind", SLEIGHT_OF_MIND),
        vec![(vec![ColorWord], vec![], None)]
    );
    assert_eq!(
        change_text_snapshots(
            "Glamerdye",
            "Change the text of target spell or permanent by replacing all \
             instances of one color word with another."
        ),
        vec![(vec![ColorWord], vec![], None)]
    );
    assert_eq!(
        change_text_snapshots(
            "Alter Reality",
            "Change the text of target spell or permanent by replacing all \
             instances of one color word with another."
        ),
        vec![(vec![ColorWord], vec![], None)]
    );
    assert_eq!(
        change_text_snapshots("Magical Hack", MAGICAL_HACK),
        vec![(vec![BLand], vec![], None)]
    );

    // Two-category, indefinite.
    assert_eq!(
        change_text_snapshots(
            "Mind Bend",
            "Change the text of target permanent by replacing all instances of \
             one color word with another or one basic land type with another."
        ),
        vec![(vec![ColorWord, BLand], vec![], None)]
    );

    // Two-category, until end of turn.
    assert_eq!(
        change_text_snapshots("Crystal Spray", CRYSTAL_SPRAY),
        vec![(
            vec![ColorWord, BLand],
            vec![],
            Some(Duration::UntilEndOfTurn)
        )]
    );
    assert_eq!(
        change_text_snapshots(
            "Trait Doctoring",
            "Change the text of target permanent by replacing all instances of \
             one color word with another or one basic land type with another \
             until end of turn."
        ),
        vec![(
            vec![ColorWord, BLand],
            vec![],
            Some(Duration::UntilEndOfTurn)
        )]
    );
    assert_eq!(
        change_text_snapshots(
            "Whim of Volrath",
            "Change the text of target permanent by replacing all instances of \
             one color word with another or one basic land type with another \
             until end of turn."
        ),
        vec![(
            vec![ColorWord, BLand],
            vec![],
            Some(Duration::UntilEndOfTurn)
        )]
    );

    // Creature type with the Wall exclusion (Task-1 continuation absorber).
    assert_eq!(
        change_text_snapshots("Artificial Evolution", ARTIFICIAL_EVOLUTION_FULL),
        vec![(
            vec![CreatureType],
            vec![TextWord::CreatureType("Wall".to_string())],
            None
        )]
    );

    // Modal: each mode lowers to a single-category ChangeTextWords.
    let spectral = change_text_snapshots(
        "Spectral Shift",
        "Choose one —\n\
         • Change the text of target spell or permanent by replacing all \
         instances of one basic land type with another.\n\
         • Change the text of target spell or permanent by replacing all \
         instances of one color word with another.",
    );
    assert_eq!(
        spectral.len(),
        2,
        "Spectral Shift must lower to two ChangeTextWords modes: {spectral:?}"
    );
    for (cats, excluded, dur) in &spectral {
        assert_eq!(
            cats.len(),
            1,
            "each mode is a single-category change: {cats:?}"
        );
        assert!(excluded.is_empty(), "no exclusion on Spectral Shift modes");
        assert_eq!(*dur, None, "Spectral Shift modes are indefinite");
    }
    let mode_cats: std::collections::BTreeSet<TextWordCategory> = spectral
        .iter()
        .flat_map(|(c, _, _)| c.iter().copied())
        .collect();
    assert_eq!(
        mode_cats,
        [BLand, ColorWord].into_iter().collect(),
        "Spectral Shift's modes cover the basic-land and color-word categories"
    );
}

/// CR 612.1 (plan 12): the interactive `WaitingFor`/`GameAction` payloads and the
/// `Effect::ChangeTextWords` with a non-empty `excluded_to` round-trip through
/// serde (guards the `skip_serializing_if = "Vec::is_empty"` on `excluded_to`).
#[test]
fn serde_round_trip_text_word_types() {
    let wf = WaitingFor::TextWordReplacement {
        player: P0,
        source: ObjectId(11),
        target: ObjectId(22),
        options: vec![TextWordReplacementOption {
            category: TextWordCategory::ColorWord,
            from: TextWord::Color(ManaColor::Red),
            to: TextWord::Color(ManaColor::Blue),
            label: "Red → Blue".to_string(),
        }],
        duration: Some(Duration::UntilEndOfTurn),
    };
    let json = serde_json::to_string(&wf).expect("serialize WaitingFor");
    let back: WaitingFor = serde_json::from_str(&json).expect("deserialize WaitingFor");
    assert_eq!(wf, back);

    let action = GameAction::ChooseTextWordReplacement { index: 3 };
    let json = serde_json::to_string(&action).expect("serialize GameAction");
    let back: GameAction = serde_json::from_str(&json).expect("deserialize GameAction");
    assert_eq!(action, back);

    // Non-empty excluded_to must survive the round trip despite skip-if-empty.
    let effect = Effect::ChangeTextWords {
        target: engine::types::ability::TargetFilter::Any,
        allowed_categories: vec![TextWordCategory::CreatureType],
        excluded_to: vec![TextWord::CreatureType("Wall".to_string())],
        duration: None,
    };
    let json = serde_json::to_string(&effect).expect("serialize Effect");
    let back: Effect = serde_json::from_str(&json).expect("deserialize Effect");
    assert_eq!(effect, back);
}
