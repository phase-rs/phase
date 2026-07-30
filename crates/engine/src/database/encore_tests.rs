//! Tests for Encore (CR 702.141) synthesis and runtime. Declared from
//! `database/mod.rs` so the implementation module (`encore.rs`) and the resolver
//! (`game/effects/encore.rs`) stay free of inline test scaffolding.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::encore::synthesize_encore;
use crate::database::mtgjson::{AtomicCard, AtomicIdentifiers};
use crate::game::casting::{can_activate_ability_now, handle_activate_ability};
use crate::game::stack::resolve_top;
use crate::game::triggers::check_delayed_triggers;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityCost, AbilityKind, ContinuousModification, DelayedTriggerCondition, Effect,
    TargetFilter, TargetRef,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::format::FormatConfig;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

// ---------------------------------------------------------------------------
// Synthesis-shape tests (building-block level)
// ---------------------------------------------------------------------------

fn face_with(keyword: Keyword) -> CardFace {
    let mut face = CardFace::default();
    face.keywords.push(keyword);
    face
}

/// CR 702.141a + CR 602.1a: a synthesized Encore ability is a sorcery-speed
/// graveyard-activated `Effect::Encore` with a composite `mana + exile-self`
/// cost.
#[test]
fn synthesize_encore_builds_graveyard_sorcery_speed_ability() {
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 4,
    };
    let mut face = face_with(Keyword::Encore(cost.clone()));
    synthesize_encore(&mut face);

    assert_eq!(face.abilities.len(), 1, "exactly one synthesized ability");
    let def = &face.abilities[0];
    assert_eq!(def.kind, AbilityKind::Activated);
    assert_eq!(def.activation_zone, Some(Zone::Graveyard));
    assert!(def.is_sorcery_speed(), "activate only as a sorcery");
    assert!(matches!(def.effect.as_ref(), Effect::Encore));

    // CR 602.1a: activation cost = keyword mana cost + exile-self-from-graveyard.
    match def.cost.as_ref().expect("must have a cost") {
        AbilityCost::Composite { costs } => {
            assert_eq!(costs.len(), 2);
            assert!(matches!(&costs[0], AbilityCost::Mana { cost: c } if c == &cost));
            assert!(matches!(
                &costs[1],
                AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Graveyard),
                    filter: Some(TargetFilter::SelfRef),
                }
            ));
        }
        other => panic!("expected Composite cost, got {other:?}"),
    }
}

/// Per CR 113.2c each keyword instance yields its own ability.
#[test]
fn synthesize_encore_one_ability_per_keyword_instance() {
    let mut face = CardFace::default();
    face.keywords.push(Keyword::Encore(ManaCost::default()));
    face.keywords.push(Keyword::Encore(ManaCost::default()));
    synthesize_encore(&mut face);
    assert_eq!(face.abilities.len(), 2);
}

/// Cards without the keyword are untouched.
#[test]
fn synthesize_encore_is_noop_without_keyword() {
    let mut face = face_with(Keyword::Flying);
    synthesize_encore(&mut face);
    assert!(face.abilities.is_empty());
}

// ---------------------------------------------------------------------------
// Runtime tests (end-to-end activation)
// ---------------------------------------------------------------------------

/// Put a creature with synthesized Encore into the graveyard, with distinctive
/// copiable characteristics so the token copy is observable.
fn setup_graveyard_source(state: &mut GameState, cost: ManaCost) -> ObjectId {
    let source = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Encore Source".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = state.objects.get_mut(&source).unwrap();
        obj.power = Some(3);
        obj.base_power = Some(3);
        obj.toughness = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Goblin".to_string());
        obj.base_card_types.subtypes.push("Goblin".to_string());
        obj.color = vec![ManaColor::Red];
        obj.base_color = vec![ManaColor::Red];
        obj.keywords.push(Keyword::Encore(cost.clone()));
    }
    let mut face = CardFace::default();
    face.keywords.push(Keyword::Encore(cost));
    synthesize_encore(&mut face);
    Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).extend(face.abilities);
    source
}

fn main_phase_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.phase = Phase::PreCombatMain;
    state
}

/// CR 702.141a: activatable from the graveyard at sorcery speed.
#[test]
fn encore_activatable_from_graveyard_at_sorcery_speed() {
    let mut state = main_phase_state();
    let source = setup_graveyard_source(&mut state, ManaCost::default());
    assert!(
        can_activate_ability_now(&state, PlayerId(0), source, 0),
        "Encore must be activatable from graveyard in the sorcery window"
    );
}

/// CR 702.141a: cannot activate outside the sorcery-speed window.
#[test]
fn encore_rejects_instant_speed() {
    let mut state = main_phase_state();
    state.phase = Phase::Upkeep;
    let source = setup_graveyard_source(&mut state, ManaCost::default());
    assert!(
        !can_activate_ability_now(&state, PlayerId(0), source, 0),
        "Encore must reject activation outside the sorcery-speed window"
    );
}

