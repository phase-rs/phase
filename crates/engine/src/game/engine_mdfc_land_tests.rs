use super::*;
use crate::game::game_object::BackFaceData;
use crate::game::zones::create_object;
use crate::types::card::LayoutKind;
use crate::types::card_type::{CardType, CoreType};
use crate::types::format::FormatConfig;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaCost;

fn setup_game_at_main_phase() -> GameState {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

fn make_land_type() -> CardType {
    CardType {
        supertypes: vec![],
        core_types: vec![CoreType::Land],
        subtypes: vec![],
    }
}

fn make_creature_type() -> CardType {
    CardType {
        supertypes: vec![],
        core_types: vec![CoreType::Creature],
        subtypes: vec![],
    }
}

fn make_back_face(
    name: &str,
    card_types: CardType,
    layout_kind: Option<LayoutKind>,
) -> BackFaceData {
    BackFaceData {
        name: name.to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        card_types,
        mana_cost: ManaCost::default(),
        keywords: Vec::new(),
        abilities: Vec::new(),
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: Vec::new(),
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind,
    }
}

/// Create an MDFC in hand with the given front and back card types.
fn create_mdfc_in_hand(
    state: &mut GameState,
    front_name: &str,
    front_types: CardType,
    back_name: &str,
    back_types: CardType,
) -> (ObjectId, CardId) {
    let obj_id = create_object(
        state,
        CardId(100),
        PlayerId(0),
        front_name.to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.card_types = front_types;
    obj.back_face = Some(make_back_face(
        back_name,
        back_types,
        Some(LayoutKind::Modal),
    ));
    (obj_id, CardId(100))
}

// CR 712.12: MDFC Land/Land should return ModalFaceChoice
#[test]
fn mdfc_land_land_returns_modal_face_choice() {
    let mut state = setup_game_at_main_phase();
    let (obj_id, card_id) = create_mdfc_in_hand(
        &mut state,
        "Branchloft Pathway",
        make_land_type(),
        "Boulderloft Pathway",
        make_land_type(),
    );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id,
        },
    )
    .unwrap();

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ModalFaceChoice {
                player: PlayerId(0),
                ..
            }
        ),
        "Expected ModalFaceChoice, got {:?}",
        result.waiting_for
    );
}

// CR 712.12: Choosing back face enters with back-face characteristics
#[test]
fn mdfc_choose_back_face_enters_with_back_characteristics() {
    let mut state = setup_game_at_main_phase();
    let (obj_id, card_id) = create_mdfc_in_hand(
        &mut state,
        "Branchloft Pathway",
        make_land_type(),
        "Boulderloft Pathway",
        make_land_type(),
    );

    // Trigger ModalFaceChoice
    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ModalFaceChoice { .. }
    ));

    // Choose back face
    let result =
        apply_as_current(&mut state, GameAction::ChooseModalFace { back_face: true }).unwrap();

    // Should return to priority (not another ModalFaceChoice)
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority after face choice, got {:?}",
        result.waiting_for
    );

    // Object should be on battlefield with back-face name
    let obj = state.objects.get(&obj_id).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, "Boulderloft Pathway");
    assert!(
        !obj.transformed,
        "MDFC face choice must not set transformed"
    );
}

// CR 712.12: Choosing front face enters normally
#[test]
fn mdfc_choose_front_face_enters_normally() {
    let mut state = setup_game_at_main_phase();
    let (obj_id, card_id) = create_mdfc_in_hand(
        &mut state,
        "Branchloft Pathway",
        make_land_type(),
        "Boulderloft Pathway",
        make_land_type(),
    );

    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id,
        },
    )
    .unwrap();

    let result =
        apply_as_current(&mut state, GameAction::ChooseModalFace { back_face: false }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    let obj = state.objects.get(&obj_id).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, "Branchloft Pathway");
}

