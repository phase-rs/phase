use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::EffectKind;
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

    // CR 608.2c + CR 401.4: the pile is placed by the last instruction, after the
    // pump; its owner arranges it. Submit the encounter order.
    let outcome = runner
        .cast(spell)
        .target_object(victim)
        .effect_zone(&[land1, land2, nonland])
        .resolve();

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

/// CR 608.2c + CR 401.4 + CR 701.20a: "Put all cards revealed this way on the bottom of your
/// library in any order." The placement is the last instruction, so it is followed after the
/// pump: when 2+ cards are revealed, the engine pauses with `WaitingFor::EffectZoneChoice`
/// for the owner to arrange the pile. Submitting a custom permutation must place the cards
/// on the library bottom in that exact submitted order.
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

    // CR 608.2c + CR 701.20b: the pump has already resolved when the pile placement
    // pauses, and the revealed cards are still where the reveal found them.
    evaluate_layers(committed.state_mut());
    assert_eq!(committed.state().objects[&victim].power, Some(5));
    assert_eq!(
        committed.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![land1, land2, nonland, other],
        "the pile is not placed before the pump instruction"
    );

    // Verify engine paused on the owner's arrangement of the whole revealed pile.
    match &committed.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(
                cards
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                [land1, land2, nonland].into_iter().collect(),
            );
        }
        other_wait => panic!("expected EffectZoneChoice, got {other_wait:?}"),
    }

    // Submit a custom non-encounter permutation: [nonland, land2, land1]
    let custom_order = vec![nonland, land2, land1];

    // CR 401.4: AI candidate generation enumerates alternate permutations for owner arrangement.
    let candidates = engine::ai_support::candidate_actions_broad(committed.state());
    let candidate_orders: Vec<Vec<ObjectId>> = candidates
        .into_iter()
        .filter_map(|cand| match cand.action {
            GameAction::SelectCards { cards } => Some(cards),
            _ => None,
        })
        .collect();
    assert!(
        candidate_orders.len() > 1,
        "expected alternate ordering permutations for AI, got {candidate_orders:?}"
    );
    assert!(
        candidate_orders.contains(&custom_order),
        "AI candidates should contain custom permutation"
    );

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
                enters_with_modifications: Default::default(),
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

const GOBLIN_MACHINIST_ORACLE: &str = "{2}{R}: Reveal cards from the top of your library until you reveal a nonland card. This creature gets +X/+0 until end of turn, where X is that card's mana value. Put the revealed cards on the bottom of your library in any order.";

/// CR 608.2c + CR 401.4 + CR 701.20a: Goblin Machinist — "{2}{R}: Reveal cards
/// from the top of your library until you reveal a nonland card. This creature
/// gets +X/+0 until end of turn, where X is that card's mana value. Put the
/// revealed cards on the bottom of your library in any order."
///
/// Activating the ability reveals cards until a nonland card is found, pumps
/// Goblin Machinist's power by that card's mana value, and pauses with
/// `WaitingFor::EffectZoneChoice` for the owner to arrange the revealed pile on
/// the bottom of the library. Verifies AI candidates include alternate
/// permutations and custom order is respected.
#[test]
fn goblin_machinist_activation_pump_and_owner_arranged_bottom() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let machinist = scenario
        .add_creature_from_oracle(P0, "Goblin Machinist", 0, 5, GOBLIN_MACHINIST_ORACLE)
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
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
        .add_spell_to_library_top(P0, "Mountain", false)
        .as_land()
        .id();
    let land1 = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: machinist,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Goblin Machinist ability");

    // Pass priority from both players down to the bottom-order choice
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();

    // Verify pump applied: base 0/5 + Divination MV 3 -> 3/5
    evaluate_layers(runner.state_mut());
    let machinist_obj = &runner.state().objects[&machinist];
    assert_eq!(machinist_obj.power, Some(3));
    assert_eq!(machinist_obj.toughness, Some(5));

    // Verify engine paused on EffectZoneChoice with mass library ordering
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, P0);
            assert!(
                mass_library_order.is_some(),
                "expected mass_library_order for bottom arrangement"
            );
            assert_eq!(
                cards
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                [land1, land2, nonland].into_iter().collect(),
            );
        }
        other_wait => panic!("expected EffectZoneChoice, got {other_wait:?}"),
    }

    // CR 401.4: AI candidate generation enumerates alternate permutations for owner arrangement.
    let custom_order = vec![nonland, land1, land2];
    let candidates = engine::ai_support::candidate_actions_broad(runner.state());
    let candidate_orders: Vec<Vec<ObjectId>> = candidates
        .into_iter()
        .filter_map(|cand| match cand.action {
            GameAction::SelectCards { cards } => Some(cards),
            _ => None,
        })
        .collect();
    assert!(
        candidate_orders.len() > 1,
        "expected alternate ordering permutations for AI, got {candidate_orders:?}"
    );
    assert!(
        candidate_orders.contains(&custom_order),
        "AI candidates should contain custom permutation"
    );

    // Submit custom permutation and verify final library bottom
    runner
        .act(GameAction::SelectCards {
            cards: custom_order.clone(),
        })
        .unwrap();

    let p0_lib = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .library;
    assert_eq!(
        p0_lib.iter().copied().collect::<Vec<_>>(),
        vec![other, nonland, land1, land2],
        "library bottom must match custom submitted permutation"
    );

    // Final P/T check
    evaluate_layers(runner.state_mut());
    let machinist_obj = &runner.state().objects[&machinist];
    assert_eq!(machinist_obj.power, Some(3));
    assert_eq!(machinist_obj.toughness, Some(5));
}

