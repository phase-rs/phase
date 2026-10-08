use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const ERRATIC_MUTATION_ORACLE: &str = "Choose target creature. Reveal cards from the top of your library until you reveal a nonland card. That creature gets +X/-X until end of turn, where X is that card's mana value. Put all cards revealed this way on the bottom of your library in any order.";

#[test]
fn erratic_mutation_single_target_and_cards_to_bottom() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Victim", 2, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Erratic Mutation", true, ERRATIC_MUTATION_ORACLE)
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    // Setup library: bottom to top
    // Engine convention: `library[0]` is the top. `add_card_to_library_top` inserts at index 0.
    // So to have Top = Land1, Land2, Nonland (MV 3), Other:
    // We add them in reverse order:
    // 1. Other
    // 2. Nonland (MV 3)
    // 3. Land2
    // 4. Land1
    let other = scenario.add_card_to_library_top(P0, "Deep Card");
    let nonland = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let land2 = scenario
        .add_spell_to_library_top(P0, "Island", false)
        .as_land()
        .id();
    let land1 = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .id();

    let mut runner = scenario.build();

    // Verify library order before cast: [land1, land2, nonland, other]
    {
        let p0_lib = &runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .library;
        assert_eq!(p0_lib[0], land1);
        assert_eq!(p0_lib[1], land2);
        assert_eq!(p0_lib[2], nonland);
        assert_eq!(p0_lib[3], other);
    }

    let outcome = runner.cast(spell).target_object(victim).resolve();

    // Assertions:
    // 1. Hand did not draw any cards (Issue 2: nonland card does NOT go to hand).
    outcome.assert_hand_drawn(P0, 0);

    // 2. All revealed cards (land1, land2, nonland) are in the library.
    outcome.assert_zone(&[land1, land2, nonland], Zone::Library);

    // CR 701.20a: Check exact library order after "in any order" bottom placement.
    // The driver submits the encounter order [land1, land2, nonland], which are
    // placed on bottom under the existing card `other`.
    // Result from top to bottom: [other, land1, land2, nonland].
    {
        let p0_lib = &runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .library;
        assert_eq!(
            p0_lib.iter().copied().collect::<Vec<_>>(),
            vec![other, land1, land2, nonland],
            "library order must preserve bottom placement of revealed cards in submitted order"
        );
    }

    // 3. Check victim P/T: base is 2/5. With +3/-3 from revealed Divination (MV 3), should be 5/2.
    evaluate_layers(runner.state_mut());
    let victim_obj = &runner.state().objects[&victim];
    assert_eq!(victim_obj.power, Some(5));
    assert_eq!(victim_obj.toughness, Some(2));
}

/// CR 608.2d + CR 701.20a: "Put all cards revealed this way on the bottom of your library
/// in any order." When 2+ cards are revealed and bottomed, the engine pauses with
/// `WaitingFor::RevealUntilBottomOrder` for the controller to announce their chosen
/// permutation. Submitting a custom permutation must place the cards on the library bottom
/// in that exact submitted order.
#[test]
fn erratic_mutation_custom_bottom_order_permutation() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Victim", 2, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Erratic Mutation", true, ERRATIC_MUTATION_ORACLE)
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let other = scenario.add_card_to_library_top(P0, "Deep Card");
    let nonland = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let land2 = scenario
        .add_spell_to_library_top(P0, "Island", false)
        .as_land()
        .id();
    let land1 = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .id();

    let mut runner = scenario.build();

    // Cast Erratic Mutation targeting victim and resolve down to the bottom-order choice.
    let mut committed = runner.cast(spell).target_object(victim).commit();
    committed.act(GameAction::PassPriority).unwrap();
    committed.act(GameAction::PassPriority).unwrap();

    // Verify engine paused on WaitingFor::RevealUntilBottomOrder
    match &committed.state().waiting_for {
        WaitingFor::RevealUntilBottomOrder { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(cards, &[land1, land2, nonland]);
        }
        other_wait => panic!("expected RevealUntilBottomOrder, got {other_wait:?}"),
    }

    // Submit a custom non-encounter permutation: [nonland, land2, land1]
    let custom_order = vec![nonland, land2, land1];
    committed
        .act(GameAction::SelectCards {
            cards: custom_order.clone(),
        })
        .unwrap();

    // Verify exact library order matches custom submission: [other, nonland, land2, land1]
    {
        let p0_lib = &committed
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .library;
        assert_eq!(
            p0_lib.iter().copied().collect::<Vec<_>>(),
            vec![other, nonland, land2, land1],
            "library bottom must match the custom submitted permutation"
        );
    }

    // Check victim P/T: base 2/5 with +3/-3 from Divination (MV 3) -> 5/2.
    evaluate_layers(committed.state_mut());
    let victim_obj = &committed.state().objects[&victim];
    assert_eq!(victim_obj.power, Some(5));
    assert_eq!(victim_obj.toughness, Some(2));
}

