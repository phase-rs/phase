//! CR 612: Text-changing effects — word replacement. Runtime regressions driving
//! the real cast pipeline (parse → cast → resolve → `WaitingFor::TextWordReplacement`
//! → `GameAction::ChooseTextWordReplacement` → Layer-3 continuous effect).

use engine::game::combat::AttackTarget;
use engine::game::layers::{flush_layers, prune_end_of_turn_effects};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::text_substitution::collect_present_words;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, BasicLandType, ContinuousModification, Duration, Effect,
    StaticCondition, StaticDefinition, TargetFilter, TextWord, TextWordCategory, TriggerCondition,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, TextWordReplacementOption, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

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

/// Recursively collect every `TypeFilter::Subtype` string reachable from a filter.
/// Shared by the cost / condition regression tests below.
fn filter_subtypes(filter: &engine::types::ability::TargetFilter, out: &mut Vec<String>) {
    use engine::types::ability::TargetFilter;
    match filter {
        TargetFilter::Typed(typed) => {
            for tf in &typed.type_filters {
                type_filter_subtypes(tf, out);
            }
        }
        TargetFilter::Not { filter } => filter_subtypes(filter, out),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for f in filters {
                filter_subtypes(f, out);
            }
        }
        _ => {}
    }
}

fn type_filter_subtypes(tf: &engine::types::ability::TypeFilter, out: &mut Vec<String>) {
    use engine::types::ability::TypeFilter;
    match tf {
        TypeFilter::Subtype(s) => out.push(s.clone()),
        TypeFilter::Non(inner) => type_filter_subtypes(inner, out),
        TypeFilter::AnyOf(inner) => {
            for f in inner {
                type_filter_subtypes(f, out);
            }
        }
        _ => {}
    }
}

/// Build a `TargetFilter` naming a single creature subtype (a buried creature-type
/// carrier). Used by the HIGH-carrier regression tests below.
fn creature_subtype_filter(subtype: &str) -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Subtype(subtype.to_string())],
        controller: None,
        properties: vec![],
    })
}

/// CR 702.48a + CR 612.2 (HIGH carrier #1): the creature type spelled by an
/// `Offering` keyword ("Fox offering") is text-changed. The buried "Fox" is the
/// object's SOLE creature-type word, so pre-fix `collect_present_words` is empty.
///
/// Revert guard: with the `Keyword::Offering` subtype-cursor arm dropped, Fox is
/// neither collected (the `before` reach-guard flips) nor offered as a `from`
/// word (no `WaitingFor::TextWordReplacement` is raised, so `apply_replacement`
/// panics). The final assertion — the live keyword now reads `Offering("Elf")` —
/// additionally fails if only the collect side were wired.
#[test]
fn creature_type_in_offering_keyword_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Offering Patron", 3, 3)
        .with_keyword(Keyword::Offering("Fox".to_string()))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types =
        vec!["Fox".to_string(), "Elf".to_string(), "Wall".to_string()];

    // Reach-guard: the only creature-type word is inside the Offering keyword.
    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Fox".to_string())),
        "the Offering keyword's creature type should be collected: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Fox".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let obj = &runner.state().objects[&creature];
    assert!(
        obj.keywords.contains(&Keyword::Offering("Elf".to_string())),
        "Offering should now name Elf: {:?}",
        obj.keywords
    );
    assert!(
        !obj.keywords.contains(&Keyword::Offering("Fox".to_string())),
        "Offering Fox must be gone: {:?}",
        obj.keywords
    );
}

/// CR 613.1f + CR 612.1 (HIGH carrier #2): a creature type buried in a granted
/// `AddStaticMode`'s inner filter ("can't be blocked by Goblins") is text-changed.
/// The buried "Goblin" is the object's SOLE creature-type word.
///
/// Revert guard: with the `ContinuousModification::AddStaticMode` recursion
/// dropped, Goblin is neither collected (the `before` reach-guard flips) nor
/// offered, so `apply_replacement` panics. The final structural dig (the granted
/// mode's filter now names Elf, not Goblin) additionally fails if only the collect
/// side were wired.
#[test]
fn creature_type_in_granted_add_static_mode_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let static_def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantBeBlockedBy {
                filter: creature_subtype_filter("Goblin"),
            },
        }]);
    let creature = scenario
        .add_creature(P0, "Mode Grantor", 3, 3)
        .with_static_definition(static_def)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types =
        vec!["Goblin".to_string(), "Elf".to_string(), "Wall".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the granted AddStaticMode's filter should be collected: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    // Structural dig into the live granted mode's inner filter.
    let mut subs = Vec::new();
    for sd in runner.state().objects[&creature]
        .static_definitions
        .iter_unchecked()
    {
        for m in &sd.modifications {
            if let ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBeBlockedBy { filter },
            } = m
            {
                filter_subtypes(filter, &mut subs);
            }
        }
    }
    assert!(
        subs.iter().any(|s| s == "Elf"),
        "granted mode filter should now name Elf: {subs:?}"
    );
    assert!(
        !subs.iter().any(|s| s == "Goblin"),
        "granted mode filter must no longer name Goblin: {subs:?}"
    );
}