// CR 712.12: MDFC Creature/Land auto-swaps to land face without choice dialog
#[test]
fn mdfc_creature_land_auto_swaps_to_land_face() {
    let mut state = setup_game_at_main_phase();
    let (obj_id, card_id) = create_mdfc_in_hand(
        &mut state,
        "Kazandu Mammoth",
        make_creature_type(),
        "Kazandu Valley",
        make_land_type(),
    );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id,
        },
    )
    .unwrap();

    // Should go directly to Priority (no ModalFaceChoice)
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority (auto-swap), got {:?}",
        result.waiting_for
    );

    // Object enters with back-face (land) characteristics
    let obj = state.objects.get(&obj_id).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, "Kazandu Valley");
    assert!(!obj.transformed);
}

// CR 712.12: MDFC Land/Creature plays front face normally, no choice needed
#[test]
fn mdfc_land_creature_plays_front_face_normally() {
    let mut state = setup_game_at_main_phase();
    let (obj_id, card_id) = create_mdfc_in_hand(
        &mut state,
        "Hagra Mauling",
        make_land_type(),
        "Hagra Broodpit",
        make_creature_type(),
    );
    // Set layout_kind on back face to Modal
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        if let Some(ref mut bf) = obj.back_face {
            bf.layout_kind = Some(LayoutKind::Modal);
        }
    }

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id,
        },
    )
    .unwrap();

    // Should go directly to Priority (front is Land, back is Creature, no choice)
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority, got {:?}",
        result.waiting_for
    );
    let obj = state.objects.get(&obj_id).unwrap();
    assert_eq!(obj.name, "Hagra Mauling");
}

// Transform DFC with Land back should NOT trigger ModalFaceChoice
#[test]
fn transform_dfc_land_back_no_modal_face_choice() {
    let mut state = setup_game_at_main_phase();
    let obj_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Westvale Abbey".to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.card_types = make_land_type();
    obj.back_face = Some(make_back_face(
        "Ormendahl",
        make_land_type(),
        Some(LayoutKind::Transform), // Transform, not Modal
    ));

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id: CardId(200),
        },
    )
    .unwrap();

    // Should NOT produce ModalFaceChoice — only Modal layout triggers it
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Transform DFC should not trigger ModalFaceChoice, got {:?}",
        result.waiting_for
    );
}

// AI candidates: both ChooseModalFace options generated for ModalFaceChoice
#[test]
fn ai_generates_both_modal_face_candidates() {
    let mut state = setup_game_at_main_phase();
    let (obj_id, card_id) = create_mdfc_in_hand(
        &mut state,
        "Branchloft Pathway",
        make_land_type(),
        "Boulderloft Pathway",
        make_land_type(),
    );

    // Trigger ModalFaceChoice via PlayLand
    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ModalFaceChoice { .. }
    ));

    let candidates = crate::ai_support::legal_actions(&state);
    let modal_actions: Vec<_> = candidates
        .iter()
        .filter(|c| matches!(c, GameAction::ChooseModalFace { .. }))
        .collect();

    assert_eq!(
        modal_actions.len(),
        2,
        "Expected 2 ChooseModalFace candidates"
    );
}

// CR 712.11b + CR 903.8: A spell//spell Modal DFC commander (Esika, God of
// the Tree // The Prismatic Bridge) cast from the command zone must offer the
// face choice so the player can put either face on the stack (#1548). The
// choice was previously gated to the hand, so only the front face was
// castable from the command zone.
#[test]
fn mdfc_commander_cast_from_command_zone_offers_face_choice() {
    let mut state = setup_game_at_main_phase();
    state.format_config.command_zone = true;
    let obj_id = create_object(
        &mut state,
        CardId(100),
        PlayerId(0),
        "Esika, God of the Tree".to_string(),
        Zone::Command,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.is_commander = true;
        obj.card_types = make_creature_type();
        obj.back_face = Some(make_back_face(
            "The Prismatic Bridge",
            CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Enchantment],
                subtypes: vec![],
            },
            Some(LayoutKind::Modal),
        ));
    }

    let cast_actions = crate::ai_support::legal_actions(&state)
            .iter()
            .filter(|action| {
                matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == obj_id)
            })
            .count();
    assert_eq!(
        cast_actions, 1,
        "the MDFC commander must be offered as castable from the command zone"
    );

    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: obj_id,
            card_id: CardId(100),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert!(
        matches!(result.waiting_for, WaitingFor::ModalFaceChoice { .. }),
        "spell//spell MDFC commander cast from the command zone must offer \
             ModalFaceChoice, got {:?}",
        result.waiting_for
    );

    // Both faces must be offered (front: Esika; back: The Prismatic Bridge).
    let candidates = crate::ai_support::legal_actions(&state);
    let modal_actions = candidates
        .iter()
        .filter(|c| matches!(c, GameAction::ChooseModalFace { .. }))
        .count();
    assert_eq!(
        modal_actions, 2,
        "both MDFC commander faces must be offered from the command zone"
    );
}