/// CR 608.2c + CR 616.1: When zone changes during RevealUntil resolution trigger
/// competing replacement effects, the engine pauses on `WaitingFor::ReplacementChoice`.
/// After answering the replacement choice, the batch completion must carry the hit
/// snapshot so the chained pump still resolves against the revealed card's mana value.
#[test]
fn erratic_mutation_replacement_pause_preserves_mana_value_referent() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Victim", 2, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Erratic Mutation", true, ERRATIC_MUTATION_ORACLE)
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    // Install two competing library-to-exile replacement effects on the battlefield.
    let lib_redirect = |desc: &str| {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Library)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    origin: None,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .description(desc.to_string())
    };

    scenario
        .add_creature(P0, "Redirect A", 1, 1)
        .as_enchantment()
        .with_replacement_definition(lib_redirect("Redirect A"));
    scenario
        .add_creature(P0, "Redirect B", 1, 1)
        .as_enchantment()
        .with_replacement_definition(lib_redirect("Redirect B"));

    let other = scenario.add_card_to_library_top(P0, "Deep Card");
    let nonland = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let land2 = scenario
        .add_spell_to_library_top(P0, "Island", false)
        .as_land()
        .id();
    let land1 = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .id();

    let mut runner = scenario.build();

    // Cast Erratic Mutation targeting victim.
    // Resolution hits competing replacement redirects (Library -> Exile), pauses on
    // ReplacementChoice, answers via policy index 0, and resumes batch completion.
    let outcome = runner
        .cast(spell)
        .target_object(victim)
        .replacement_choice(0)
        .resolve();

    // All revealed cards (land1, land2, nonland) were redirected to Exile by the replacement effect.
    let state = outcome.state();
    assert_eq!(state.objects[&nonland].zone, Zone::Exile);
    assert_eq!(state.objects[&land1].zone, Zone::Exile);
    assert_eq!(state.objects[&land2].zone, Zone::Exile);
    assert_eq!(state.objects[&other].zone, Zone::Library);

    // After resolution, check victim P/T: base 2/5 + 3/-3 (Divination MV 3) -> 5/2.
    let victim_obj = &outcome.state().objects[&victim];
    assert_eq!(
        victim_obj.power,
        Some(5),
        "power must be 5 (2 + 3 from hit card MV across replacement pause)"
    );
    assert_eq!(
        victim_obj.toughness,
        Some(2),
        "toughness must be 2 (5 - 3 from hit card MV across replacement pause)"
    );
}

/// CR 701.20a + CR 608.2c: "Put those land cards onto the battlefield tapped and
/// the rest on the bottom of your library in a random order." The rest pile must
/// be placed on the bottom in random order (not pausing for player choice and not preserving top order).
#[test]
fn the_ring_goes_south_battlefield_tapped_and_random_bottom_rest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let legendary = scenario
        .add_creature(P0, "Frodo Baggins", 2, 2)
        .as_legendary()
        .id();

    let oracle = "The Ring tempts you. Then reveal cards from the top of your library until you reveal X land cards, where X is the number of legendary creatures you control. Put those land cards onto the battlefield tapped and the rest on the bottom of your library in a random order.";
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Ring Goes South", false, oracle)
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let land = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .id();
    let spell2 = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .id();
    let spell1 = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", false)
        .id();

    let mut runner = scenario.build();

    let mut committed = runner.cast(spell).commit();
    committed
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    committed
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();

    // The Ring tempts you prompts for Ring Bearer choice
    if let engine::types::game_state::WaitingFor::ChooseRingBearer { .. } =
        committed.state().waiting_for
    {
        committed
            .act(engine::types::actions::GameAction::ChooseRingBearer { target: legendary })
            .unwrap();
    }

    // Resolution completes: land enters battlefield tapped, spell1 and spell2 are bottomed (under deep) in random order.
    assert_eq!(committed.state().objects[&land].zone, Zone::Battlefield);
    assert!(committed.state().objects[&land].tapped);

    let p0_lib = &committed
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .library;
    assert_eq!(p0_lib[0], deep);
    assert!(p0_lib.contains(&spell1));
    assert!(p0_lib.contains(&spell2));
    assert_eq!(p0_lib.len(), 3);
}

