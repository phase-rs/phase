//! Tests for Electrostatic Bolt:
//! "Electrostatic Bolt deals 2 damage to target creature. If it's an artifact creature,
//! Electrostatic Bolt deals 4 damage to it instead."
//!
//! CR 608.2c + CR 614.1a + CR 614.6:
//! An "instead" clause with target-anaphoric subject "it" referring to a declared target
//! lowers as a conditional sub-ability with `AbilityCondition::ConditionInstead`
//! wrapping `TargetMatchesFilter`. At runtime, `apply_instead_swap` checks the
//! target's characteristics: artifact creatures take 4 damage, non-artifact creatures take 2.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Effect, EffectKind, LibraryShuffleMode, PlayerFilter, PlayerRelation,
    PossessionAxis, TargetFilter, TargetRef, ThisWayCause, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::PlayerActionKind;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::{CoreType, GameEvent};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const ELECTROSTATIC_BOLT_ORACLE: &str = "Electrostatic Bolt deals 2 damage to target creature. \
    If it's an artifact creature, Electrostatic Bolt deals 4 damage to it instead.";

#[test]
fn electrostatic_bolt_parses_without_unimplemented() {
    let parsed = parse_oracle_text(
        ELECTROSTATIC_BOLT_ORACLE,
        "Electrostatic Bolt",
        &[],
        &["Instant".to_string()],
        &[],
    );

    // No unimplemented fallback nodes
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "must not contain Effect::Unimplemented in abilities"
    );

    assert_eq!(
        parsed.abilities.len(),
        1,
        "Electrostatic Bolt must lower to a single ability with an instead branch sub-ability"
    );

    let base = &parsed.abilities[0];
    assert!(
        matches!(&*base.effect, Effect::DealDamage { .. }),
        "base effect must be DealDamage, got {:?}",
        base.effect
    );

    let sub = base
        .sub_ability
        .as_ref()
        .expect("the 'deals 4 damage to it instead' must lower as a sub_ability");

    assert!(
        matches!(&*sub.effect, Effect::DealDamage { .. }),
        "sub-ability effect must be DealDamage, got {:?}",
        sub.effect
    );

    let cond = sub
        .condition
        .as_ref()
        .expect("sub-ability must have a condition");

    let AbilityCondition::ConditionInstead { inner } = cond else {
        panic!("expected ConditionInstead, got {cond:?}");
    };

    let AbilityCondition::TargetMatchesFilter {
        filter,
        use_lki,
        subject_slot,
    } = inner.as_ref()
    else {
        panic!("expected TargetMatchesFilter inside ConditionInstead, got {inner:?}");
    };

    assert!(!use_lki, "present-tense 'it's' must evaluate current state");
    assert_eq!(*subject_slot, None);

    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };

    assert!(
        tf.type_filters.contains(&TypeFilter::Artifact),
        "condition must filter for Artifact"
    );
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "condition must filter for Creature"
    );
}

#[test]
fn electrostatic_bolt_deals_2_damage_to_non_artifact_creature() {
    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P1, "Grizzly Bears", 2, 5).id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Electrostatic Bolt", true, ELECTROSTATIC_BOLT_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(bolt).target_object(target).resolve();

    assert_eq!(
        runner.state().objects[&target].damage_marked,
        2,
        "Electrostatic Bolt must deal 2 damage to non-artifact creature"
    );
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "5 toughness creature survives 2 damage"
    );
}

#[test]
fn electrostatic_bolt_deals_4_damage_to_artifact_creature() {
    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P1, "Steel Wall", 0, 5).id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Electrostatic Bolt", true, ELECTROSTATIC_BOLT_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let obj = runner.state_mut().objects.get_mut(&target).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types.core_types.push(CoreType::Artifact);

    runner.cast(bolt).target_object(target).resolve();

    assert_eq!(
        runner.state().objects[&target].damage_marked,
        4,
        "Electrostatic Bolt must deal 4 damage to artifact creature"
    );
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "5 toughness creature survives 4 damage"
    );
}