// CR 712.8a: MDFC Creature/Land in graveyard — front face only, NOT a land
#[test]
fn mdfc_creature_land_in_graveyard_not_offered_as_land() {
    let mut state = setup_game_at_main_phase();
    let obj_id = create_object(
        &mut state,
        CardId(300),
        PlayerId(0),
        "Kazandu Mammoth".to_string(),
        Zone::Graveyard,
    );
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.card_types = make_creature_type();
    obj.back_face = Some(make_back_face(
        "Kazandu Valley",
        make_land_type(),
        Some(LayoutKind::Modal),
    ));

    let candidates = crate::ai_support::legal_actions(&state);
    let land_actions: Vec<_> = candidates
        .iter()
        .filter(|c| matches!(c, GameAction::PlayLand { object_id, .. } if *object_id == obj_id))
        .collect();

    assert!(
        land_actions.is_empty(),
        "CR 712.8a: MDFC Creature/Land in graveyard should not be offered as PlayLand"
    );
}

/// Build a spell//spell Modal DFC (Esika, God of the Tree //
/// The Prismatic Bridge) in hand with explicit, asymmetric mana costs.
fn create_spell_mdfc_in_hand(state: &mut GameState) -> (ObjectId, CardId) {
    use crate::types::mana::ManaCostShard;
    let obj_id = create_object(
        state,
        CardId(400),
        PlayerId(0),
        "Esika, God of the Tree".to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.card_types = make_creature_type();
    // Front: {1}{G}{G}
    obj.mana_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        generic: 1,
    };
    let mut back = make_back_face(
        "The Prismatic Bridge",
        CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Enchantment],
            subtypes: vec![],
        },
        Some(LayoutKind::Modal),
    );
    // Back: {W}{U}{B}{R}{G}
    back.mana_cost = ManaCost::Cost {
        shards: vec![
            ManaCostShard::White,
            ManaCostShard::Blue,
            ManaCostShard::Black,
            ManaCostShard::Red,
            ManaCostShard::Green,
        ],
        generic: 0,
    };
    obj.back_face = Some(back);
    (obj_id, CardId(400))
}