/// CR 608.2c + CR 607.1: A compound exile chain (e.g. ExileTop followed by GrantCastingPermission
/// with TargetFilter::TrackedSet) must grant casting permissions over the entire tracked set,
/// not just the last exiled card from the final ExileTop step.
#[test]
fn compound_exile_grants_casting_permission_over_full_tracked_set() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, Effect, LibraryInstructionActor,
        LibraryPosition, TargetFilter,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Build a custom compound exile spell:
    // 1. Exile top 2 cards
    // 2. Grant casting permission over TargetFilter::TrackedSet
    let mut ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ExileTop {
            player: TargetFilter::Controller,
            count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
            position: LibraryPosition::Top,
            face_down: false,
            actor: LibraryInstructionActor::Controller,
        },
    );
    ability.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GrantCastingPermission {
            permission: CastingPermission::ExileWithAltCost {
                cost: ManaCost::zero(),
                cost_provenance: engine::types::ability::ExileGrantCostProvenance::Alternative,
                cast_transformed: false,
                constraint: None,
                granted_to: None,
                resolution_cleanup: None,
                duration: None,
                source_id: None,
                graveyard_replacement: None,
                mana_spend_permission: None,
                enters_with_counter: None,
                enters_with_modifications: Vec::new(),
                cast_cost_modifier: None,
            },
            target: TargetFilter::TrackedSet {
                id: engine::types::identifiers::TrackedSetId(0),
            },
            grantee: engine::types::ability::PermissionGrantee::AbilityController,
        },
    )));

    let spell = scenario
        .add_spell_to_hand(P0, "Impulse Exile", false)
        .with_ability_definition(ability)
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let card2 = scenario
        .add_spell_to_library_top(P0, "Card Two", false)
        .id();
    let card1 = scenario
        .add_spell_to_library_top(P0, "Card One", false)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    let state = outcome.state();
    assert_eq!(state.objects[&card1].zone, Zone::Exile);
    assert_eq!(state.objects[&card2].zone, Zone::Exile);

    // Both cards must have casting permissions granted
    assert!(
        state.objects[&card1]
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::ExileWithAltCost { granted_to: Some(p_id), .. } if *p_id == P0)),
        "card1 must have casting permission granted from tracked set"
    );
    assert!(
        state.objects[&card2]
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::ExileWithAltCost { granted_to: Some(p_id), .. } if *p_id == P0)),
        "card2 must have casting permission granted from tracked set"
    );
}

/// CR 401.4: synthetic grammar fixture, not a printed card. Omitting an order
/// instruction must still let the owner order two cards placed on the bottom.
#[test]
fn reveal_until_unspecified_bottom_order_is_owner_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unspecified Bottom Order Test",
            true,
            "Reveal cards from the top of your library until you reveal a nonland card. Put that card into your hand and the rest on the bottom of your library.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let hit = scenario.add_spell_to_library_top(P0, "Hit", false).id();
    let second = scenario
        .add_spell_to_library_top(P0, "Second Land", false)
        .as_land()
        .id();
    let first = scenario
        .add_spell_to_library_top(P0, "First Land", false)
        .as_land()
        .id();
    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).commit();
    committed.act(GameAction::PassPriority).unwrap();
    committed.act(GameAction::PassPriority).unwrap();
    match &committed.state().waiting_for {
        WaitingFor::RevealUntilBottomOrder { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(cards, &[first, second]);
        }
        other => panic!("expected owner ordering choice, got {other:?}"),
    }
    // CR 401.4: AI candidate generation must offer alternate permutations,
    // including [second, first] ([B, A]).
    let ai_candidates = engine::ai_support::candidate_actions(committed.state());
    let has_b_a = ai_candidates.iter().any(
        |c| matches!(&c.action, GameAction::SelectCards { cards } if cards == &[second, first]),
    );
    assert!(
        has_b_a,
        "CR 401.4: AI candidates must include the alternate permutation [second, first]"
    );
    assert_eq!(committed.state().objects[&hit].zone, Zone::Hand);
    committed
        .act(GameAction::SelectCards {
            cards: vec![second, first],
        })
        .unwrap();
    let library = &committed
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .library;
    assert_eq!(
        library.iter().copied().collect::<Vec<_>>(),
        vec![deep, second, first]
    );
}