/// CR 614.1 + CR 612.1 (HIGH carrier #3): a creature type buried in an
/// `Effect::AddTargetReplacement`'s installed replacement (its `valid_card` event
/// filter — "the next Goblin you cast gains …") is text-changed. `AddTargetReplacement`
/// returns `None` from `target_filter_mut`, so only the dedicated walker arm reaches
/// it. The buried "Goblin" is the object's SOLE creature-type word.
///
/// Revert guard: with the `Effect::AddTargetReplacement` arm dropped, Goblin is
/// neither collected (the `before` reach-guard flips) nor offered, so
/// `apply_replacement` panics. The final dig (the installed replacement's
/// `valid_card` now names Elf) additionally fails if only the collect side were wired.
#[test]
fn creature_type_in_add_target_replacement_is_replaced() {
    use engine::types::replacements::ReplacementEvent;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let replacement =
        engine::types::ability::ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .valid_card(creature_subtype_filter("Goblin"));
    let ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::AddTargetReplacement {
            replacement: Box::new(replacement),
            target: TargetFilter::Any,
        },
    );
    let creature = scenario
        .add_creature(P0, "Replacement Installer", 3, 3)
        .with_ability_definition(ability)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types =
        vec!["Goblin".to_string(), "Elf".to_string(), "Wall".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the AddTargetReplacement's valid_card should be collected: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let mut subs = Vec::new();
    for ability in runner.state().objects[&creature].abilities.iter() {
        if let Effect::AddTargetReplacement { replacement, .. } = ability.effect.as_ref() {
            if let Some(vc) = &replacement.valid_card {
                filter_subtypes(vc, &mut subs);
            }
        }
    }
    assert!(
        subs.iter().any(|s| s == "Elf"),
        "installed replacement valid_card should now name Elf: {subs:?}"
    );
    assert!(
        !subs.iter().any(|s| s == "Goblin"),
        "installed replacement valid_card must no longer name Goblin: {subs:?}"
    );
}

/// CR 105 + CR 612.2 (HIGH carrier #4): the color word in a
/// `StaticCondition::ChosenColorIs { color }` gate ("as long as the chosen color
/// is red") is text-changed. The buried red is the object's SOLE color word.
///
/// Revert guard: with the `StaticCondition::ChosenColorIs` color-cursor arm
/// dropped, red is neither collected (the `before` reach-guard flips) nor offered,
/// so `apply_replacement` panics. The final dig (the static's condition now reads
/// blue) additionally fails if only the collect side were wired.
#[test]
fn color_word_in_chosen_color_is_condition_is_replaced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let static_def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .condition(StaticCondition::ChosenColorIs {
            color: ManaColor::Red,
        });
    let creature = scenario
        .add_creature(P0, "Chosen Color Gate", 3, 3)
        .with_static_definition(static_def)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sleight of Mind", true, SLEIGHT_OF_MIND)
        .id();
    let mut runner = scenario.build();

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::ColorWord,
    );
    assert!(
        before.contains(&TextWord::Color(ManaColor::Red)),
        "the ChosenColorIs condition's color should be collected: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::Color(ManaColor::Red),
        TextWord::Color(ManaColor::Blue),
    );

    let has_blue = runner.state().objects[&creature]
        .static_definitions
        .iter_unchecked()
        .any(|sd| {
            matches!(
                sd.condition,
                Some(StaticCondition::ChosenColorIs {
                    color: ManaColor::Blue
                })
            )
        });
    let has_red = runner.state().objects[&creature]
        .static_definitions
        .iter_unchecked()
        .any(|sd| {
            matches!(
                sd.condition,
                Some(StaticCondition::ChosenColorIs {
                    color: ManaColor::Red
                })
            )
        });
    assert!(has_blue, "ChosenColorIs should now gate on blue");
    assert!(!has_red, "ChosenColorIs must no longer gate on red");
}