/// Add one mana of each given color to the player's pool.
fn add_pool_mana(state: &mut GameState, player: PlayerId, colors: &[crate::types::mana::ManaType]) {
    use crate::types::mana::ManaUnit;
    let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for &color in colors {
        p.mana_pool.add(ManaUnit {
            color,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }
}

// CR 712.11c: A spell//spell MDFC is castable when only the *back* face is
// affordable — only the face that will be on the stack is evaluated for
// castability (front Esika needs {1}{G}{G}; back Prismatic Bridge needs
// {W}{U}{B}{R}{G}). The user's bug: with W/U/B/R/G in pool the front is
// unaffordable, so the card was dropping out of legal actions entirely.
#[test]
fn spell_mdfc_castable_when_only_back_face_affordable() {
    use crate::types::mana::ManaType;
    let mut state = setup_game_at_main_phase();
    let (obj_id, _card_id) = create_spell_mdfc_in_hand(&mut state);
    add_pool_mana(
        &mut state,
        PlayerId(0),
        &[
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ],
    );

    assert!(
        crate::game::casting::can_cast_object_now(&state, PlayerId(0), obj_id),
        "Spell MDFC must be castable when only the back face is affordable"
    );

    let candidates = crate::ai_support::legal_actions(&state);
    assert!(
        candidates.iter().any(|c| matches!(
            c,
            GameAction::CastSpell { object_id, .. } if *object_id == obj_id
        )),
        "Expected a CastSpell candidate for the spell MDFC"
    );
}

// CR 712.11b: Casting a spell//spell MDFC prompts a face choice, and choosing
// the back face puts the back-face spell on the stack.
#[test]
fn spell_mdfc_cast_back_face_goes_on_stack() {
    use crate::types::mana::ManaType;
    let mut state = setup_game_at_main_phase();
    let (obj_id, card_id) = create_spell_mdfc_in_hand(&mut state);
    add_pool_mana(
        &mut state,
        PlayerId(0),
        &[
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ],
    );

    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ModalFaceChoice {
                player: PlayerId(0),
                ..
            }
        ),
        "Casting a spell MDFC should prompt ModalFaceChoice, got {:?}",
        result.waiting_for
    );

    let result =
        apply_as_current(&mut state, GameAction::ChooseModalFace { back_face: true }).unwrap();
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority after casting the back face, got {:?}",
        result.waiting_for
    );

    // The back-face spell is on the stack; the object left the hand.
    let on_stack = state.stack.iter().any(|e| e.id == obj_id);
    assert!(on_stack, "back-face spell should be on the stack");
    let obj = state.objects.get(&obj_id).unwrap();
    assert_eq!(obj.name, "The Prismatic Bridge");
    assert!(
        !obj.transformed,
        "MDFC face choice must not set transformed"
    );
}

/// Engine-level defense-in-depth: a non-host actor must not be able to
/// grant debug permission, even when sandbox mode is enabled. server-core
/// also checks this at the transport boundary; this test pins the
/// engine-side guard so WASM/P2P-host adapters cannot be bypassed by
/// crafting the action shape directly.
#[test]
fn grant_debug_permission_rejected_for_non_host() {
    let mut state = GameState::new(
        crate::types::format::FormatConfig::standard().with_sandbox(),
        2,
        42,
    );
    let err = apply(
        &mut state,
        PlayerId(1),
        GameAction::GrantDebugPermission {
            player_id: PlayerId(1),
        },
    )
    .expect_err("non-host Grant must be rejected");
    assert!(
        matches!(err, EngineError::ActionNotAllowed(_)),
        "got {:?}",
        err
    );
    assert!(
        !state.debug_permitted.contains(&PlayerId(1)),
        "permission must not have been mutated on rejection"
    );
}

/// Engine-level defense-in-depth: Grant/Revoke is rejected outright when
/// the format does not have `allow_debug_actions` set. Closes the WASM /
/// P2P-host path that previously skipped this check.
#[test]
fn grant_debug_permission_rejected_when_sandbox_disabled() {
    let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
    let err = apply(
        &mut state,
        PlayerId(0),
        GameAction::GrantDebugPermission {
            player_id: PlayerId(1),
        },
    )
    .expect_err("Grant must be rejected when sandbox is disabled");
    assert!(
        matches!(err, EngineError::ActionNotAllowed(_)),
        "got {:?}",
        err
    );
}