/// CR 401.4: synthetic grammar fixture, not a printed card. Omitting an order
/// instruction must still let the owner order three cards placed on the bottom.
#[test]
fn reveal_until_all_unspecified_bottom_order_is_owner_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "All Unspecified Bottom Order Test",
            true,
            "Reveal cards from the top of your library until you reveal a nonland card. Put all cards revealed this way on the bottom of your library.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let hit = scenario.add_spell_to_library_top(P0, "Hit", false).id();
    let second = scenario
        .add_spell_to_library_top(P0, "Second Land", false)
        .as_land()
        .id();
    let first = scenario
        .add_spell_to_library_top(P0, "First Land", false)
        .as_land()
        .id();
    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).commit();
    committed.act(GameAction::PassPriority).unwrap();
    committed.act(GameAction::PassPriority).unwrap();
    match &committed.state().waiting_for {
        WaitingFor::RevealUntilBottomOrder { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(cards, &[first, second, hit]);
        }
        other => panic!("expected owner ordering choice, got {other:?}"),
    }
    assert_eq!(committed.state().objects[&hit].zone, Zone::Library);
    committed
        .act(GameAction::SelectCards {
            cards: vec![hit, second, first],
        })
        .unwrap();
    let library = &committed
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .library;
    assert_eq!(
        library.iter().copied().collect::<Vec<_>>(),
        vec![deep, hit, second, first]
    );
}

/// CR 701.24a: When a spell reveals until a condition and then instructs to shuffle the
/// library (e.g. The Crimson Avenger, Underdark Beholder), the whole library including
/// unrevealed cards must be shuffled, rather than only placing the revealed pile.
#[test]
fn reveal_until_then_shuffle_randomizes_unrevealed_library_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let oracle = "Reveal cards from the top of your library until you reveal a nonland card. Put that card into your hand. Then shuffle your library.";
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Reveal and Shuffle", true, oracle)
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    // Setup library: bottom to top
    // Cards: Deep3, Deep2, Deep1, Hit (nonland), Land2, Land1
    let deep3 = scenario.add_card_to_library_top(P0, "Deep 3");
    let deep2 = scenario.add_card_to_library_top(P0, "Deep 2");
    let deep1 = scenario.add_card_to_library_top(P0, "Deep 1");
    let hit = scenario.add_spell_to_library_top(P0, "Hit", false).id();
    let land2 = scenario
        .add_spell_to_library_top(P0, "Land 2", false)
        .as_land()
        .id();
    let land1 = scenario
        .add_spell_to_library_top(P0, "Land 1", false)
        .as_land()
        .id();

    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).commit();
    // Collect every event emitted while the spell resolves, so the shuffle
    // assertion observes the production action pipeline rather than zones.
    let mut events: Vec<GameEvent> = Vec::new();
    events.extend(committed.act(GameAction::PassPriority).unwrap().events);
    events.extend(committed.act(GameAction::PassPriority).unwrap().events);

    // If bottom order was prompted, answer it
    if let WaitingFor::RevealUntilBottomOrder { .. } = &committed.state().waiting_for {
        events.extend(
            committed
                .act(GameAction::SelectCards {
                    cards: vec![land1, land2],
                })
                .unwrap()
                .events,
        );
    }

    // Hit card must be in hand
    assert_eq!(committed.state().objects[&hit].zone, Zone::Hand);

    // CR 701.24a: "Then shuffle your library" performs a library shuffle for
    // the caster. Placing only the revealed pile would leave this event absent.
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                }
            ))
            .count(),
        1,
        "the caster's library must be shuffled exactly once, got {events:?}"
    );

    // CR 701.24a: The remaining library must contain Land1, Land2, Deep1, Deep2, Deep3 (all 5 cards)
    let p0_lib = &committed
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .library;
    assert_eq!(p0_lib.len(), 5);
    for id in [land1, land2, deep1, deep2, deep3] {
        assert!(
            p0_lib.contains(&id),
            "library must contain remaining card {id:?}"
        );
        assert_eq!(committed.state().objects[&id].zone, Zone::Library);
    }
}