/// CR 612.1 + CR 612.2 + CR 701.21 (maintainer blocker): a creature type that
/// lives ONLY inside an activated ability's *cost* ("Sacrifice a Goblin", Goblin
/// Chirurgeon) is text-changed. This is the carrier the walker previously skipped:
/// `walk_ability_definition` walked `effect`/`sub`/`else`/`modes`/`repeat_for` but
/// never descended into `AbilityDefinition.cost`, so the cost silently stayed
/// "Goblin" after Artificial Evolution changed Goblin → Elf.
///
/// Revert guard: without the new `walk_ability_cost` recursion the creature carries
/// no other creature-type word, so `collect_present_words` returns empty. The
/// positive reach-guard (`before` contains Goblin) then fails, and — because no
/// creature-type word is present — the cast raises no `WaitingFor::TextWordReplacement`,
/// so `apply_replacement` panics. The final assertion (the cost's sacrifice filter
/// now names Elf, not Goblin) additionally fails if only the collect side were wired.
#[test]
fn creature_type_in_activation_cost_is_replaced() {
    use engine::game::game_object::GameObject;
    use engine::types::ability::AbilityCost;

    /// Subtypes named by any (possibly composite) sacrifice cost on the object.
    fn sacrifice_cost_subtypes(obj: &GameObject) -> Vec<String> {
        fn collect(cost: &AbilityCost, out: &mut Vec<String>) {
            match cost {
                AbilityCost::Sacrifice(sac) => filter_subtypes(&sac.target, out),
                AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
                    for c in costs {
                        collect(c, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for ability in obj.abilities.iter() {
            if let Some(cost) = &ability.cost {
                collect(cost, &mut out);
            }
        }
        out
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Verbatim Oracle text; the ONLY creature-type word is inside the cost.
    let chirurgeon = scenario
        .add_creature(P0, "Goblin Chirurgeon", 1, 1)
        .from_oracle_text("{0}, Sacrifice a Goblin: Regenerate target creature.")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    // Sanity: the cost really parsed to a Goblin sacrifice filter.
    let cost_before = sacrifice_cost_subtypes(&runner.state().objects[&chirurgeon]);
    assert!(
        cost_before.iter().any(|s| s == "Goblin"),
        "the activation cost should sacrifice a Goblin: {cost_before:?}"
    );

    // Positive reach-guard: the walker sees Goblin ONLY by descending into the cost.
    let before = collect_present_words(
        &runner.state().objects[&chirurgeon],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the activation cost's sacrifice filter should carry the Goblin word: {before:?}"
    );

    runner.cast(spell).target_object(chirurgeon).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    // Revert-failing assertion: the cost now requires sacrificing an Elf, not a Goblin.
    let cost_after = sacrifice_cost_subtypes(&runner.state().objects[&chirurgeon]);
    assert!(
        cost_after.iter().any(|s| s == "Elf"),
        "the activation cost must now require sacrificing an Elf: {cost_after:?}"
    );
    assert!(
        !cost_after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the sacrifice cost: {cost_after:?}"
    );
}

/// CR 612.1 + CR 612.2 + CR 608.2c (maintainer blocker): a creature type that
/// lives ONLY inside an ability's resolution *condition* ("If this creature is a
/// Goblin, …") is text-changed. Exercises the brand-new `walk_ability_condition`
/// via the `AbilityDefinition.condition` root (`SourceMatchesFilter`).
///
/// The effect's own type words (Kithkin / Soldier) are filtered out of the offered
/// `from` set by the live creature-type intersection (`all_creature_types =
/// [Goblin, Elf]`), so the only legal `from` is the Goblin buried in the condition.
/// Revert guard: without `walk_ability_condition`, Goblin is neither collected nor
/// rewritten — `collect_present_words` (post-intersection) is empty, no
/// `WaitingFor::TextWordReplacement` is raised, and `apply_replacement` panics; the
/// final condition-filter assertion also fails.
#[test]
fn creature_type_in_ability_condition_is_replaced() {
    use engine::game::game_object::GameObject;
    use engine::types::ability::AbilityCondition;

    /// Subtypes named by any `SourceMatchesFilter` resolution condition on the object.
    fn condition_subtypes(obj: &GameObject) -> Vec<String> {
        let mut out = Vec::new();
        for ability in obj.abilities.iter() {
            if let Some(AbilityCondition::SourceMatchesFilter { filter }) = &ability.condition {
                filter_subtypes(filter, &mut out);
            }
        }
        out
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Proven parse: "If this creature is a [type], …" → `SourceMatchesFilter`.
    let figure = scenario
        .add_creature(P0, "Figure of Fable", 1, 1)
        .from_oracle_text(
            "If this creature is a Goblin, it becomes a Kithkin Soldier \
             with base power and toughness 4/5.",
        )
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    // Sanity: the condition really parsed to a Goblin source gate.
    let cond_before = condition_subtypes(&runner.state().objects[&figure]);
    assert!(
        cond_before.iter().any(|s| s == "Goblin"),
        "the resolution condition should gate on a Goblin: {cond_before:?}"
    );

    // Positive reach-guard: the walker sees Goblin only by descending into the
    // condition (Kithkin/Soldier are excluded by the live creature-type set).
    let before = collect_present_words(
        &runner.state().objects[&figure],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the condition's filter should carry the Goblin word: {before:?}"
    );

    runner.cast(spell).target_object(figure).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    // Revert-failing assertion: the condition now gates on Elf, not Goblin.
    let cond_after = condition_subtypes(&runner.state().objects[&figure]);
    assert!(
        cond_after.iter().any(|s| s == "Elf"),
        "the condition must now gate on an Elf: {cond_after:?}"
    );
    assert!(
        !cond_after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the condition: {cond_after:?}"
    );
}

/// CR 612.1 + CR 612.2 + CR 603.4 (maintainer blocker 1): a creature type that
/// lives ONLY inside a trigger's intervening-if `TriggerCondition::ControlsType`
/// ("Whenever this creature attacks, if you control a Goblin, ...") is
/// text-changed, and the change alters real TRIGGER FIRING. After Artificial
/// Evolution rewrites Goblin → Elf, the intervening-if reads "if you control an
/// Elf"; with an Elf (and no Goblin) in play the trigger now fires and its
/// `you gain 1 life` effect resolves.
///
/// This drives the full pipeline: parse → cast → resolve → text-word replacement
/// → Layer-3 continuous effect → declare attackers → trigger fires → resolve.
///
/// Revert guard (double): removing `walk_trigger_condition` means Goblin is never
/// collected from the trigger condition, so no `WaitingFor::TextWordReplacement`
/// offers Goblin → Elf and `apply_replacement` panics. Even if the collect side
/// still saw it, the live condition would stay `ControlsType { Goblin }`; with
/// only an Elf in play the intervening-if is false, the trigger never fires, and
/// the `+1 life` assertion fails.
#[test]
fn creature_type_in_trigger_intervening_if_changes_firing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The ONLY creature-type word on the scout is the Goblin buried in its
    // trigger's intervening-if condition (no subtype on its type line, and the
    // effect / event text carry no creature type).
    let scout = scenario
        .add_creature(P0, "Warband Scout", 2, 2)
        .from_oracle_text(
            "Whenever this creature attacks, if you control a Goblin, you gain 1 life.",
        )
        .id();
    // A vanilla Elf that satisfies the post-change "control an Elf" gate. It is
    // NOT a Goblin, so the pre-change gate would read false.
    let _elf = scenario
        .add_creature(P0, "Elvish Fodder", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    // Sanity: the intervening-if really parsed to a Goblin control gate.
    let cond = runner.state().objects[&scout]
        .trigger_definitions
        .iter_unchecked()
        .find_map(|t| t.condition.clone());
    assert!(
        matches!(&cond, Some(TriggerCondition::ControlsType { .. })),
        "expected a ControlsType intervening-if on the attack trigger, got {cond:?}"
    );

    // Change Goblin → Elf on the scout; the intervening-if now reads "control an Elf".
    runner.cast(spell).target_object(scout).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let life_before = runner.life(P0);
    // Declare the scout attacking — the event the intervening-if trigger watches.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(scout, AttackTarget::Player(P1))])
        .expect("declare attackers should succeed");
    runner.advance_until_stack_empty();

    // Revert-failing assertion: the trigger fired because it now reads "control an
    // Elf" (an Elf is in play). Pre-fix the gate stays Goblin, no Goblin is in
    // play, the trigger never fires, and life is unchanged.
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "the intervening-if trigger must fire and gain 1 life once it reads 'control an Elf'"
    );
}

/// CR 612.1 + CR 612.2 + CR 701.21 (maintainer blocker 2): after Artificial
/// Evolution rewrites Goblin → Elf on Goblin Chirurgeon, the "Sacrifice a Goblin"
/// activation cost is exercised through the REAL activation + cost-payment
/// pipeline: an Elf is offered as a legal sacrifice and a Goblin is not.
///
/// Reach guard: the activation reaches `WaitingFor::PayCost { kind: Sacrifice }`,
/// proving the activated ability is available and its sacrifice cost is being
/// paid (not a vacuous inspection). Revert-failing assertion: with the cost
/// text-change in place the eligible `choices` contain the Elf and exclude the
/// Goblin; reverting the cost walk leaves the cost naming Goblin, so `choices`
/// would contain the Goblin and exclude the Elf — flipping both assertions.
#[test]
fn creature_type_in_activation_cost_drives_real_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chirurgeon = scenario
        .add_creature(P0, "Goblin Chirurgeon", 1, 1)
        .from_oracle_text("{0}, Sacrifice a Goblin: Regenerate target creature.")
        .id();
    // Sacrifice fodder of each type. After the change the cost sacrifices an Elf.
    let elf = scenario
        .add_creature(P0, "Elvish Fodder", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let goblin = scenario
        .add_creature(P0, "Goblin Fodder", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    // Change Goblin → Elf on the Chirurgeon's activation cost.
    runner.cast(spell).target_object(chirurgeon).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    // Locate the activated ability whose (possibly composite) cost includes a
    // sacrifice — Goblin Chirurgeon's cost is `Composite { Mana{0}, Sacrifice }`.
    fn cost_has_sacrifice(cost: &engine::types::ability::AbilityCost) -> bool {
        use engine::types::ability::AbilityCost;
        match cost {
            AbilityCost::Sacrifice(_) => true,
            AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
                costs.iter().any(cost_has_sacrifice)
            }
            _ => false,
        }
    }
    let ability_index = runner.state().objects[&chirurgeon]
        .abilities
        .iter()
        .position(|a| a.cost.as_ref().is_some_and(cost_has_sacrifice))
        .expect("Goblin Chirurgeon must have a sacrifice-cost activated ability");

    // Announce the activation and drive it to the sacrifice-cost payment window,
    // targeting the Elf's regeneration (any creature is a legal target).
    runner
        .act(GameAction::ActivateAbility {
            source_id: chirurgeon,
            ability_index,
        })
        .expect("ActivateAbility must be accepted");

    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(chirurgeon)),
                    })
                    .expect("ChooseTarget (regenerate target) must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing the {0} mana cost must be accepted");
            }
            WaitingFor::PayCost { .. } => break,
            other => panic!("unexpected waiting state before PayCost: {other:?}"),
        }
    }

    // Reach guard + revert-failing assertions: we are paying a Sacrifice cost, and
    // the eligible set now contains the Elf and excludes the Goblin.
    match &runner.state().waiting_for {
        WaitingFor::PayCost { kind, choices, .. } => {
            assert!(
                matches!(kind, PayCostKind::Sacrifice),
                "expected a Sacrifice cost payment, got {kind:?}"
            );
            assert!(
                choices.contains(&elf),
                "an Elf must be a legal sacrifice after Goblin → Elf: {choices:?}"
            );
            assert!(
                !choices.contains(&goblin),
                "a Goblin must NOT be a legal sacrifice after Goblin → Elf: {choices:?}"
            );
        }
        other => panic!("activation did not reach the sacrifice PayCost window: {other:?}"),
    }

    // Complete the payment with the Elf to prove the pipeline accepts it.
    runner
        .act(GameAction::SelectCards { cards: vec![elf] })
        .expect("sacrificing the Elf must be accepted");
    assert_eq!(
        runner.state().objects.get(&elf).map(|o| o.zone),
        Some(engine::types::zones::Zone::Graveyard),
        "the sacrificed Elf must move to the graveyard"
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

/// CR 612.1 + CR 508.1 (review finding 1): a creature type that lives ONLY inside
/// a trigger's `TriggerCondition::AttackersDeclaredCount` subject filter ("if two
/// or more Pirates attacked this combat") is text-changed. The walker previously
/// classified `AttackersDeclaredCount` as a no-op, so the `Option<TargetFilter>`
/// carried by BOTH subject axes was neither collected nor rewritten.
///
/// Revert guard: with `AttackersDeclaredCount` back in the no-op arm, Pirate is
/// never collected — no `WaitingFor::TextWordReplacement` offers Pirate → Elf and
/// `apply_replacement` panics. The final assertion (the subject filter now names
/// Elf) additionally fails if only the collect side were wired.
#[test]
fn creature_type_in_attackers_declared_count_is_replaced() {
    use engine::game::game_object::GameObject;
    use engine::types::ability::{
        AttackersDeclaredCountSubject, Comparator, ControllerRef, TargetFilter, TriggerDefinition,
        TypedFilter,
    };
    use engine::types::triggers::TriggerMode;

    fn subject_subtypes(obj: &GameObject) -> Vec<String> {
        let mut out = Vec::new();
        for t in obj.trigger_definitions.iter_unchecked() {
            if let Some(TriggerCondition::AttackersDeclaredCount { subject, .. }) = &t.condition {
                let filter = match subject {
                    AttackersDeclaredCountSubject::Controller { filter, .. }
                    | AttackersDeclaredCountSubject::AttackTarget { filter, .. } => filter,
                };
                if let Some(f) = filter {
                    filter_subtypes(f, &mut out);
                }
            }
        }
        out
    }

    let pirate_filter = TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .subtype("Pirate".to_string()),
    );
    let mut trigger = TriggerDefinition::new(TriggerMode::YouAttack);
    trigger.condition = Some(TriggerCondition::AttackersDeclaredCount {
        subject: AttackersDeclaredCountSubject::Controller {
            scope: ControllerRef::You,
            filter: Some(pirate_filter),
        },
        comparator: Comparator::GE,
        count: 2,
    });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warden = scenario
        .add_creature(P0, "Pirate Warden", 2, 2)
        .with_trigger_definition(trigger)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Pirate".to_string(), "Elf".to_string()];

    // Positive reach-guard: the walker sees Pirate ONLY by descending into the
    // AttackersDeclaredCount subject filter.
    let before = collect_present_words(
        &runner.state().objects[&warden],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Pirate".to_string())),
        "the AttackersDeclaredCount subject filter should carry Pirate: {before:?}"
    );

    runner.cast(spell).target_object(warden).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Pirate".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let after = subject_subtypes(&runner.state().objects[&warden]);
    assert!(
        after.iter().any(|s| s == "Elf"),
        "the subject filter must now name Elf: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Pirate"),
        "Pirate must be gone from the subject filter: {after:?}"
    );
}