/// Engine-level: the host may grant; afterwards the granted player can
/// submit a Debug action that the engine accepts.
#[test]
fn grant_debug_permission_succeeds_for_host_and_unlocks_debug() {
    let mut state = GameState::new(
        crate::types::format::FormatConfig::standard().with_sandbox(),
        2,
        42,
    );
    state.debug_mode = true;
    // Host (PlayerId(0)) is implicitly authorized; seed empty set first.
    state.debug_permitted.clear();

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::GrantDebugPermission {
            player_id: PlayerId(1),
        },
    )
    .expect("host Grant should succeed");
    assert!(state.debug_permitted.contains(&PlayerId(1)));
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, GameEvent::DebugPermissionGranted { .. })));

    // Post-grant: the granted player can now submit a Debug action that
    // the engine accepts. Use `ShuffleLibrary` — a side-effect-light op
    // that doesn't require pre-existing objects.
    let debug_result = apply(
        &mut state,
        PlayerId(1),
        GameAction::Debug(crate::types::actions::DebugAction::ShuffleLibrary {
            player_id: PlayerId(1),
        }),
    )
    .expect("granted player's Debug action should succeed");
    assert!(debug_result
        .events
        .iter()
        .any(|e| matches!(e, GameEvent::DebugActionUsed { .. })));
}

/// Engine-level: the host may not revoke their own permission — that
/// would leave nobody able to act in sandbox.
#[test]
fn revoke_debug_permission_rejects_host_self_revoke() {
    let mut state = GameState::new(
        crate::types::format::FormatConfig::standard().with_sandbox(),
        2,
        42,
    );
    state.debug_permitted.insert(PlayerId(0));
    let err = apply(
        &mut state,
        PlayerId(0),
        GameAction::RevokeDebugPermission {
            player_id: PlayerId(0),
        },
    )
    .expect_err("host self-revoke must be rejected");
    assert!(
        matches!(err, EngineError::ActionNotAllowed(_)),
        "got {:?}",
        err
    );
    assert!(
        state.debug_permitted.contains(&PlayerId(0)),
        "host permission must remain on rejection"
    );
}

// --- First-player d20 contest (start_game) -------------------------------

/// Extract the single `StartingPlayerContest` event's (rounds, winner) from
/// an ActionResult. Panics if absent or duplicated — the contest path emits
/// exactly one such event.
fn contest_event(result: &ActionResult) -> (Vec<ContestRound>, PlayerId) {
    let mut found = result.events.iter().filter_map(|e| match e {
        GameEvent::StartingPlayerContest { rounds, winner } => Some((rounds.clone(), *winner)),
        _ => None,
    });
    let event = found.next().expect("a StartingPlayerContest event");
    assert!(
        found.next().is_none(),
        "exactly one StartingPlayerContest event"
    );
    event
}

/// CR 103.1 / CR 706: a seeded contest with no tie emits a single round
/// with one d20 per seat and the high roller becomes the starting player.
#[test]
fn start_game_contest_emits_d20_per_seat_and_picks_high_roller() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 7);
    let result = start_game(&mut state);
    let (rounds, winner) = contest_event(&result);

    // No tie at this seed → exactly one round, one roll per seat.
    assert_eq!(rounds.len(), 1, "no tie → single round");
    let rolls = &rounds[0].rolls;
    assert_eq!(rolls.len(), 2, "one roll per seat");
    assert_ne!(rolls[0].1, rolls[1].1, "seed 7 should not tie");
    let max_roll = rolls.iter().map(|&(_, r)| r).max().unwrap();
    // The winner is the seat that rolled the max.
    let argmax = rolls.iter().find(|&&(_, r)| r == max_roll).unwrap().0;
    assert_eq!(winner, argmax, "winner == argmax of the round");
    assert_eq!(
        state.current_starting_player, winner,
        "high roller becomes the starting player"
    );
    // All d20 rolls are in range.
    assert!(rolls.iter().all(|&(_, r)| (1..=20).contains(&r)));
}

/// Event sequencing: the single `StartingPlayerContest` precedes
/// `GameStarted`, which precedes `TurnStarted`.
#[test]
fn start_game_contest_sequences_dice_before_game_started() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 7);
    let result = start_game(&mut state);
    let contest = result
        .events
        .iter()
        .position(|e| matches!(e, GameEvent::StartingPlayerContest { .. }))
        .expect("StartingPlayerContest present");
    let first_game_started = result
        .events
        .iter()
        .position(|e| matches!(e, GameEvent::GameStarted))
        .expect("GameStarted present");
    let first_turn_started = result
        .events
        .iter()
        .position(|e| matches!(e, GameEvent::TurnStarted { .. }))
        .expect("TurnStarted present");
    assert!(
        contest < first_game_started,
        "StartingPlayerContest must precede GameStarted"
    );
    assert!(
        first_game_started < first_turn_started,
        "GameStarted must precede TurnStarted"
    );
}