/// CR 406.3 + CR 608.2c: When Clone Shell dies, it turns the exiled card face up,
/// and if it's a creature card, puts it onto the battlefield under your control.
/// The follow-up ChangeZone resolves ParentTarget from Zone::Exile.
#[test]
fn clone_shell_dies_trigger_puts_exiled_creature_onto_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let dies_oracle = "When Clone Shell dies, turn the exiled card face up. If it's a creature card, put it onto the battlefield under your control.";
    let shell = scenario
        .add_creature_from_oracle(P0, "Clone Shell", 2, 2, dies_oracle)
        .id();

    let exiled_creature = scenario
        .add_creature_to_exile(P0, "Colossal Dreadmaw", 6, 6)
        .id();

    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&exiled_creature)
        .unwrap()
        .face_down = true;
    runner
        .state_mut()
        .exile_links
        .push(engine::types::game_state::ExileLink {
            source_id: shell,
            exiled_id: exiled_creature,
            kind: engine::types::game_state::ExileLinkKind::TrackedBySource,
        });

    runner.cast(murder).target_object(shell).resolve();

    // Clone Shell died and was put into the graveyard.
    assert_eq!(runner.state().objects[&shell].zone, Zone::Graveyard);

    // The exiled creature was turned face up and put onto the battlefield under P0's control.
    let obj = &runner.state().objects[&exiled_creature];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.controller, P0);
    assert!(!obj.face_down);
}

/// CR 406.3 + CR 608.2c: When Clone Shell dies, if the exiled card is not a creature card,
/// it is turned face up in exile but is NOT put onto the battlefield.
#[test]
fn clone_shell_dies_trigger_leaves_non_creature_in_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let dies_oracle = "When Clone Shell dies, turn the exiled card face up. If it's a creature card, put it onto the battlefield under your control.";
    let shell = scenario
        .add_creature_from_oracle(P0, "Clone Shell", 2, 2, dies_oracle)
        .id();

    let exiled_spell = scenario.add_spell_to_exile(P0, "Lightning Bolt", true).id();

    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&exiled_spell)
        .unwrap()
        .face_down = true;
    runner
        .state_mut()
        .exile_links
        .push(engine::types::game_state::ExileLink {
            source_id: shell,
            exiled_id: exiled_spell,
            kind: engine::types::game_state::ExileLinkKind::TrackedBySource,
        });

    runner.cast(murder).target_object(shell).resolve();

    // Clone Shell died and was put into the graveyard.
    assert_eq!(runner.state().objects[&shell].zone, Zone::Graveyard);

    // The non-creature card was turned face up in exile, but stayed in exile.
    let obj = &runner.state().objects[&exiled_spell];
    assert_eq!(obj.zone, Zone::Exile);
    assert!(!obj.face_down);
}

/// Printed Oracle text of Transmogrify (MTGJSON AtomicCards).
const TRANSMOGRIFY_ORACLE: &str = "Exile target creature. That creature's controller reveals cards from the top of their library until they reveal a creature card. That player puts that card onto the battlefield, then shuffles the rest into their library.";

/// CR 701.20a + CR 701.24c: Transmogrify's subject-elided "then shuffles the
/// rest into their library" shuffles the REVEALING player's library — the
/// exiled creature's controller — not the caster's.
#[test]
fn transmogrify_shuffles_the_revealing_players_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Transmogrify", false, TRANSMOGRIFY_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    // P1 library, top first: Land, Hit (creature), Deep.
    let deep = scenario.add_card_to_library_top(P1, "Deep Card");
    let hit = scenario
        .add_spell_to_library_top(P1, "Hit Creature", false)
        .as_creature()
        .id();
    let land = scenario
        .add_spell_to_library_top(P1, "Revealed Land", false)
        .as_land()
        .id();
    scenario.add_card_to_library_top(P0, "Caster Card");

    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).target_object(victim).commit();
    let mut events: Vec<GameEvent> = Vec::new();
    events.extend(committed.act(GameAction::PassPriority).unwrap().events);
    events.extend(committed.act(GameAction::PassPriority).unwrap().events);

    let state = committed.state();
    assert_eq!(state.objects[&victim].zone, Zone::Exile);
    assert_eq!(state.objects[&hit].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&hit].controller, P1,
        "the revealed creature enters under the revealing player's control"
    );
    for id in [land, deep] {
        assert_eq!(state.objects[&id].zone, Zone::Library);
    }

    let shuffles_for = |player| {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    } if *player_id == player
                )
            })
            .count()
    };
    assert_eq!(
        shuffles_for(P1),
        1,
        "the exiled creature's controller shuffles their library, got {events:?}"
    );
    assert_eq!(
        shuffles_for(P0),
        0,
        "the caster's library is not shuffled, got {events:?}"
    );
}