/// CR 612.1 + CR 614.1d (review finding 2): a creature type that lives ONLY inside
/// a replacement effect's applicability condition ("if you control a Goblin",
/// `ReplacementCondition::IfControlsMatching`) is text-changed. The entire
/// `replacement_definitions` root (Root 6) was previously unwalked.
///
/// Revert guard: without Root 6 / `walk_replacement_condition`, Goblin is never
/// collected — no replacement choice offers Goblin → Elf and `apply_replacement`
/// panics; the live condition also stays Goblin.
#[test]
fn creature_type_in_replacement_condition_is_replaced() {
    use engine::game::game_object::GameObject;
    use engine::types::ability::{
        ControllerRef, ReplacementCondition, ReplacementDefinition, TargetFilter, TypedFilter,
    };
    use engine::types::replacements::ReplacementEvent;

    fn condition_subtypes(obj: &GameObject) -> Vec<String> {
        let mut out = Vec::new();
        for r in obj.replacement_definitions.iter_unchecked() {
            if let Some(ReplacementCondition::IfControlsMatching { filter, .. }) = &r.condition {
                filter_subtypes(filter, &mut out);
            }
        }
        out
    }

    let goblin_filter = TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .subtype("Goblin".to_string()),
    );
    let mut rep = ReplacementDefinition::new(ReplacementEvent::DamageDone);
    rep.condition = Some(ReplacementCondition::IfControlsMatching {
        minimum: 1,
        filter: goblin_filter,
    });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warden = scenario
        .add_creature(P0, "Goblin Warden", 2, 2)
        .with_replacement_definition(rep)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&warden],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the replacement condition should carry Goblin: {before:?}"
    );

    runner.cast(spell).target_object(warden).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let after = condition_subtypes(&runner.state().objects[&warden]);
    assert!(
        after.iter().any(|s| s == "Elf"),
        "the replacement condition must now name Elf: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the replacement condition: {after:?}"
    );
}