/// Tie path: when the first round ties, a reroll round occurs and each
/// later round's seat set is a subset of the prior round's tied-max group.
#[test]
fn start_game_contest_tie_triggers_reroll_and_resolves() {
    // Scan seeds for one whose contest needs more than one round (a tie at
    // the round's max forces a reroll). Proves the reroll branch end-to-end.
    let mut tie_seed = None;
    for seed in 0..2000u64 {
        let mut probe = GameState::new(FormatConfig::standard(), 2, seed);
        let result = start_game(&mut probe);
        let (rounds, _) = contest_event(&result);
        if rounds.len() > 1 {
            tie_seed = Some(seed);
            break;
        }
    }
    let seed = tie_seed.expect("a tie within 2000 seeds (P(tie) = 1/20)");
    let mut state = GameState::new(FormatConfig::standard(), 2, seed);
    let result = start_game(&mut state);
    let (rounds, winner) = contest_event(&result);
    assert!(rounds.len() > 1, "tie seed must produce a reroll round");
    // CR 103.1: each later round rerolls exactly the prior round's tied-max
    // group, so its seat set ⊆ that group.
    for window in rounds.windows(2) {
        let (prev, next) = (&window[0], &window[1]);
        let prev_max = prev.rolls.iter().map(|&(_, r)| r).max().unwrap();
        let prev_top: Vec<PlayerId> = prev
            .rolls
            .iter()
            .filter(|&&(_, r)| r == prev_max)
            .map(|&(s, _)| s)
            .collect();
        for &(seat, _) in &next.rolls {
            assert!(
                prev_top.contains(&seat),
                "reroll round seats must be a subset of the prior tied-max group"
            );
        }
    }
    // Resolves to exactly one starting player that is a valid seat.
    assert_eq!(state.current_starting_player, winner);
    assert!(
        state.seat_order.contains(&winner),
        "starting player is a valid seat after a reroll"
    );
    exactly_one_game_started(&result);
}

/// CR 103.1: high roller wins — for 3- and 4-player contests across many
/// seeds, the winner is the unique-max roller of the FINAL round's rolls.
#[test]
fn start_game_contest_high_roller_wins_three_and_four_seats() {
    for player_count in [3u8, 4] {
        for seed in 0..500u64 {
            let mut state = GameState::new(FormatConfig::commander(), player_count, seed);
            let result = start_game(&mut state);
            let (rounds, winner) = contest_event(&result);
            let final_round = rounds.last().expect("at least one round");
            let max_roll = final_round.rolls.iter().map(|&(_, r)| r).max().unwrap();
            let top: Vec<PlayerId> = final_round
                .rolls
                .iter()
                .filter(|&&(_, r)| r == max_roll)
                .map(|&(s, _)| s)
                .collect();
            // ChaCha20 never reaches the all-tie cap within these seeds, so
            // the final round always has a unique max == winner.
            assert_eq!(
                top.len(),
                1,
                "final round has a unique max (no cap fallback) at seed {seed}"
            );
            assert_eq!(
                winner, top[0],
                "winner is the unique-max roller of the final round"
            );
            assert_eq!(state.current_starting_player, winner);
        }
    }
}