#[test]
fn electrostatic_bolt_destroys_4_toughness_artifact_creature_but_not_non_artifact() {
    let mut scenario = GameScenario::new();
    let non_artifact = scenario.add_creature(P1, "Tough Wall", 0, 4).id();
    let artifact = scenario.add_creature(P1, "Iron Wall", 0, 4).id();

    let bolt1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Electrostatic Bolt", true, ELECTROSTATIC_BOLT_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let bolt2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Electrostatic Bolt", true, ELECTROSTATIC_BOLT_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let obj = runner.state_mut().objects.get_mut(&artifact).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types.core_types.push(CoreType::Artifact);

    // Cast 1 on non-artifact 0/4: takes 2 damage, survives
    runner.cast(bolt1).target_object(non_artifact).resolve();
    assert_eq!(
        runner.state().objects[&non_artifact].zone,
        Zone::Battlefield,
        "0/4 non-artifact creature survives 2 damage"
    );
    assert_eq!(runner.state().objects[&non_artifact].damage_marked, 2);

    // Cast 2 on artifact 0/4: takes 4 damage, lethal damage moves it to graveyard
    runner.cast(bolt2).target_object(artifact).resolve();
    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Graveyard,
        "0/4 artifact creature is destroyed by lethal 4 damage"
    );
}

/// CR 608.2c + CR 109.2a + CR 110.4:
/// When an ability contains both a declared target and a revealed/exiled card,
/// conditions qualifying a non-battlefield "card" must retain `RevealedHasCardType`
/// and must never be misrouted to `TargetMatchesFilter`.
#[test]
fn target_plus_revealed_preserves_revealed_card_type() {
    // Hidetsugu and Kairi: exile top card + target opponent + "If it's an instant or sorcery card"
    let hk_parsed = parse_oracle_text(
        "When Hidetsugu and Kairi dies, exile the top card of your library. Target opponent loses \
         life equal to its mana value. If it's an instant or sorcery card, you may cast it without \
         paying its mana cost.",
        "Hidetsugu and Kairi",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let execute = hk_parsed.triggers[0]
        .execute
        .as_deref()
        .expect("trigger has execute");
    let lose_life = execute
        .sub_ability
        .as_deref()
        .expect("exile chains to life loss");
    let free_cast = lose_life
        .sub_ability
        .as_deref()
        .expect("life loss chains to free cast");
    assert_eq!(
        free_cast.condition,
        Some(AbilityCondition::RevealedHasCardType {
            card_types: vec![CoreType::Instant, CoreType::Sorcery],
            additional_filter: None,
            subtype_filter: None,
        }),
        "Hidetsugu and Kairi must retain RevealedHasCardType, not TargetMatchesFilter"
    );

    // Audacious Swap: verified Oracle text from Scryfall:
    // "Casualty 2\n\
    // The owner of target nonenchantment permanent shuffles it into their library, \
    // then exiles the top card of their library. If it's a land card, they put it \
    // onto the battlefield. Otherwise, they may cast it without paying its mana cost."
    let swap_parsed = parse_oracle_text(
        "Casualty 2\n\
         The owner of target nonenchantment permanent shuffles it into their library, \
         then exiles the top card of their library. If it's a land card, they put it \
         onto the battlefield. Otherwise, they may cast it without paying its mana cost.",
        "Audacious Swap",
        &[],
        &["Instant".to_string()],
        &[],
    );
    assert_eq!(swap_parsed.abilities.len(), 1);
    // CR 108.3 + CR 400.3 + CR 608.2c: the declared target is announced by the
    // root, the selected object moves into its owner's library, and the owner
    // population that move publishes (one member) shuffles and then exiles the
    // top card of ITS library, inside that owner's iteration.
    let target_only = &swap_parsed.abilities[0];
    let Effect::TargetOnly {
        target: ref target_filter,
    } = *target_only.effect
    else {
        panic!(
            "expected the declared-target root, got {:?}",
            target_only.effect
        );
    };
    assert!(
        matches!(target_filter, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Permanent)
            && tf.type_filters.contains(&TypeFilter::Non(Box::new(TypeFilter::Enchantment))))
    );

    let move_all = target_only
        .sub_ability
        .as_deref()
        .expect("target root chains to the owner-library move");
    assert!(matches!(
        *move_all.effect,
        Effect::ChangeZoneAll {
            destination: Zone::Library,
            target: TargetFilter::ParentTarget,
            library_shuffle: LibraryShuffleMode::TerminalShuffle,
            ..
        }
    ));

    let shuffle = move_all
        .sub_ability
        .as_deref()
        .expect("move chains to the terminal shuffle");
    assert!(matches!(
        *shuffle.effect,
        Effect::Shuffle {
            target: TargetFilter::ScopedPlayer
        }
    ));
    assert!(matches!(
        shuffle.player_scope,
        Some(PlayerFilter::TrackedSetPossessor {
            relation: PlayerRelation::All,
            possession: PossessionAxis::Owner,
            filter: TargetFilter::Any,
            caused_by: Some(ThisWayCause::OwnerLibraryShuffleSubject),
        })
    ));

    let exile_top = shuffle
        .sub_ability
        .as_deref()
        .expect("shuffle chains to exile top card");
    assert!(matches!(
        *exile_top.effect,
        Effect::ExileTop {
            player: TargetFilter::ScopedPlayer,
            ..
        }
    ));

    let land_branch = exile_top
        .sub_ability
        .as_deref()
        .expect("exile top chains to land branch");
    assert!(matches!(
        *land_branch.effect,
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            ..
        }
    ));
    assert_eq!(
        land_branch.condition,
        Some(AbilityCondition::RevealedHasCardType {
            card_types: vec![CoreType::Land],
            additional_filter: None,
            subtype_filter: None,
        }),
        "CR 109.2 + CR 608.2c: 'If it\\'s a land card' must retain RevealedHasCardType, NOT TargetMatchesFilter"
    );

    let otherwise_cast = land_branch
        .else_ability
        .as_deref()
        .expect("land branch has Otherwise cast else-ability");
    assert!(
        matches!(
            *otherwise_cast.effect,
            Effect::CastFromZone {
                without_paying_mana_cost: true,
                ..
            }
        ),
        "Otherwise branch must cast from zone without paying mana cost"
    );
}

