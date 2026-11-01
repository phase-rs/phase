//! phase-rs/phase#10003 — "[an]other <noun>" inside a shared-quality reference
//! is relative to the compared object when that object is itself a member of the
//! reference population.
//!
//! Naming Screen (verbatim Oracle text, third paragraph is the subject):
//! "Each creature you control that doesn't share a name with any other creature
//! you control gets +1/+1."
//!
//! Pattern Matcher (the hostile sibling whose "another" must stay relative to
//! the ability SOURCE): "When this creature enters, you may search your library
//! for a card with the same name as another creature you control, reveal it, put
//! it into your hand, then shuffle." Ruling (2019-07-12): "If a second Pattern
//! Matcher enters the battlefield under your control, you can have it look at the
//! first Pattern Matcher to search for a third Pattern Matcher."
//!
//! Rules: CR 201.2a (same name), CR 109.2 (a zone-less description is a permanent
//! on the battlefield), CR 611.3a (a static ability's effect is re-evaluated),
//! CR 613.4c (layer 7c power/toughness modification).
//!
//! Every row drives the real pipeline: the Naming Screen rows read the effective
//! post-`evaluate_layers` power/toughness of the real parsed static; the Pattern
//! Matcher rows cast the creature, accept its optional ETB search through
//! `apply()`, and read the offered `WaitingFor::SearchChoice` candidates.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, SharedQuality, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const NAMING_SCREEN: &str = "As a creature token you control enters, name it using up to four letters. (It keeps its types. It just has a new name. Remember this is a family event.)\nWhen this enchantment enters, create four 1/1 colorless Hero tokens.\nEach creature you control that doesn't share a name with any other creature you control gets +1/+1.";

const PATTERN_MATCHER: &str = "When this creature enters, you may search your library for a card with the same name as another creature you control, reveal it, put it into your hand, then shuffle.";

/// Recompute layers and read an object's effective (post-layer) power/toughness.
fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

/// True when `filter` is a typed filter carrying a name `SharesQuality` property.
fn carries_name_shares_quality(filter: &TargetFilter) -> bool {
    let TargetFilter::Typed(typed) = filter else {
        return false;
    };
    typed.properties.iter().any(|prop| {
        matches!(
            prop,
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                ..
            }
        )
    })
}

/// Reach guard: Naming Screen's parsed static must be on the source, otherwise a
/// "no bonus" expectation would be vacuous.
fn assert_naming_screen_static(runner: &GameRunner, screen: ObjectId) {
    let statics = runner.state().objects[&screen]
        .static_definitions
        .as_slice();
    assert!(
        statics.iter().any(|sd| {
            sd.affected
                .as_ref()
                .is_some_and(carries_name_shares_quality)
        }),
        "Naming Screen must carry the shared-name static, statics={statics:?}"
    );
}

/// CR 201.2a + CR 613.4c: two creatures with different names each have no
/// other creature sharing their name, so both get +1/+1.
#[test]
fn n1_two_differently_named_creatures_both_get_the_bonus() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let screen = scenario
        .add_enchantment_from_oracle(P0, "Naming Screen", NAMING_SCREEN)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ogre = scenario.add_creature(P0, "Gray Ogre", 2, 2).id();
    let mut runner = scenario.build();
    assert_naming_screen_static(&runner, screen);

    assert_eq!(effective_pt(&mut runner, bears), (3, 3));
    assert_eq!(effective_pt(&mut runner, ogre), (3, 3));
}

/// CR 201.2a: two Grizzly Bears share a name, so neither gets the bonus; the
/// differently named Gray Ogre does (positive reach guard on the static).
#[test]
fn n2_same_named_creatures_get_no_bonus() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let screen = scenario
        .add_enchantment_from_oracle(P0, "Naming Screen", NAMING_SCREEN)
        .id();
    let bears_a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let bears_b = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ogre = scenario.add_creature(P0, "Gray Ogre", 2, 2).id();
    let mut runner = scenario.build();
    assert_naming_screen_static(&runner, screen);

    assert_eq!(
        effective_pt(&mut runner, ogre),
        (3, 3),
        "reach guard: the static applies to the uniquely named creature"
    );
    assert_eq!(effective_pt(&mut runner, bears_a), (2, 2));
    assert_eq!(effective_pt(&mut runner, bears_b), (2, 2));
}