const GRIPTIDE_ORACLE: &str = "Put target creature on top of its owner's library.";

/// CR 701.20b + CR 701.20d + CR 903.9b: Goblin Machinist — in-library placement
/// of a revealed tucked commander completes without offering an extra command-zone
/// return choice, while Griptide still offers the actual cross-zone replacement choice.
#[test]
fn goblin_machinist_reordering_revealed_commander_offers_no_extra_command_zone_return() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let krenko = scenario
        .add_creature(P0, "Krenko, Mob Boss", 3, 3)
        .as_legendary()
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_commander(krenko);

    let machinist = scenario
        .add_creature_from_oracle(P0, "Goblin Machinist", 0, 5, GOBLIN_MACHINIST_ORACLE)
        .id();

    let griptide = scenario
        .add_spell_to_hand_from_oracle(P1, "Griptide", true, GRIPTIDE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let _deep = scenario.add_card_to_library_top(P0, "Deep Card");

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let mut setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        krenko,
        Zone::Battlefield,
        &mut setup_events,
    );

    // P0 passes priority so P1 can cast Griptide targeting P0's commander Krenko
    runner.act(GameAction::PassPriority).unwrap();

    let griptide_card_id = runner.state().objects[&griptide].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: griptide,
            card_id: griptide_card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("P1 casts Griptide targeting Krenko");

    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(krenko)),
        })
        .expect("select Krenko as target for Griptide");

    // Pass priority to resolve Griptide
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();

    // CR 903.9b: Griptide moving Krenko from Battlefield to Library MUST offer
    // the command-zone return replacement choice.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Griptide moving commander to library must offer CR 903.9b replacement, got {:?}",
        runner.state().waiting_for
    );

    // Decline the replacement (index 1) so Krenko is tucked onto the top of the library.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline commander replacement");

    assert_eq!(
        runner.state().objects[&krenko].zone,
        Zone::Library,
        "Krenko must be tucked in the library"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.front(),
        Some(&krenko),
        "Krenko is on top of P0's library"
    );

    // Both players pass priority to clear Griptide's resolution and return to P0 priority
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ) {
        runner.act(GameAction::PassPriority).unwrap();
    }

    // P0 activates Goblin Machinist before drawing.
    runner
        .act(GameAction::ActivateAbility {
            source_id: machinist,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Goblin Machinist ability");

    // Pass priority from both players to resolve Machinist ability
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();

    // CR 701.20b + CR 701.20d + CR 903.9b: Krenko is revealed as the first nonland card.
    // Machinist puts the revealed cards on the bottom of the library. Because this is an
    // in-library reorder (CR 701.20d), NO command-zone return choice is offered.
    // The resolution completes immediately back to Priority without pausing on ChooseReplacement.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "in-library placement of tucked commander must not offer extra replacement choice, got {:?}",
        runner.state().waiting_for
    );

    // Verify Krenko is now at the bottom of P0's library
    assert_eq!(
        runner.state().players[P0.0 as usize].library.back(),
        Some(&krenko),
        "Krenko was placed on the bottom of P0's library"
    );
    assert_eq!(
        runner.state().objects[&krenko].zone,
        Zone::Library,
        "Krenko remains in the library"
    );

    // Verify pump applied from Krenko's MV (4): base 0/5 + 4 -> 4/5
    evaluate_layers(runner.state_mut());
    let machinist_obj = &runner.state().objects[&machinist];
    assert_eq!(machinist_obj.power, Some(4));
    assert_eq!(machinist_obj.toughness, Some(5));
}