/// CR 612.1 + CR 614.1c (review finding 2, subtype-string arm): a BASIC LAND TYPE
/// inside a check-land-style replacement condition ("unless you control a Plains",
/// `ReplacementCondition::UnlessControlsSubtype`) is text-changed via Magical
/// Hack. Exercises the `subtypes: Vec<String>` cursor arm of
/// `walk_replacement_condition` (distinct from the `TargetFilter` arm above).
#[test]
fn basic_land_type_in_replacement_unless_controls_subtype_is_replaced() {
    use engine::game::game_object::GameObject;
    use engine::types::ability::{ReplacementCondition, ReplacementDefinition};
    use engine::types::replacements::ReplacementEvent;

    fn unless_subtypes(obj: &GameObject) -> Vec<String> {
        let mut out = Vec::new();
        for r in obj.replacement_definitions.iter_unchecked() {
            if let Some(ReplacementCondition::UnlessControlsSubtype { subtypes }) = &r.condition {
                out.extend(subtypes.iter().cloned());
            }
        }
        out
    }

    let mut rep = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
    rep.condition = Some(ReplacementCondition::UnlessControlsSubtype {
        subtypes: vec!["Plains".to_string()],
    });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let checkland = scenario
        .add_creature(P0, "Warden Retreat", 2, 2)
        .with_replacement_definition(rep)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Magical Hack", true, MAGICAL_HACK)
        .id();
    let mut runner = scenario.build();

    let before = collect_present_words(
        &runner.state().objects[&checkland],
        TextWordCategory::BasicLandType,
    );
    assert!(
        before.contains(&TextWord::BasicLandType(BasicLandType::Plains)),
        "the replacement condition should carry Plains: {before:?}"
    );

    runner.cast(spell).target_object(checkland).resolve();
    apply_replacement(
        &mut runner,
        TextWord::BasicLandType(BasicLandType::Plains),
        TextWord::BasicLandType(BasicLandType::Island),
    );

    let after = unless_subtypes(&runner.state().objects[&checkland]);
    assert!(
        after.iter().any(|s| s == "Island"),
        "the replacement condition must now name Island: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Plains"),
        "Plains must be gone from the replacement condition: {after:?}"
    );
}