/// CR 702.141a end-to-end (two-player): activating Encore exiles the source as a
/// cost, then on resolution creates one haste-bearing copy token that must
/// attack the single opponent and is queued for sacrifice at the next end step.
#[test]
fn encore_activation_creates_attacking_haste_copy_per_opponent() {
    let mut state = main_phase_state();
    let source = setup_graveyard_source(&mut state, ManaCost::default());

    let mut events = Vec::new();
    handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events)
        .expect("activation must succeed");

    // CR 602.1a + CR 118.3: exile-self cost paid — same ObjectId now in exile.
    assert_eq!(state.objects[&source].zone, Zone::Exile);
    assert!(!state.stack.is_empty(), "ability is on the stack");

    resolve_top(&mut state, &mut Vec::new());

    // CR 702.141a: exactly one token (one opponent in a two-player game).
    assert_eq!(
        state.last_created_token_ids.len(),
        1,
        "one copy token per opponent"
    );
    let token = state.last_created_token_ids[0];
    let tok = &state.objects[&token];
    assert!(state.battlefield.contains(&token), "token on battlefield");
    assert!(tok.is_token, "created object is a token");
    // Copies the source's copiable values (3/2 red Goblin).
    assert_eq!(tok.power, Some(3));
    assert_eq!(tok.toughness, Some(2));
    assert!(tok.card_types.subtypes.contains(&"Goblin".to_string()));
    // CR 702.141a: "The tokens gain haste."
    assert!(
        tok.keywords.contains(&Keyword::Haste),
        "token must gain haste"
    );

    // CR 702.141a + CR 508.1d: token must attack the opponent (PlayerId(1)) this
    // turn — a transient MustAttackPlayer requirement bound to the token.
    let must_attack = state
        .transient_continuous_effects
        .iter()
        .find(|ce| ce.affected == (TargetFilter::SpecificObject { id: token }))
        .expect("token must carry a transient continuous effect");
    assert!(
        must_attack.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddStaticMode {
                mode: StaticMode::MustAttackPlayer { player },
            } if *player == PlayerId(1)
        )),
        "token must be required to attack the opponent"
    );

    // CR 702.141a + CR 603.7d: a one-shot end-step delayed sacrifice of the token.
    let delayed = state
        .delayed_triggers
        .iter()
        .find(|dt| {
            matches!(
                dt.condition,
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
            ) && matches!(dt.ability.effect, Effect::Sacrifice { .. })
        })
        .expect("a next-end-step sacrifice must be scheduled");
    assert!(
        dt_targets_contain(delayed, token),
        "the delayed sacrifice must target the created token"
    );
}

fn dt_targets_contain(dt: &crate::types::game_state::DelayedTrigger, id: ObjectId) -> bool {
    dt.ability
        .targets
        .iter()
        .any(|t| matches!(t, crate::types::ability::TargetRef::Object(obj) if *obj == id))
}