/// CR 701.20b + CR 701.20d + CR 903.9b: Goblin Machinist reveals multiple cards
/// including a tucked commander and a land card. When the owner answers the
/// bottom-order choice, the tucked commander is reordered within the library
/// through the library authority without constructing a zone-change proposal
/// or offering an extra command-zone choice.
#[test]
fn goblin_machinist_reordering_multiple_revealed_cards_with_commander_offers_no_extra_command_zone_choice(
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let krenko = scenario
        .add_creature(P0, "Krenko, Mob Boss", 3, 3)
        .as_legendary()
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_commander(krenko);

    let machinist = scenario
        .add_creature_from_oracle(P0, "Goblin Machinist", 0, 5, GOBLIN_MACHINIST_ORACLE)
        .id();

    let griptide = scenario
        .add_spell_to_hand_from_oracle(P1, "Griptide", true, GRIPTIDE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let _deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let mountain = scenario
        .add_spell_to_library_top(P0, "Mountain", false)
        .as_land()
        .id();

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let mut setup_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        krenko,
        Zone::Battlefield,
        &mut setup_events,
    );

    // P0 passes priority so P1 can cast Griptide targeting P0's commander Krenko
    runner.act(GameAction::PassPriority).unwrap();

    let griptide_card_id = runner.state().objects[&griptide].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: griptide,
            card_id: griptide_card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("P1 casts Griptide targeting Krenko");

    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(krenko)),
        })
        .expect("select Krenko as target for Griptide");

    // Pass priority to resolve Griptide
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();

    // CR 903.9b: Griptide moving Krenko from Battlefield to Library MUST offer
    // the command-zone return replacement choice.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Griptide moving commander to library must offer CR 903.9b replacement, got {:?}",
        runner.state().waiting_for
    );

    // Decline the replacement (index 1) so Krenko is tucked onto the top of the library.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline commander replacement");

    // Move Mountain to index 0 on top of Krenko so Machinist reveals [Mountain, Krenko] (2 cards)
    let mut tuck_events = Vec::new();
    engine::game::zones::move_to_library_at_index(
        runner.state_mut(),
        mountain,
        Some(0),
        &mut tuck_events,
    );

    // Both players pass priority to clear Griptide's resolution and return to P0 priority
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ) {
        runner.act(GameAction::PassPriority).unwrap();
    }

    // P0 activates Goblin Machinist
    runner
        .act(GameAction::ActivateAbility {
            source_id: machinist,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Goblin Machinist ability");

    // Pass priority from both players to resolve Machinist ability
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();

    // With 2 cards revealed (Mountain and Krenko), owner is prompted for bottom order
    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected EffectZoneChoice for 2 revealed cards, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        cards
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [mountain, krenko].into_iter().collect()
    );

    // Answering bottom order with SelectCards reorders within library
    let _res = runner
        .act(GameAction::SelectCards {
            cards: vec![krenko, mountain],
        })
        .expect("answer the bottom order");

    // CR 701.20b + CR 701.20d + CR 903.9b: NO command zone choice offered!
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "in-library placement of tucked commander in multi-card reorder must not offer replacement choice, got {:?}",
        runner.state().waiting_for
    );

    // Krenko is at the bottom in the answered order (mountain is on the bottom, krenko above it)
    assert_eq!(
        runner.state().players[P0.0 as usize].library.back(),
        Some(&mountain),
    );
    assert_eq!(
        runner.state().objects[&krenko].zone,
        Zone::Library,
        "Krenko remains in the library"
    );

    // Verify pump applied from Krenko's MV (4): base 0/5 + 4 -> 4/5
    evaluate_layers(runner.state_mut());
    let machinist_obj = &runner.state().objects[&machinist];
    assert_eq!(machinist_obj.power, Some(4));
    assert_eq!(machinist_obj.toughness, Some(5));
}