/// CR 103.1: round-structure invariants across player counts and seeds —
/// round 1 covers exactly the seat order, each later round's seat set equals
/// the prior round's tied-max group, and the winner is the final round's
/// unique max.
#[test]
fn start_game_contest_round_structure_invariants() {
    for player_count in [2u8, 3, 4] {
        for seed in 0..300u64 {
            let format = if player_count == 2 {
                FormatConfig::standard()
            } else {
                FormatConfig::commander()
            };
            let mut state = GameState::new(format, player_count, seed);
            let seat_order = state.seat_order.clone();
            let result = start_game(&mut state);
            let (rounds, winner) = contest_event(&result);
            assert!(!rounds.is_empty(), "at least one round");

            // Round 1 covers exactly the seat order, in seat order.
            let round1_seats: Vec<PlayerId> = rounds[0].rolls.iter().map(|&(s, _)| s).collect();
            assert_eq!(
                round1_seats, seat_order,
                "round 1 rolls cover exactly the seat order"
            );

            // Each later round == set of seats tied at max of the prior round.
            for window in rounds.windows(2) {
                let (prev, next) = (&window[0], &window[1]);
                let prev_max = prev.rolls.iter().map(|&(_, r)| r).max().unwrap();
                let mut prev_top: Vec<PlayerId> = prev
                    .rolls
                    .iter()
                    .filter(|&&(_, r)| r == prev_max)
                    .map(|&(s, _)| s)
                    .collect();
                let mut next_seats: Vec<PlayerId> = next.rolls.iter().map(|&(s, _)| s).collect();
                prev_top.sort();
                next_seats.sort();
                assert_eq!(
                    next_seats, prev_top,
                    "reroll round seat set == prior round's tied-max group"
                );
            }

            // Winner == unique max of the final round (no all-tie cap hit
            // within these seeds).
            let final_round = rounds.last().unwrap();
            let max_roll = final_round.rolls.iter().map(|&(_, r)| r).max().unwrap();
            let top: Vec<PlayerId> = final_round
                .rolls
                .iter()
                .filter(|&&(_, r)| r == max_roll)
                .map(|&(s, _)| s)
                .collect();
            assert_eq!(top.len(), 1, "final round has a unique max");
            assert_eq!(winner, top[0]);
            assert_eq!(state.current_starting_player, winner);
        }
    }
}

/// The tie loop is BOUNDED: at most FIRST_PLAYER_CONTEST_MAX_ROUNDS rounds
/// before the lowest-seat fallback. (Forcing a *true* all-tie out of
/// ChaCha20 is impractical, so this asserts the structural round bound that
/// makes the fallback reachable rather than the fallback firing.)
#[test]
fn start_game_contest_is_bounded_no_hang() {
    for seed in 0..200u64 {
        for player_count in [2u8, 3, 4] {
            let mut state = GameState::new(FormatConfig::commander(), player_count, seed);
            let result = start_game(&mut state);
            let (rounds, winner) = contest_event(&result);
            assert!(
                    rounds.len() <= FIRST_PLAYER_CONTEST_MAX_ROUNDS,
                    "contest must terminate within the bounded reroll cap (got {} rounds, cap {FIRST_PLAYER_CONTEST_MAX_ROUNDS})",
                    rounds.len()
                );
            assert!(state.seat_order.contains(&winner));
            assert_eq!(state.current_starting_player, winner);
        }
    }
}

/// `build_contest_rounds` with SCRIPTED rolls (no RNG): a unique max in a
/// later round breaks an earlier tie, and an all-tie path falls back to the
/// lowest seat index. The one allowed hand-constructed contest test.
#[test]
fn build_contest_rounds_scripted_paths() {
    let seats = [PlayerId(0), PlayerId(1), PlayerId(2)];

    // Round 1: seats 0,1,2 roll 20,20,5 → tie among 0,1.
    // Round 2: seats 0,1 roll 20,3 → seat 0 wins.
    let scripted = [
        vec![(PlayerId(0), 20u8), (PlayerId(1), 20), (PlayerId(2), 5)],
        vec![(PlayerId(0), 20u8), (PlayerId(1), 3)],
    ];
    let mut idx = 0;
    let (rounds, winner) = build_contest_rounds(&seats, |contenders| {
        let round = scripted[idx].clone();
        // The closure receives exactly the contenders we scripted for.
        let seats_in: Vec<PlayerId> = round.iter().map(|&(s, _)| s).collect();
        assert_eq!(contenders.to_vec(), seats_in);
        idx += 1;
        round
    });
    assert_eq!(rounds.len(), 2, "tie forces exactly one reroll round");
    assert_eq!(rounds[0].rolls.len(), 3);
    assert_eq!(rounds[1].rolls.len(), 2, "reroll only the tied group");
    assert_eq!(winner, PlayerId(0));

    // All-tie path: every round ties the full group → cap reached → lowest
    // seat index (seat 1 here) wins.
    let tie_seats = [PlayerId(2), PlayerId(1)];
    let (rounds, winner) = build_contest_rounds(&tie_seats, |contenders| {
        contenders.iter().map(|&s| (s, 7u8)).collect()
    });
    assert_eq!(
        rounds.len(),
        FIRST_PLAYER_CONTEST_MAX_ROUNDS,
        "all-tie runs to the cap"
    );
    assert_eq!(winner, PlayerId(1), "lowest seat index wins on cap");
}