/// CR 702.141a end-to-end (THREE players — the per-opponent case the dedicated
/// resolver exists for): activating Encore creates one haste-bearing copy token
/// PER opponent, each bound via `MustAttackPlayer` to the *distinct* opponent it
/// was created for, all collected into one next-end-step sacrifice — and that
/// sacrifice, when the end step arrives, actually removes both tokens.
#[test]
fn encore_three_player_one_token_per_opponent_then_sacrificed_at_end_step() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
    state.active_player = PlayerId(0);
    state.phase = Phase::PreCombatMain;
    let source = setup_graveyard_source(&mut state, ManaCost::default());

    let mut events = Vec::new();
    handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events)
        .expect("activation must succeed");
    assert_eq!(state.objects[&source].zone, Zone::Exile);
    resolve_top(&mut state, &mut Vec::new());

    // The resolver collects every created token into the delayed sacrifice's
    // explicit targets (`last_created_token_ids` only holds the LAST opponent's
    // token, so read the tokens from there).
    let delayed = state
        .delayed_triggers
        .iter()
        .find(|dt| {
            matches!(
                dt.condition,
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
            ) && matches!(dt.ability.effect, Effect::Sacrifice { .. })
        })
        .expect("a next-end-step sacrifice must be scheduled")
        .clone();
    let tokens: Vec<ObjectId> = delayed
        .ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect();

    // CR 702.141a: "For each opponent, create a token …" — two opponents → two tokens.
    assert_eq!(
        tokens.len(),
        2,
        "one copy token per opponent (3 players → 2 tokens)"
    );

    // Each token is a haste-bearing copy bound to a DISTINCT opponent's
    // `MustAttackPlayer` requirement (the whole reason the dedicated resolver
    // exists — generic ForceAttack can't bind "that opponent").
    let mut bound_opponents = BTreeSet::new();
    for &token in &tokens {
        let tok = &state.objects[&token];
        assert!(state.battlefield.contains(&token), "token on battlefield");
        assert!(tok.is_token, "created object is a token");
        assert!(
            tok.keywords.contains(&Keyword::Haste),
            "each token gains haste"
        );
        let player = state
            .transient_continuous_effects
            .iter()
            .filter(|ce| ce.affected == (TargetFilter::SpecificObject { id: token }))
            .find_map(|ce| {
                ce.modifications.iter().find_map(|m| match m {
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustAttackPlayer { player },
                    } => Some(*player),
                    _ => None,
                })
            })
            .expect("token must carry a MustAttackPlayer requirement");
        bound_opponents.insert(player);
    }
    let expected: BTreeSet<PlayerId> = [PlayerId(1), PlayerId(2)].into_iter().collect();
    assert_eq!(
        bound_opponents, expected,
        "each token must be bound to a distinct opponent, not all to the same one"
    );

    // CR 702.141a + CR 603.7d: at the next end step the sacrifice fires and removes
    // BOTH tokens (drives the resolution end-to-end, not just the scheduling).
    state.phase = Phase::End;
    let stacked =
        check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
    assert!(!stacked.is_empty(), "the end-step sacrifice must fire");
    resolve_top(&mut state, &mut Vec::new());

    for &token in &tokens {
        assert!(
            !state.battlefield.contains(&token),
            "every encore token is sacrificed at the next end step"
        );
    }
    assert!(
        !state.delayed_triggers.iter().any(|dt| matches!(
            dt.condition,
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
        ) && matches!(
            dt.ability.effect,
            Effect::Sacrifice { .. }
        )),
        "the one-shot end-step sacrifice is consumed after firing"
    );
}

// ---------------------------------------------------------------------------
// Real-pipeline integration (MTGJSON -> parse -> synthesize)
// ---------------------------------------------------------------------------

/// Real Encore card routed through `build_oracle_face` — exercises the true
/// production path (MTGJSON keyword parse -> `synthesize_all` ->
/// `synthesize_encore`).
#[test]
fn real_encore_card_synthesizes_encore_ability() {
    let atomic = AtomicCard {
        name: "Coastline Marauders".to_string(),
        mana_cost: Some("{4}{R}".to_string()),
        colors: vec!["R".to_string()],
        color_identity: vec!["R".to_string()],
        text: Some(
            "Whenever Coastline Marauders attacks, it gets +2/+0 until end of turn.\n\
             Encore {6}{R}{R}{R} ({6}{R}{R}{R}, Exile this card from your graveyard: For each \
             opponent, create a token that's a copy of this card that attacks that opponent this \
             turn if able. They gain haste. Sacrifice them at the beginning of the next end step. \
             Activate only as a sorcery.)"
                .to_string(),
        ),
        power: Some("5".to_string()),
        toughness: Some("3".to_string()),
        loyalty: None,
        defense: None,
        layout: "normal".to_string(),
        type_line: Some("Creature — Human Pirate".to_string()),
        types: vec!["Creature".to_string()],
        subtypes: vec!["Human".to_string(), "Pirate".to_string()],
        supertypes: Vec::new(),
        keywords: Some(vec!["Encore".to_string()]),
        side: None,
        face_name: None,
        mana_value: 5.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some("coastline-marauders-oracle".to_string()),
            scryfall_id: Some("coastline-marauders-face".to_string()),
        },
        foreign_data: Vec::new(),
        related_cards: crate::database::mtgjson::SetRelatedCards::default(),
    };

    let face = crate::database::synthesis::build_oracle_face(&atomic, None);
    assert!(
        face.keywords
            .iter()
            .any(|k| matches!(k, Keyword::Encore(_))),
        "Encore keyword must parse from MTGJSON"
    );
    let def = face
        .abilities
        .iter()
        .find(|d| {
            d.kind == AbilityKind::Activated
                && d.activation_zone == Some(Zone::Graveyard)
                && matches!(d.effect.as_ref(), Effect::Encore)
        })
        .expect("a graveyard-activated Encore ability must be synthesized");
    assert!(def.is_sorcery_speed(), "activate only as a sorcery");
    assert!(
        matches!(
            def.cost.as_ref(),
            Some(AbilityCost::Composite { costs })
                if costs.iter().any(|c| matches!(
                    c,
                    AbilityCost::Exile { zone: Some(Zone::Graveyard), filter: Some(TargetFilter::SelfRef), .. }
                ))
        ),
        "cost must exile self from the graveyard"
    );
    // CR 614: the card is now genuinely runnable, not a parse stub.
    assert!(
        !crate::game::coverage::card_face_has_unimplemented_parts(&face),
        "face must have no Unimplemented parts after synthesis"
    );
}