/// CR 401.2 + CR 401.4 + CR 701.20d: after the owner arranges the revealed
/// pile, the settled order is authoritative state only. The authorized owner
/// receives the arrangement prompt; every viewer's projection — the owner's
/// included, because CR 401.2 keeps even the owner from reading library order —
/// hides the revealed cards and cannot join their previously public identities
/// to the slots the arrangement gave them.
#[test]
fn settled_owner_order_is_not_identifiable_from_any_projection() {
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
    let mut committed = runner.cast(spell).target_object(victim).commit();
    committed.act(GameAction::PassPriority).unwrap();
    committed.act(GameAction::PassPriority).unwrap();

    // Reach guard: the authorized owner receives the ordering prompt over the
    // whole revealed pile.
    match &committed.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the library owner arranges its own pile");
            assert_eq!(
                cards
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                [land1, land2, nonland].into_iter().collect(),
            );
        }
        other_prompt => panic!("expected EffectZoneChoice, got {other_prompt:?}"),
    }

    // CR 701.20a: the pile is public while the instruction still needs it.
    let revealed_to_opponent =
        engine::game::visibility::filter_state_for_viewer(committed.state(), P1);
    assert_ne!(
        revealed_to_opponent.objects[&land1].name,
        engine::game::visibility::HIDDEN_CARD_NAME,
        "CR 701.20a: the revealed pile is public while the owner arranges it"
    );

    // A permutation that is NOT the canonical id order, so the projection's
    // canonical hidden slots cannot coincide with the truth by construction.
    let submitted = vec![nonland, land1, land2];
    committed
        .act(GameAction::SelectCards {
            cards: submitted.clone(),
        })
        .unwrap();

    let true_order: Vec<ObjectId> = committed.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        true_order,
        vec![other, nonland, land1, land2],
        "the submitted arrangement is the authoritative settled order"
    );

    for (label, projected) in [
        (
            "P0",
            engine::game::visibility::filter_state_for_viewer(committed.state(), P0),
        ),
        (
            "P1",
            engine::game::visibility::filter_state_for_viewer(committed.state(), P1),
        ),
        (
            "unseated",
            engine::game::visibility::filter_state_for_unseated_viewer(committed.state()),
        ),
    ] {
        for id in [land1, land2, nonland] {
            assert_eq!(
                projected.objects[&id].name,
                engine::game::visibility::HIDDEN_CARD_NAME,
                "{label}: CR 401.2 — a settled library card carries no identity"
            );
        }
        let projected_order: Vec<ObjectId> = projected.players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(
            projected_order.len(),
            true_order.len(),
            "{label}: reach guard — every settled member is still in the zone"
        );
        assert_eq!(
            projected_order,
            vec![other, nonland, land2, land1],
            "{label}: hidden members occupy canonical slots, not their settled positions"
        );
    }
}

/// CR 614.6 + CR 616.1: A synthetic `Moved` library redirect. The removed
/// replacement is the witness; the replacement is installed on a textless
/// enchantment creature so the board, not a printed card, carries it.
fn library_redirect(
    description: &str,
    destination: Zone,
) -> engine::types::ability::ReplacementDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;

    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Library)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                library_shuffle: Default::default(),
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
        .description(description.to_string())
}

/// A synthetic object-scoped redirect matching a specific destination zone.
fn zone_redirect_for_card(
    description: &str,
    trigger_dest: Zone,
    destination: Zone,
    id: ObjectId,
) -> engine::types::ability::ReplacementDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::EtbTapState;

    let mut definition = ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(trigger_dest)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                library_shuffle: Default::default(),
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
        .description(description.to_string());
    definition.valid_card = Some(TargetFilter::SpecificObject { id });
    definition
}

/// The same synthetic redirect, restricted to one exact object's library move
/// so a multi-card rest pile can keep its other members in the library.
fn library_redirect_for_card(
    description: &str,
    destination: Zone,
    id: ObjectId,
) -> engine::types::ability::ReplacementDefinition {
    zone_redirect_for_card(description, Zone::Library, destination, id)
}