/// Audacious Swap's verbatim Oracle text (AtomicCards.json), including the
/// casualty reminder text.
const AUDACIOUS_SWAP: &str = "Casualty 2 (As you cast this spell, you may sacrifice a creature with power 2 or greater. When you do, copy this spell and you may choose a new target for the copy.)\n\
    The owner of target nonenchantment permanent shuffles it into their library, \
    then exiles the top card of their library. If it's a land card, they put it \
    onto the battlefield. Otherwise, they may cast it without paying its mana cost.";

/// Finish a cast that stopped at its optional prompt: answer it, then pass
/// priority until the stack empties, returning every event the answers emitted.
fn answer_optional_and_settle(runner: &mut GameRunner, accept: bool) -> Vec<GameEvent> {
    let mut events = runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("the optional decision is accepted")
        .events;
    for _ in 0..6 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("priority passes while the cast spell resolves")
                .events,
        );
    }
    events
}

/// The `object_id` of every spell cast in `events`.
fn cast_spells(events: &[GameEvent]) -> Vec<(ObjectId, PlayerId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::SpellCast {
                object_id,
                controller,
                ..
            } => Some((*object_id, *controller)),
            _ => None,
        })
        .collect()
}

/// End-to-end runtime test for Audacious Swap using verified Oracle text:
/// "Casualty 2 (As you cast this spell, you may sacrifice a creature with power
/// 2 or greater. When you do, copy this spell and you may choose a new target
/// for the copy.)
/// The owner of target nonenchantment permanent shuffles it into their library, \
/// then exiles the top card of their library. If it's a land card, they put it \
/// onto the battlefield. Otherwise, they may cast it without paying its mana cost."
///
/// Distinguishes the two branches:
/// 1. Nonland card (Target Bear): Target nonenchantment permanent is shuffled into
///    owner's library; top card is exiled; nonland fails the land gate so the 'otherwise'
///    free-cast branch executes and casts Target Bear from exile.
/// 2. Land card (Dryad Arbor): Target nonenchantment permanent is shuffled into
///    owner's library; top card is exiled; land satisfies the land gate and is put onto
///    the battlefield directly without offering a cast.
/// 3. Staged library (P1's library holds lands besides the shuffled target): the
///    exiled card is a land that is NOT the declared target, and the land branch
///    puts THAT card onto the battlefield.
///
/// CR 108.3 + CR 608.2c + CR 601.2a + CR 110.2: the owner (P1), not the caster
/// (P0), is the player who "may cast it", and the card enters under the owner's
/// control.
#[test]
fn audacious_swap_runtime_distinguishes_nonland_from_land() {
    // Case 1: Nonland card (Target Bear).
    // - P1's target nonland creature is shuffled into P1's library.
    // - P1 exiles top card (Target Bear).
    // - Fails "If it's a land card", so "Otherwise, they may cast it..." executes.
    // - Target Bear is cast from exile onto the stack and resolves to the battlefield.
    {
        let mut scenario = GameScenario::new();
        let target = scenario.add_creature(P1, "Target Bear", 2, 2).id();
        let swap = scenario
            .add_spell_to_hand_from_oracle(P0, "Audacious Swap", true, AUDACIOUS_SWAP)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        let stopped = runner
            .cast(swap)
            .target_object(target)
            .commit()
            .stop_at_optional()
            .resolve();

        // CR 608.2d + CR 108.3: the "they may cast it" decision belongs to the
        // card's owner (P1), not to Audacious Swap's caster (P0).
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "the optional cast prompt is reached: {:?}",
            runner.state().waiting_for
        );
        assert_eq!(
            runner.state().waiting_for.acting_player(),
            Some(P1),
            "the owner (P1) decides whether to cast the exiled card"
        );
        let mut events = stopped.events().to_vec();
        events.extend(answer_optional_and_settle(&mut runner, true));

        // 1. Target nonenchantment permanent shuffled into owner's library.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == P1
            )),
            "P1's library must have been shuffled"
        );

        // 2. Nonland card (Target Bear) is exiled from library and cast via Otherwise branch.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ExileTop,
                    ..
                }
            )),
            "ExileTop effect must resolve"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::CastFromZone,
                    ..
                }
            )),
            "CastFromZone effect must resolve for nonland card"
        );
        assert!(
            cast_spells(&events).contains(&(target, P1)),
            "Target Bear must be cast from exile, by its owner P1: {:?}",
            cast_spells(&events)
        );
        assert_eq!(
            runner.state().objects[&target].zone,
            Zone::Battlefield,
            "Target Bear resolves from the stack and enters the battlefield"
        );
        assert_eq!(
            runner.state().objects[&target].controller,
            P1,
            "the spell the owner cast enters under the owner's control"
        );
        assert_eq!(
            &runner.state().waiting_for,
            &WaitingFor::Priority { player: P0 },
            "priority returns to active player after resolution"
        );
    }

    // Case 2: Land card (Dryad Arbor - land creature).
    // - P1's target land permanent is shuffled into P1's library.
    // - P1 exiles top card (Dryad Arbor).
    // - Satisfies "If it's a land card", so P1 puts Dryad Arbor onto the battlefield.
    // - No cast is offered; resolution completes cleanly.
    {
        let mut scenario = GameScenario::new();
        let land_target = scenario.add_creature(P1, "Dryad Arbor", 1, 1).id();
        let swap = scenario
            .add_spell_to_hand_from_oracle(P0, "Audacious Swap", true, AUDACIOUS_SWAP)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();

        let obj = runner.state_mut().objects.get_mut(&land_target).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types.core_types.push(CoreType::Land);

        let outcome = runner
            .cast(swap)
            .target_object(land_target)
            .commit()
            .accept_optional()
            .resolve();

        // 1. Target nonenchantment permanent shuffled into owner's library.
        assert!(
            outcome.events().iter().any(|e| matches!(
                e,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == P1
            )),
            "P1's library must have been shuffled"
        );

        // 2. Land card is put directly onto the battlefield.
        assert_eq!(
            outcome.zone_of(land_target),
            Zone::Battlefield,
            "land card satisfies 'If it\\'s a land card' and enters the battlefield"
        );
        assert_eq!(
            outcome.state().objects[&land_target].controller,
            P1,
            "CR 108.3 + CR 110.2: the owner puts the land onto the battlefield under their control"
        );
        assert!(
            !outcome.events().iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::CastFromZone,
                    ..
                }
            )),
            "CastFromZone must not resolve for land card"
        );
        assert!(
            !outcome.events().iter().any(|e| matches!(
                e,
                GameEvent::SpellCast { object_id, .. } if *object_id == land_target
            )),
            "Dryad Arbor must not be cast (put directly onto battlefield)"
        );
        assert_eq!(
            outcome.final_waiting_for(),
            &WaitingFor::Priority { player: P0 },
            "resolution finishes and priority returns to active player"
        );
    }

    // Case 3: staged library. P1's library holds lands besides the shuffled
    // nonland target, so the exiled top card is a land that is not the
    // declared target (CR 701.13a: "it" is the exiled card, CR 608.2c).
    // - The land branch must put the EXILED land onto the battlefield under P1.
    // - The declared target stays in P1's library.
    // - Nothing is cast: the land satisfies "If it's a land card".
    //
    // Staged-land count: `STAGED_LANDS` (five). With four lands the
    // deterministic seed-42 shuffle left the target on top and the reach guard
    // `exiled != target` below failed; the count was raised one at a time and
    // five is the first that holds. The guard is never weakened.
    {
        let mut scenario = GameScenario::new();
        let target = scenario.add_creature(P1, "Target Bear", 2, 2).id();
        for _ in 0..STAGED_LANDS {
            scenario.add_land_to_library_top(P1, "Forest");
        }
        let swap = scenario
            .add_spell_to_hand_from_oracle(P0, "Audacious Swap", true, AUDACIOUS_SWAP)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        let outcome = runner
            .cast(swap)
            .target_object(target)
            .commit()
            .accept_optional()
            .resolve();

        let exiled_from_library: Vec<ObjectId> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    to: Zone::Exile,
                    ..
                } if outcome.state().objects[object_id].owner == P1 => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            exiled_from_library.len(),
            1,
            "exactly one card leaves P1's library for exile: {exiled_from_library:?}"
        );
        let exiled = exiled_from_library[0];

        // Reach guard: the exiled card is a staged land, not the declared target.
        assert_ne!(
            exiled, target,
            "the staged library leaves a land on top after the shuffle"
        );
        assert!(outcome.state().objects[&exiled]
            .card_types
            .core_types
            .contains(&CoreType::Land));

        // CR 701.13a + CR 608.2c + CR 110.2: the exiled land enters under its
        // owner's control; the declared target is untouched in P1's library.
        assert_eq!(outcome.zone_of(exiled), Zone::Battlefield);
        assert_eq!(outcome.state().objects[&exiled].controller, P1);
        assert_eq!(outcome.zone_of(target), Zone::Library);
        assert!(outcome.state().players[1].library.contains(&target));

        // Paired with the positive battlefield arrival above: neither the exiled
        // land nor the target is cast (Audacious Swap's own cast is expected).
        let cast = cast_spells(outcome.events());
        assert!(
            cast.iter()
                .all(|(object_id, _)| *object_id != exiled && *object_id != target),
            "neither the exiled land nor the declared target is cast: {cast:?}"
        );
        assert_eq!(
            outcome.final_waiting_for(),
            &WaitingFor::Priority { player: P0 }
        );
        assert!(outcome.state().stack.is_empty());
    }
}