/// Explicit `start_game_with_starting_player` runs no contest and emits NO
/// `StartingPlayerContest` event.
#[test]
fn start_game_with_explicit_player_emits_no_dice() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 7);
    let result = start_game_with_starting_player(&mut state, PlayerId(1));
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::StartingPlayerContest { .. })),
        "explicit starting player path must emit no contest event"
    );
    assert_eq!(state.current_starting_player, PlayerId(1));
}

#[test]
fn archenemy_starting_life_and_first_turn_use_configured_archenemy() {
    let mut config = FormatConfig::archenemy();
    config.archenemy_player = Some(PlayerId(2));
    let mut state = GameState::new(config, 4, 7);

    assert_eq!(state.players[0].life, 20);
    assert_eq!(state.players[1].life, 20);
    assert_eq!(state.players[2].life, 40);
    assert_eq!(state.players[3].life, 20);
    assert_eq!(state.active_player, PlayerId(2));
    assert_eq!(state.priority_player, PlayerId(2));
    assert_eq!(state.current_starting_player, PlayerId(2));
    assert_eq!(
        state.waiting_for,
        crate::types::game_state::WaitingFor::Priority {
            player: PlayerId(2)
        }
    );

    let result = start_game(&mut state);

    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::StartingPlayerContest { .. })),
        "Archenemy must not run the starting-player contest"
    );
    assert_eq!(state.current_starting_player, PlayerId(2));
    assert_eq!(state.active_player, PlayerId(2));
    assert_eq!(state.priority_player, PlayerId(2));
}

#[test]
fn two_hg_team_identity_survives_starting_player_rotation() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 7);

    start_game_with_starting_player(&mut state, PlayerId(1));

    assert_eq!(
        state.seat_order,
        vec![PlayerId(1), PlayerId(2), PlayerId(3), PlayerId(0)]
    );
    assert!(!crate::game::players::is_opponent(
        &state,
        PlayerId(0),
        PlayerId(1)
    ));
    assert!(crate::game::players::is_opponent(
        &state,
        PlayerId(0),
        PlayerId(2)
    ));
    assert!(crate::game::players::is_opponent(
        &state,
        PlayerId(0),
        PlayerId(3)
    ));
    assert_eq!(
        crate::game::players::team_life_total(&state, PlayerId(0)),
        30
    );
    assert_eq!(
        crate::game::players::team_life_total(&state, PlayerId(1)),
        30
    );
}

/// Empty seat order keeps the PlayerId(0) fast path and emits no contest.
#[test]
fn start_game_empty_seat_order_no_contest() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 7);
    state.seat_order.clear();
    let result = start_game(&mut state);
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::StartingPlayerContest { .. })),
        "empty seat order must emit no contest event"
    );
    assert_eq!(state.current_starting_player, PlayerId(0));
}

fn exactly_one_game_started(result: &ActionResult) {
    let count = result
        .events
        .iter()
        .filter(|e| matches!(e, GameEvent::GameStarted))
        .count();
    assert_eq!(count, 1, "exactly one GameStarted event");
}