/// CR 701.20b + CR 701.20d: "Put all cards revealed this way on the
/// bottom of your library in any order." Revealing cards does not cause them
/// to leave the library (CR 701.20b), and putting them on the bottom of the
/// library in any order is an in-library reorder (CR 701.20d). In-library
/// placement delegates directly to the library reorder authority without
/// constructing a zone-change proposal or consulting `Moved` (destination: Library)
/// replacements. Even when competing library redirects exist, the revealed cards
/// settle directly on the bottom of the library in the owner's chosen order.
#[test]
fn erratic_mutation_owner_order_reorders_in_library_without_consulting_zone_replacements() {
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
    scenario
        .add_creature(P0, "Redirect A", 1, 1)
        .as_enchantment()
        .with_replacement_definition(library_redirect("Redirect A", Zone::Exile));
    scenario
        .add_creature(P0, "Redirect B", 1, 1)
        .as_enchantment()
        .with_replacement_definition(library_redirect("Redirect B", Zone::Exile));

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
    let mut committed = runner.cast(spell).target_object(victim).commit();
    committed.act(GameAction::PassPriority).unwrap();
    committed.act(GameAction::PassPriority).unwrap();

    // Reach guard: the pump already resolved and the owner is arranging the
    // revealed pile while it is still on top of the library (CR 701.20b).
    evaluate_layers(committed.state_mut());
    assert_eq!(committed.state().objects[&victim].power, Some(5));
    match &committed.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(
                cards
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                [land1, land2, nonland].into_iter().collect(),
            );
        }
        other_wait => panic!("expected EffectZoneChoice, got {other_wait:?}"),
    }

    // Submit the encounter order down the bottom.
    committed
        .act(GameAction::SelectCards {
            cards: vec![land1, land2, nonland],
        })
        .unwrap();

    // CR 701.20b + CR 701.20d: The cards are already in the library, so putting
    // them on the bottom of the library in any order is an in-library reorder,
    // not a zone change. It delegates directly to the library reorder authority
    // without consulting Moved replacements (even if competing library redirects exist).
    assert!(
        matches!(committed.state().waiting_for, WaitingFor::Priority { .. }),
        "in-library placement settles directly without pausing on replacement choices, got {:?}",
        committed.state().waiting_for
    );

    let state = committed.state();
    for (id, label) in [(land1, "land1"), (land2, "land2"), (nonland, "nonland")] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Library,
            "{label} must remain in the library, not redirected to exile"
        );
        assert!(
            !state.revealed_cards.contains(&id),
            "{label} must stop being revealed"
        );
    }
    assert_eq!(
        state.players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![other, land1, land2, nonland],
        "revealed cards settle at the bottom in the chosen order"
    );
    assert!(state.active_batch_delivery().is_none());
    assert!(state.stack.is_empty());
    evaluate_layers(committed.state_mut());
    assert_eq!(committed.state().objects[&victim].power, Some(5));
    assert_eq!(committed.state().objects[&victim].toughness, Some(2));
}

/// Printed Proteus Staff Oracle text (MTGJSON).
const PROTEUS_STAFF: &str = "{2}{U}, {T}: Put target creature on the bottom of its owner's library. That creature's controller reveals cards from the top of their library until they reveal a creature card. The player puts that card onto the battlefield and the rest on the bottom of their library in any order. Activate only as a sorcery.";

/// Drive the activation through priority and replacement pauses until a prompt
/// the resolution driver does not answer (the owner's bottom-order choice). The
/// shared `AbilityActivation::resolve` driver auto-answers
/// `RevealUntilBottomOrder` with the encounter order, which would hide the
/// answer path under test.
fn drive_to_owner_order(runner: &mut engine::game::scenario::GameRunner, limit: usize) {
    for _ in 0..limit {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("answer the library modifier");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the activation");
            }
            _ => return,
        }
    }
    panic!("the activation did not reach the owner-order prompt within {limit} steps");
}

/// Give a staged library card the creature type the reveal is looking for.
fn configure_kept_creature(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    use engine::types::card_type::CoreType;
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
}