/// CR 612.1 + CR 611.2b (review finding 3): a creature type that lives ONLY inside
/// an ability's `Duration::ForAsLongAs` condition ("for as long as you control a
/// Goblin") is text-changed. `Duration` was previously never walked from any root.
///
/// Revert guard: without `walk_duration` on `AbilityDefinition.duration`, Goblin
/// is never collected — no choice offers Goblin → Elf and `apply_replacement`
/// panics; the live duration condition also stays Goblin.
#[test]
fn creature_type_in_for_as_long_as_duration_is_replaced() {
    use engine::game::game_object::GameObject;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, StaticCondition, TargetFilter, TypedFilter,
    };

    fn duration_subtypes(obj: &GameObject) -> Vec<String> {
        let mut out = Vec::new();
        for a in obj.abilities.iter() {
            if let Some(Duration::ForAsLongAs {
                condition: StaticCondition::IsPresent { filter: Some(f) },
            }) = &a.duration
            {
                filter_subtypes(f, &mut out);
            }
        }
        out
    }

    let goblin_filter = TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .subtype("Goblin".to_string()),
    );
    let ability = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).duration(
        Duration::ForAsLongAs {
            condition: StaticCondition::IsPresent {
                filter: Some(goblin_filter),
            },
        },
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Duration Warden", 2, 2)
        .with_ability_definition(ability)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the ForAsLongAs duration should carry Goblin: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let after = duration_subtypes(&runner.state().objects[&creature]);
    assert!(
        after.iter().any(|s| s == "Elf"),
        "the duration condition must now name Elf: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the duration condition: {after:?}"
    );
}