/// A lone creature has no other creature to share a name with.
#[test]
fn n3_lone_creature_gets_the_bonus() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let screen = scenario
        .add_enchantment_from_oracle(P0, "Naming Screen", NAMING_SCREEN)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    assert_naming_screen_static(&runner, screen);

    assert_eq!(effective_pt(&mut runner, bears), (3, 3));
}

/// CR 109.5: "any other creature YOU control" — an opponent's same-named creature
/// is outside the reference population and does not cancel the bonus, and the
/// opponent's creature is outside the affected "creature you control".
#[test]
fn n4_opponents_same_named_creature_is_not_in_the_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let screen = scenario
        .add_enchantment_from_oracle(P0, "Naming Screen", NAMING_SCREEN)
        .id();
    let mine = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    assert_naming_screen_static(&runner, screen);

    assert_eq!(effective_pt(&mut runner, mine), (3, 3));
    assert_eq!(effective_pt(&mut runner, theirs), (2, 2));
}

/// CR 201.2a + CR 109.2: Naming Screen animated as an enchantment creature.
/// "Any other creature you control" excludes the COMPARED creature, not just the
/// source: a plain creature named "Naming Screen" shares its name with the
/// animated source (a creature you control other than itself) and gets no bonus,
/// while Grizzly Bears does. Excluding both the source and the compared object
/// would wrongly buff the plain "Naming Screen".
#[test]
fn n5_creature_source_is_in_the_reference_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The only anthem source: an Enchantment Creature that keeps both types.
    let screen = scenario
        .add_enchantment_from_oracle(P0, "Naming Screen", NAMING_SCREEN)
        .as_creature()
        .id();
    let plain = scenario.add_creature(P0, "Naming Screen", 1, 1).id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    {
        let source = runner
            .state_mut()
            .objects
            .get_mut(&screen)
            .expect("source exists");
        source.power = Some(4);
        source.toughness = Some(4);
        source.base_power = Some(4);
        source.base_toughness = Some(4);
    }

    assert_eq!(effective_pt(&mut runner, bears), (3, 3));
    // Reach guard: the source is a creature AND an enchantment and carries the
    // shared-name static, so it is a member of the reference population.
    let source = &runner.state().objects[&screen];
    assert!(source.card_types.core_types.contains(&CoreType::Creature));
    assert!(source
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    assert_naming_screen_static(&runner, screen);

    assert_eq!(
        effective_pt(&mut runner, screen),
        (4, 4),
        "the animated source shares its name with the plain 'Naming Screen'"
    );
    assert_eq!(
        effective_pt(&mut runner, plain),
        (1, 1),
        "the plain 'Naming Screen' shares a name with the animated source"
    );
}