/// CR 701.20d + CR 701.20a + CR 608.2c: Proteus Staff's "the rest on the bottom
/// of their library in any order" pauses the resolving instruction on
/// `RevealUntilBottomOrder`. Answering it delivers the rest pile through the
/// replacement-consulting batch, and the batch settles the arrangement's single
/// reorder BEFORE the instruction's reveal cleanup runs: the answered cards
/// become new objects (CR 701.20d) instead of keeping their old occurrences
/// while the cleanup already dropped their reveal markers.
#[test]
fn proteus_staff_owner_order_settles_reorder_before_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let staff = scenario
        .add_artifact_from_oracle(P0, "Proteus Staff", PROTEUS_STAFF)
        .id();
    let victim = scenario.add_vanilla(P0, 2, 2);
    // Top-first library: [Miss A, Miss B, Kept Creature, Deep Card].
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let kept = scenario.add_card_to_library_top(P0, "Kept Creature");
    let miss_b = scenario.add_card_to_library_top(P0, "Miss B");
    let miss_a = scenario.add_card_to_library_top(P0, "Miss A");
    let mut runner = scenario.build();
    configure_kept_creature(runner.state_mut(), kept);
    let incarnations_before = [
        runner.state().objects[&miss_a].incarnation,
        runner.state().objects[&miss_b].incarnation,
    ];

    runner
        .act(GameAction::ActivateAbility {
            source_id: staff,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Proteus Staff");
    drive_to_owner_order(&mut runner, 12);

    // Reach guard: the production owner-order prompt names the two misses in
    // reveal order.
    let WaitingFor::RevealUntilBottomOrder { player, cards, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "expected RevealUntilBottomOrder, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(cards, vec![miss_a, miss_b]);

    // CR 401.4: the owner answers with the bottom order.
    let res = runner
        .act(GameAction::SelectCards {
            cards: vec![miss_b, miss_a],
        })
        .expect("answer the owner's bottom order");

    // F2: assert exactly one EffectResolved for RevealUntil and staff from the owner-answer
    assert_eq!(
        res.events
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::RevealUntil,
                    source_id,
                    ..
                } if *source_id == staff
            ))
            .count(),
        1,
        "public owner-answer must emit exactly one EffectResolved for staff RevealUntil"
    );

    let state = runner.state();
    assert_eq!(
        state.players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![deep, victim, miss_b, miss_a],
        "the answered order is the settled bottom of the library"
    );
    assert_eq!(
        state.objects[&kept].zone,
        Zone::Battlefield,
        "the revealed creature card is the kept card"
    );
    // CR 701.20d: every reordered revealed card stops being revealed and
    // becomes a new object. A cleanup running ahead of the reorder settlement
    // would leave these occurrences unchanged.
    for (id, label, incarnation_before) in [
        (miss_a, "Miss A", incarnations_before[0]),
        (miss_b, "Miss B", incarnations_before[1]),
    ] {
        assert!(
            state.objects[&id].incarnation > incarnation_before,
            "{label} must become a new object when the arrangement settles"
        );
        assert!(
            !state.revealed_cards.contains(&id) && !state.public_revealed_cards.contains(&id),
            "{label} must stop being revealed"
        );
    }
    let opponent_view = engine::game::visibility::filter_state_for_viewer(state, P1);
    for (id, label) in [(miss_a, "Miss A"), (miss_b, "Miss B")] {
        assert_eq!(
            opponent_view.objects[&id].name,
            engine::game::visibility::HIDDEN_CARD_NAME,
            "{label} must be hidden from the opponent after settlement"
        );
    }
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the instruction settled back to priority, got {:?}",
        state.waiting_for
    );
    assert!(state.active_batch_delivery().is_none());
    assert!(state.stack.is_empty());
}