/// CR 612.1 + CR 509.1b (convergence audit): a creature type inside a static
/// ability's MODE ("can't be blocked by Goblins", `StaticMode::CantBeBlockedBy`)
/// is text-changed. `StaticDefinition.mode` was previously unwalked — its
/// word-bearing evasion / protection / cost filters were silently skipped despite
/// a (now-corrected) doc claim that `StaticMode` carries no word.
///
/// Revert guard: without `walk_static_mode`, Goblin is never collected — no
/// choice offers Goblin → Elf and `apply_replacement` panics; the live mode
/// filter also stays Goblin.
#[test]
fn creature_type_in_static_mode_filter_is_replaced() {
    use engine::game::game_object::GameObject;
    use engine::types::ability::{TargetFilter, TypedFilter};
    use engine::types::statics::StaticMode;

    fn static_mode_subtypes(obj: &GameObject) -> Vec<String> {
        let mut out = Vec::new();
        for s in obj.static_definitions.iter_unchecked() {
            if let StaticMode::CantBeBlockedBy { filter } = &s.mode {
                filter_subtypes(filter, &mut out);
            }
        }
        out
    }

    let goblin_filter = TargetFilter::Typed(TypedFilter::creature().subtype("Goblin".to_string()));
    let static_def = engine::types::ability::StaticDefinition::new(StaticMode::CantBeBlockedBy {
        filter: goblin_filter,
    });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Evasive Warden", 2, 2)
        .with_static_definition(static_def)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the static mode filter should carry Goblin: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let after = static_mode_subtypes(&runner.state().objects[&creature]);
    assert!(
        after.iter().any(|s| s == "Elf"),
        "the static mode filter must now name Elf: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the static mode filter: {after:?}"
    );
}

/// CR 612.1 + CR 702.29 (convergence audit): a basic land type inside a
/// `Keyword::Typecycling` subtype parameter ("Plainscycling") is text-changed via
/// Magical Hack. `Typecycling`/`Splice`/`Champion`/`BandsWithOther` were
/// previously in the keyword no-op group despite their subtype-string parameter.
///
/// Revert guard: with `Typecycling` back in the no-op arm, Plains is never
/// collected — no choice offers Plains → Island and `apply_replacement` panics;
/// the live keyword also stays Plainscycling.
#[test]
fn basic_land_type_in_typecycling_keyword_is_replaced() {
    use engine::types::mana::ManaCost;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Cycling Warden", 2, 2)
        .with_keyword(Keyword::Typecycling {
            cost: ManaCost::default(),
            subtype: "Plains".to_string(),
        })
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Magical Hack", true, MAGICAL_HACK)
        .id();
    let mut runner = scenario.build();

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::BasicLandType,
    );
    assert!(
        before.contains(&TextWord::BasicLandType(BasicLandType::Plains)),
        "Plainscycling should carry the Plains basic-land-type word: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::BasicLandType(BasicLandType::Plains),
        TextWord::BasicLandType(BasicLandType::Island),
    );

    let keywords = &runner.state().objects[&creature].keywords;
    assert!(
        keywords.iter().any(|k| matches!(
            k,
            Keyword::Typecycling { subtype, .. } if subtype == "Island"
        )),
        "typecycling should now be Islandcycling: {keywords:?}"
    );
    assert!(
        !keywords.iter().any(|k| matches!(
            k,
            Keyword::Typecycling { subtype, .. } if subtype == "Plains"
        )),
        "Plainscycling must be gone: {keywords:?}"
    );
}

// ============================================================================
// walk_effect exhaustiveness: secondary word-bearing carriers on leaf effects.
// Each buries its target word as the SOLE word of its category, so pre-fix
// `collect_present_words` is empty → `apply_replacement` panics on revert
// (positive reach-guard), and the post-fix live-effect assertion flips too.
// ============================================================================

/// Pull the first ability effect matching `pred`, mapping it to a subtype list.
fn ability_effect_subtypes<F>(
    obj: &engine::game::game_object::GameObject,
    mut extract: F,
) -> Vec<String>
where
    F: FnMut(&Effect, &mut Vec<String>),
{
    let mut out = Vec::new();
    for a in obj.abilities.iter() {
        extract(&a.effect, &mut out);
    }
    out
}

/// CR 613.4 + CR 205.1a (walk_effect gap — `Effect::Animate.types`): the creature
/// type an animate effect grants ("becomes a Goblin") is text-changed. Buried in a
/// leaf effect's `types` `Vec<String>`, previously dropped by the no-op catch-all.
///
/// Revert guard: without the `Animate` walk arm, Goblin is never collected — no
/// choice offers Goblin → Elf and `apply_replacement` panics; the live `types`
/// vector also stays Goblin.
#[test]
fn creature_type_in_animate_effect_types_is_replaced() {
    use engine::types::ability::{AbilityDefinition, AbilityKind};

    let animate = Effect::Animate {
        power: None,
        toughness: None,
        types: vec!["Goblin".to_string()],
        remove_types: vec![],
        target: TargetFilter::Any,
        keywords: vec![],
    };
    let ability = AbilityDefinition::new(AbilityKind::Activated, animate);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Animate Warden", 2, 2)
        .with_ability_definition(ability)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the Animate effect should carry Goblin: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let after = ability_effect_subtypes(&runner.state().objects[&creature], |e, out| {
        if let Effect::Animate { types, .. } = e {
            out.extend(types.iter().cloned());
        }
    });
    assert!(
        after.iter().any(|s| s == "Elf"),
        "the Animate types must now name Elf: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the Animate types: {after:?}"
    );
}