/// Staged lands in P1's library for Case 3 of the Audacious Swap runtime test
/// (see the count record there).
const STAGED_LANDS: usize = 5;

/// The players whose library was shuffled, in order, consecutive repeats
/// collapsed. A move into a library followed by the printed "shuffle" step
/// emits one `ShuffledLibrary` event per step, so this reads WHOSE library was
/// shuffled (what the owner binding discriminates), not how many events fired.
fn shuffled_libraries(events: &[GameEvent]) -> Vec<PlayerId> {
    let mut players: Vec<PlayerId> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    players.dedup();
    players
}

/// CR 608.2d + CR 108.3: Audacious Swap's "they may cast it without
/// paying its mana cost" is the OWNER's decision. When the owner (P1, not the
/// caster P0) declines, the exiled card stays in exile and is never cast; the
/// shuffle and exile-top still happened in the owner's library. The prompt-actor
/// assertion (`Some(P1)`) fails if the optional were asked of the caster.
#[test]
fn audacious_swap_owner_declining_the_free_cast_leaves_the_card_exiled() {
    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    let swap = scenario
        .add_spell_to_hand_from_oracle(P0, "Audacious Swap", true, AUDACIOUS_SWAP)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let stopped = runner
        .cast(swap)
        .target_object(target)
        .commit()
        .stop_at_optional()
        .resolve();

    // Reach guard: the optional cast prompt was reached, and it is the owner's.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the optional cast prompt is reached: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().waiting_for.acting_player(),
        Some(P1),
        "the owner (P1), not the caster (P0), decides whether to cast the exiled card"
    );
    let mut events = stopped.events().to_vec();
    events.extend(answer_optional_and_settle(&mut runner, false));

    // Reach guards: the target was shuffled into P1's library (and only P1's),
    // then left that library for exile as the exiled top card.
    assert_eq!(
        shuffled_libraries(&events),
        vec![P1],
        "only the owner's library is shuffled"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Library,
                ..
            } if *object_id == target
        )),
        "the target was shuffled into its owner's library"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Exile,
                ..
            } if *object_id == target
        )),
        "the target is the top card of P1's library after the shuffle, and is exiled"
    );

    // Declined: the exiled card stays exiled, uncast; Audacious Swap's own cast
    // is the only spell cast.
    assert_eq!(runner.state().objects[&target].zone, Zone::Exile);
    assert!(!runner.state().players[1].library.contains(&target));
    assert_eq!(
        cast_spells(&events),
        vec![(swap, P0)],
        "only Audacious Swap itself was cast; the exiled card was not"
    );
    assert_eq!(
        &runner.state().waiting_for,
        &WaitingFor::Priority { player: P0 }
    );
    assert!(runner.state().stack.is_empty());
}