/// CR 701.20d + CR 614.6 + CR 616.1: Proteus Staff's kept creature delivery
/// to the battlefield hits two competing `Moved` (destination: Battlefield) replacements.
/// The instruction pauses on `WaitingFor::ReplacementChoice` before the rest pile is
/// delivered. Resuming from the pause delivers the kept creature, pauses on
/// `WaitingFor::RevealUntilBottomOrder` for the owner's rest-pile bottom order, and
/// answering it settles the arrangement's reorder before reveal cleanup runs — emitting
/// exactly one `EffectResolved` for staff and retiring revealed cards at the batch boundary.
#[test]
fn proteus_staff_paused_owner_order_still_settles_reorder() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let staff = scenario
        .add_artifact_from_oracle(P0, "Proteus Staff", PROTEUS_STAFF)
        .id();
    // Redirect the bounced creature out of the library so the rest pile starts as the whole library.
    let victim = scenario.add_creature(P0, "Redirected Victim", 2, 2).id();
    scenario
        .add_creature(P0, "Victim Redirect", 0, 1)
        .as_enchantment()
        .with_replacement_definition(library_redirect_for_card(
            "Victim redirect",
            Zone::Exile,
            victim,
        ));
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let kept = scenario.add_card_to_library_top(P0, "Kept Creature");
    let miss_b = scenario.add_card_to_library_top(P0, "Miss B");
    let miss_a = scenario.add_card_to_library_top(P0, "Miss A");

    // Two competing object-scoped redirects for the kept creature entering the battlefield:
    // Its delivery pauses on ReplacementChoice while the revealed misses stay revealed in the library.
    for label in ["Kept Redirect A", "Kept Redirect B"] {
        scenario
            .add_creature(P0, label, 0, 1)
            .as_enchantment()
            .with_replacement_definition(zone_redirect_for_card(
                label,
                Zone::Battlefield,
                Zone::Exile,
                kept,
            ));
    }
    let mut runner = scenario.build();
    configure_kept_creature(runner.state_mut(), kept);
    let incarnations_before = [
        runner.state().objects[&miss_a].incarnation,
        runner.state().objects[&miss_b].incarnation,
    ];

    runner
        .act(GameAction::ActivateAbility {
            source_id: staff,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Proteus Staff");

    // Pass priority to resolve the activation; victim is bounced to library (redirected to exile),
    // and reveal_until begins until kept creature is found.
    // Kept creature hits the two competing battlefield redirects and pauses on ReplacementChoice.
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner.act(GameAction::PassPriority).unwrap();
    }

    // CR 616.1: the kept creature's battlefield delivery surfaces the two competing redirects.
    // Both revealed misses are still revealed and still in the library when the prompt appears.
    {
        let state = runner.state();
        for (id, label) in [(miss_a, "Miss A"), (miss_b, "Miss B")] {
            assert_eq!(
                state.objects[&id].zone,
                Zone::Library,
                "{label} must still be in the library at the pause"
            );
            assert!(
                state.revealed_cards.contains(&id),
                "{label} must still be revealed at the pause"
            );
        }
        assert!(
            state.active_batch_delivery().is_some(),
            "the delivery batch must be active during the pause"
        );
    }

    // Answer the kept creature's replacement choice (redirect to Exile)
    let kept_res = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the kept creature replacement");

    // The batch resumes from the pause and transitions to RevealUntilBottomOrder
    let WaitingFor::RevealUntilBottomOrder { cards, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected RevealUntilBottomOrder after kept creature delivery, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(cards, vec![miss_a, miss_b]);

    // Submit the owner's bottom order (Miss B then Miss A)
    let mut all_events = kept_res.events;
    let order_res = runner
        .act(GameAction::SelectCards {
            cards: vec![miss_b, miss_a],
        })
        .expect("answer the owner's bottom order");
    all_events.extend(order_res.events.clone());

    // F2: Assert exactly one EffectResolved for RevealUntil and staff across the answer sequence
    assert_eq!(
        order_res
            .events
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::RevealUntil,
                    source_id,
                    ..
                } if *source_id == staff
            ))
            .count(),
        1,
        "owner-order answer must emit exactly one EffectResolved for staff RevealUntil"
    );
    assert_eq!(
        all_events
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::RevealUntil,
                    source_id,
                    ..
                } if *source_id == staff
            ))
            .count(),
        1,
        "full resume sequence must emit exactly one EffectResolved for staff RevealUntil"
    );

    let state = runner.state();
    assert_eq!(
        state.objects[&kept].zone,
        Zone::Exile,
        "kept creature took the answered redirect to exile"
    );
    assert_eq!(
        state.players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![deep, miss_b, miss_a],
        "both library-staying members keep the chosen bottom order"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the paused instruction settled back to priority, got {:?}",
        state.waiting_for
    );
    assert!(state.active_batch_delivery().is_none());
    assert!(state.stack.is_empty());

    // CR 701.20d + CR 616.1: terminal reorder retirement at the batch boundary
    // retires the entire arrangement atomically. Each card becomes exactly one new
    // object and stops being revealed.
    for (id, label, incarnation_before) in [
        (miss_a, "Miss A", incarnations_before[0]),
        (miss_b, "Miss B", incarnations_before[1]),
    ] {
        assert_eq!(
            state.objects[&id].incarnation,
            incarnation_before + 1,
            "{label} must become exactly one new object"
        );
        assert!(
            !state.revealed_cards.contains(&id) && !state.public_revealed_cards.contains(&id),
            "{label} must stop being revealed"
        );
    }
}