/// CR 701.23a + CR 612.2 (walk_effect gap — `Effect::SearchLibrary.filter`): the
/// basic land type an in-effect tutor searches for ("search your library for a
/// Mountain card") is text-changed by Magical Hack. Buried in a leaf effect's
/// required `filter`, previously dropped by the no-op catch-all.
///
/// Revert guard: without the `SearchLibrary` walk arm, Mountain is never collected
/// — no choice offers Mountain → Island and `apply_replacement` panics; the live
/// search filter also stays Mountain.
#[test]
fn basic_land_type_in_search_library_filter_is_replaced() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, SearchSelectionConstraint,
    };

    let mountain_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![
            TypeFilter::Land,
            TypeFilter::Subtype("Mountain".to_string()),
        ],
        controller: None,
        properties: vec![],
    });
    let search = Effect::SearchLibrary {
        source_zones: vec![engine::types::zones::Zone::Library],
        filter: mountain_filter,
        count: QuantityExpr::Fixed { value: 1 },
        reveal: false,
        target_player: None,
        selection_constraint: SearchSelectionConstraint::None,
        split: None,
    };
    let ability = AbilityDefinition::new(AbilityKind::Activated, search);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Tutor Warden", 2, 2)
        .with_ability_definition(ability)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Magical Hack", true, MAGICAL_HACK)
        .id();
    let mut runner = scenario.build();

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::BasicLandType,
    );
    assert!(
        before.contains(&TextWord::BasicLandType(BasicLandType::Mountain)),
        "the SearchLibrary filter should carry Mountain: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::BasicLandType(BasicLandType::Mountain),
        TextWord::BasicLandType(BasicLandType::Island),
    );

    let after = ability_effect_subtypes(&runner.state().objects[&creature], |e, out| {
        if let Effect::SearchLibrary { filter, .. } = e {
            filter_subtypes(filter, out);
        }
    });
    assert!(
        after.iter().any(|s| s == "Island"),
        "the SearchLibrary filter must now name Island: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Mountain"),
        "Mountain must be gone from the SearchLibrary filter: {after:?}"
    );
}

/// CR 118.1 + CR 701.21 (walk_effect gap — `Effect::PayCost.cost`): a creature type
/// named in a resolution-time payment cost ("Sacrifice a Goblin" as an effect cost)
/// is text-changed. Buried in a leaf effect's `AbilityCost`, previously dropped by
/// the no-op catch-all.
///
/// Revert guard: without the `PayCost` walk arm, Goblin is never collected — no
/// choice offers Goblin → Elf and `apply_replacement` panics; the live cost filter
/// also stays Goblin.
#[test]
fn creature_type_in_paycost_effect_cost_is_replaced() {
    use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, SacrificeCost};

    let pay = Effect::PayCost {
        cost: AbilityCost::Sacrifice(SacrificeCost::count(creature_subtype_filter("Goblin"), 1)),
        scale: None,
        payer: TargetFilter::Controller,
    };
    let ability = AbilityDefinition::new(AbilityKind::Activated, pay);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Payment Warden", 2, 2)
        .with_ability_definition(ability)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the PayCost sacrifice cost should carry Goblin: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let after = ability_effect_subtypes(&runner.state().objects[&creature], |e, out| {
        if let Effect::PayCost {
            cost: AbilityCost::Sacrifice(sac),
            ..
        } = e
        {
            filter_subtypes(&sac.target, out);
        }
    });
    assert!(
        after.iter().any(|s| s == "Elf"),
        "the PayCost sacrifice filter must now name Elf: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the PayCost sacrifice filter: {after:?}"
    );
}

/// CR 701.47a + CR 612.2 (walk_effect gap — `Effect::Amass.subtype`): the literal
/// creature subtype an amass effect names ("Amass Goblins") is text-changed. Buried
/// in a leaf effect's `subtype` `String`, previously dropped by the no-op catch-all.
///
/// Revert guard: without the `Amass` walk arm, Goblin is never collected — no
/// choice offers Goblin → Elf and `apply_replacement` panics; the live `subtype`
/// string also stays Goblin.
#[test]
fn creature_type_in_amass_subtype_is_replaced() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};

    let amass = Effect::Amass {
        subtype: "Goblin".to_string(),
        count: QuantityExpr::Fixed { value: 1 },
    };
    let ability = AbilityDefinition::new(AbilityKind::Activated, amass);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Amass Warden", 2, 2)
        .with_ability_definition(ability)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Artificial Evolution", true, ARTIFICIAL_EVOLUTION)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Elf".to_string()];

    let before = collect_present_words(
        &runner.state().objects[&creature],
        TextWordCategory::CreatureType,
    );
    assert!(
        before.contains(&TextWord::CreatureType("Goblin".to_string())),
        "the Amass effect should carry Goblin: {before:?}"
    );

    runner.cast(spell).target_object(creature).resolve();
    apply_replacement(
        &mut runner,
        TextWord::CreatureType("Goblin".to_string()),
        TextWord::CreatureType("Elf".to_string()),
    );

    let after = ability_effect_subtypes(&runner.state().objects[&creature], |e, out| {
        if let Effect::Amass { subtype, .. } = e {
            out.push(subtype.clone());
        }
    });
    assert!(
        after.iter().any(|s| s == "Elf"),
        "the Amass subtype must now name Elf: {after:?}"
    );
    assert!(
        !after.iter().any(|s| s == "Goblin"),
        "Goblin must be gone from the Amass subtype: {after:?}"
    );
}