/// CR 702.153a + CR 707.10 + CR 707.10c + CR 108.3: Audacious Swap with a paid
/// casualty 2 copies the spell when the casualty trigger resolves, and the
/// controller chooses a new target for the copy. In a three-player game the
/// original's target is owned by P1 and the copy's new target by P2, so each
/// resolution must shuffle and exile-top inside ITS OWN target's owner's
/// library: the owner is derived from each spell's own target, never from the
/// caster (P0) nor from the other spell's target. The copy is not cast
/// (CR 707.10: a copy of a spell is not cast).
#[test]
fn audacious_swap_casualty_copy_resolves_each_target_through_its_own_owner() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let original_target = scenario.add_creature(P1, "Original Target", 2, 2).id();
    let copy_target = scenario.add_creature(P2, "Copy Target", 2, 2).id();
    let fodder = scenario.add_creature(P0, "Casualty Fodder", 2, 2).id();
    let swap = scenario
        .add_spell_to_hand_from_oracle(P0, "Audacious Swap", true, AUDACIOUS_SWAP)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&original_target].owner, P1);
    assert_eq!(runner.state().objects[&copy_target].owner, P2);

    let committed = runner
        .cast(swap)
        .target_object(original_target)
        .sacrifice_with(&[fodder])
        .accept_optional()
        .commit()
        .stop_at_optional()
        .resolve();
    let mut events = committed.events().to_vec();

    // Drive every prompt through public actions, recording who is asked what.
    let mut copy_id = None;
    let mut optional_deciders = vec![];
    let mut max_stack = 0;
    for _ in 0..40 {
        max_stack = max_stack.max(runner.state().stack.len());
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::CopyRetarget {
                player,
                copy_id: id,
                target_slots,
                ..
            } => {
                // CR 707.10c: the copy's controller (P0) chooses the new target.
                assert_eq!(player, P0);
                assert_eq!(target_slots.len(), 1);
                assert_eq!(
                    target_slots[0].current,
                    Some(TargetRef::Object(original_target)),
                    "the copy starts with the original's target"
                );
                assert!(target_slots[0]
                    .legal_alternatives
                    .contains(&TargetRef::Object(copy_target)));
                copy_id = Some(id);
                runner.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(copy_target)),
                })
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                optional_deciders.push(player);
                runner.act(GameAction::DecideOptionalEffect { accept: false })
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            other => panic!("unexpected prompt while resolving the casualty copy: {other:?}"),
        };
        events.extend(result.expect("prompt answer accepted").events);
    }
    let copy_id =
        copy_id.expect("the paid casualty copied Audacious Swap and offered a new target");

    // Reach guards: casualty was paid (fodder sacrificed), the spell was copied,
    // and both the original and the copy were on the stack at once.
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::SpellCopied { original_id, .. } if *original_id == swap
        )),
        "Audacious Swap was copied"
    );
    assert!(max_stack >= 2, "original and copy were stacked together");

    // CR 707.10: only the original is cast; the copy is not.
    let cast = cast_spells(&events);
    assert!(
        cast.contains(&(swap, P0)),
        "reach guard: the original Audacious Swap was cast: {cast:?}"
    );
    assert!(
        cast.iter().all(|(object_id, _)| *object_id != copy_id),
        "the copy is not cast: {cast:?}"
    );

    // The copy resolves first (top of the stack): its target's owner (P2) is
    // asked and shuffles first, then the original's target's owner (P1).
    assert_eq!(
        optional_deciders,
        vec![P2, P1],
        "each resolution asks the owner of that spell's own target"
    );
    assert_eq!(shuffled_libraries(&events), vec![P2, P1]);

    // Each target went to its OWN owner's library and was exiled from there;
    // declined, so neither is cast.
    for target in [original_target, copy_target] {
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    to: Zone::Exile,
                    ..
                } if *object_id == target
            )),
            "{target:?} was exiled from its owner's library"
        );
        assert_eq!(runner.state().objects[&target].zone, Zone::Exile);
    }
    assert!(runner.state().players[0].library.is_empty());
    assert!(runner.state().players[1].library.is_empty());
    assert!(runner.state().players[2].library.is_empty());
    assert_eq!(
        &runner.state().waiting_for,
        &WaitingFor::Priority { player: P0 }
    );
    assert!(runner.state().stack.is_empty());
}