/// Cast Pattern Matcher from hand with `battlefield` already in play and the
/// given library, accept its optional ETB search through `apply()`, and return
/// the library cards offered by the resulting `SearchChoice`.
fn pattern_matcher_search_candidates(
    prepare: impl FnOnce(&mut GameScenario) -> Vec<(&'static str, ObjectId)>,
) -> Vec<(&'static str, bool)> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let matcher = scenario
        .add_creature_to_hand_from_oracle(P0, "Pattern Matcher", 3, 3, PATTERN_MATCHER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let library = prepare(&mut scenario);
    let mut runner = scenario.build();

    runner.cast(matcher).commit();
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "Pattern Matcher's optional ETB search must be offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let WaitingFor::SearchChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!(
            "accepted search must prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    library
        .into_iter()
        .map(|(name, id)| (name, cards.contains(&id)))
        .collect()
}

/// P1 — preservation. With the cast Pattern Matcher and a Grizzly Bears on the
/// battlefield, the library Grizzly Bears shares a name with "another creature you
/// control"; the library Pattern Matcher shares a name only with the SOURCE, which
/// "another" excludes; Gray Ogre shares with nothing.
#[test]
fn p1_pattern_matcher_cannot_find_a_copy_of_itself_alone() {
    let offered = pattern_matcher_search_candidates(|scenario| {
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        let library_matcher = scenario.add_card_to_library_top(P0, "Pattern Matcher");
        let library_bears = scenario.add_card_to_library_top(P0, "Grizzly Bears");
        let library_ogre = scenario.add_card_to_library_top(P0, "Gray Ogre");
        vec![
            ("Grizzly Bears", library_bears),
            ("Pattern Matcher", library_matcher),
            ("Gray Ogre", library_ogre),
        ]
    });
    assert_eq!(
        offered,
        vec![
            ("Grizzly Bears", true),
            ("Pattern Matcher", false),
            ("Gray Ogre", false),
        ]
    );
}

/// P2 — preservation (ruling 2019-07-12). An earlier Pattern Matcher is "another
/// creature you control" for the one just cast, so a third Pattern Matcher can be
/// found.
#[test]
fn p2_pattern_matcher_finds_a_copy_of_an_earlier_pattern_matcher() {
    let offered = pattern_matcher_search_candidates(|scenario| {
        scenario.add_creature(P0, "Pattern Matcher", 3, 3);
        let library_matcher = scenario.add_card_to_library_top(P0, "Pattern Matcher");
        let library_bears = scenario.add_card_to_library_top(P0, "Grizzly Bears");
        vec![
            ("Pattern Matcher", library_matcher),
            ("Grizzly Bears", library_bears),
        ]
    });
    assert_eq!(
        offered,
        vec![("Pattern Matcher", true), ("Grizzly Bears", false)]
    );
}

/// The object id of the unique card named `name` in P0's hand.
fn p0_hand_card_named(runner: &GameRunner, name: &str) -> ObjectId {
    let state = runner.state();
    state.players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .find(|id| state.objects[id].name == name)
        .unwrap_or_else(|| panic!("P0's hand has no card named {name}"))
}

/// The face-down permanents currently on the battlefield.
fn face_down_permanents(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.face_down)
        .map(|object| object.id)
        .collect()
}

/// CR 201.2a + CR 708.2a + CR 701.40a: two manifested (face-down) permanents have
/// no name, so neither shares a name with any other creature you control —
/// including with each other — and both get +1/+1 (2/2 -> 3/3).
///
/// Fails on revert: the pre-fix `DoesNotShare` arm also required the compared
/// object's name set to be non-empty, which a face-down permanent can never
/// satisfy, so a nameless creature stayed 2/2.
#[test]
fn n6_face_down_creatures_share_no_name() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let screen = scenario
        .add_enchantment_from_oracle(P0, "Naming Screen", NAMING_SCREEN)
        .id();
    let bears_a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let bears_b = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ogre = scenario.add_creature(P0, "Gray Ogre", 2, 2).id();
    for name in ["Face-Down Maker One", "Face-Down Maker Two"] {
        scenario
            .add_spell_to_hand(P0, name, false)
            .from_oracle_text("Manifest the top card of your library.")
            .with_mana_cost(ManaCost::generic(0));
    }
    // CR 701.40a: manifest takes the top card of the library. Both are named
    // "Grizzly Bears" as hidden-name insurance — a regression that compares the
    // underlying card name instead of the (empty) face-down name would make the
    // manifested permanents share with the battlefield Grizzly Bears and drop
    // the bonus.
    scenario.add_card_to_library_top(P0, "Grizzly Bears");
    scenario.add_card_to_library_top(P0, "Grizzly Bears");
    let mut runner = scenario.build();
    assert_naming_screen_static(&runner, screen);

    let first = p0_hand_card_named(&runner, "Face-Down Maker One");
    let second = p0_hand_card_named(&runner, "Face-Down Maker Two");
    runner.cast(first).resolve();
    runner.advance_until_stack_empty();
    runner.cast(second).resolve();
    runner.advance_until_stack_empty();

    let face_down = face_down_permanents(&runner);
    assert_eq!(
        face_down.len(),
        2,
        "reach guard: two manifested permanents, got {face_down:?}"
    );

    // Reach guard: the static still applies to a uniquely named creature.
    assert_eq!(
        effective_pt(&mut runner, ogre),
        (3, 3),
        "the uniquely named Gray Ogre gets the bonus"
    );
    for &id in &face_down {
        assert_eq!(
            runner.state().objects[&id].name,
            "",
            "CR 708.2a: a manifested permanent has no name"
        );
        assert_eq!(
            effective_pt(&mut runner, id),
            (3, 3),
            "CR 201.2a: a nameless creature shares a name with nothing, so it \
             gets +1/+1"
        );
    }
    // Negative sibling: two Grizzly Bears share a name, so neither is buffed.
    assert_eq!(effective_pt(&mut runner, bears_a), (2, 2));
    assert_eq!(effective_pt(&mut runner, bears_b), (2, 2));
}
