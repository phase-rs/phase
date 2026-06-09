//! Tests for Embalm (CR 702.128) / Eternalize (CR 702.129) synthesis and
//! runtime. Declared from `database/mod.rs` so the implementation module
//! (`embalm_eternalize.rs`) stays free of inline test scaffolding.

use std::sync::Arc;

use super::embalm_eternalize::synthesize_embalm_eternalize;
use crate::database::mtgjson::AtomicCard;
use crate::game::casting::{can_activate_ability_now, handle_activate_ability};
use crate::game::stack::resolve_top;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityCost, AbilityKind, CardSelectionMode, ContinuousModification, DiscardSelfScope, Effect,
    QuantityExpr, TargetFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{EmbalmCost, EternalizeCost, Keyword};
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

// ---------------------------------------------------------------------------
// Synthesis-shape tests (building-block level)
// ---------------------------------------------------------------------------

fn face_with(keyword: Keyword) -> CardFace {
    let mut face = CardFace::default();
    face.keywords.push(keyword);
    face
}

/// Shared assertions: a synthesized Embalm/Eternalize ability is a sorcery-speed
/// graveyard-activated `CopyTokenOf` with a composite `mana + exile-self` cost.
fn assert_token_copy_ability_shape(
    face: &CardFace,
    expected_cost: &ManaCost,
) -> Vec<ContinuousModification> {
    assert_eq!(face.abilities.len(), 1, "exactly one synthesized ability");
    let def = &face.abilities[0];
    assert_eq!(def.kind, AbilityKind::Activated);
    assert_eq!(def.activation_zone, Some(Zone::Graveyard));
    assert!(def.sorcery_speed, "activate only as a sorcery");

    // CR 602.1a: activation cost (everything before the colon) — keyword mana
    // cost + exile-self-from-graveyard.
    match def.cost.as_ref().expect("must have a cost") {
        AbilityCost::Composite { costs } => {
            assert_eq!(costs.len(), 2);
            assert!(matches!(&costs[0], AbilityCost::Mana { cost } if cost == expected_cost));
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

    // CR 707.2: token that's a copy of this card (SelfRef), under our control.
    match def.effect.as_ref() {
        Effect::CopyTokenOf {
            target,
            owner,
            count,
            additional_modifications,
            ..
        } => {
            assert_eq!(target, &TargetFilter::SelfRef);
            assert_eq!(owner, &TargetFilter::Controller);
            assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            additional_modifications.clone()
        }
        other => panic!("expected CopyTokenOf effect, got {other:?}"),
    }
}

/// CR 702.128a: Embalm overrides = white + no mana cost + Zombie (added).
#[test]
fn synthesize_embalm_builds_white_zombie_no_mana_cost_copy() {
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 3,
    };
    let mut face = face_with(Keyword::Embalm(EmbalmCost::Mana(cost.clone())));
    synthesize_embalm_eternalize(&mut face);

    let mods = assert_token_copy_ability_shape(&face, &cost);
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::SetColor { colors } if colors == &vec![ManaColor::White]
    )));
    assert!(mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::RemoveManaCost)));
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddSubtype { subtype } if subtype == "Zombie"
    )));
    // Embalm does not set P/T.
    assert!(!mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::SetPower { .. })));
}

/// CR 702.129a: Eternalize overrides = black + 4/4 + no mana cost + Zombie.
#[test]
fn synthesize_eternalize_builds_4_4_black_zombie_no_mana_cost_copy() {
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 4,
    };
    let mut face = face_with(Keyword::Eternalize(EternalizeCost::Mana(cost.clone())));
    synthesize_embalm_eternalize(&mut face);

    let mods = assert_token_copy_ability_shape(&face, &cost);
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::SetColor { colors } if colors == &vec![ManaColor::Black]
    )));
    assert!(mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::SetPower { value: 4 })));
    assert!(mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::SetToughness { value: 4 })));
    assert!(mods
        .iter()
        .any(|m| matches!(m, ContinuousModification::RemoveManaCost)));
    assert!(mods.iter().any(|m| matches!(
        m,
        ContinuousModification::AddSubtype { subtype } if subtype == "Zombie"
    )));
}

/// Cards with neither keyword are untouched.
#[test]
fn synthesize_is_noop_without_keyword() {
    let mut face = face_with(Keyword::Flying);
    synthesize_embalm_eternalize(&mut face);
    assert!(face.abilities.is_empty());
}

/// CR 602.1a + CR 702.129a: Champion of Wits family —
/// `EternalizeCost::NonMana(Composite[Mana{3UU}, Discard])` must synthesize a
/// cost `Composite[Mana, Discard, Exile-self]`: the discard sub-cost survives
/// (it is NOT dropped) and the self-exile is appended flat (not nested).
#[test]
fn synthesize_eternalize_nonmana_composite_keeps_discard_and_appends_exile_self() {
    let mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        generic: 3,
    };
    let discard = AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    };
    let keyword = Keyword::Eternalize(EternalizeCost::NonMana(AbilityCost::Composite {
        costs: vec![AbilityCost::Mana { cost: mana.clone() }, discard.clone()],
    }));
    let mut face = face_with(keyword);
    synthesize_embalm_eternalize(&mut face);

    assert_eq!(face.abilities.len(), 1);
    let AbilityCost::Composite { costs } = face.abilities[0]
        .cost
        .as_ref()
        .expect("synthesized ability must have a cost")
    else {
        panic!("expected Composite cost");
    };
    assert_eq!(
        costs.len(),
        3,
        "mana + discard + exile-self, flattened (not nested)"
    );
    assert!(matches!(&costs[0], AbilityCost::Mana { cost } if cost == &mana));
    assert_eq!(
        costs[1], discard,
        "the discard activation cost must survive synthesis"
    );
    assert!(matches!(
        &costs[2],
        AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: Some(TargetFilter::SelfRef),
        }
    ));
}

/// Backward-compat: a legacy bare-`ManaCost` JSON keyword payload (MTGJSON
/// shape, lacking the `type` discriminant) deserializes to the `Mana` variant.
#[test]
fn legacy_bare_mana_json_parses_to_eternalize_mana_variant() {
    // Legacy externally-tagged keyword: the value under "Eternalize" is a bare
    // ManaCost (no `EternalizeCost` `{type, data}` discriminant). The JSON arm
    // must fall back to EternalizeCost::Mana for this shape. Serialize a real
    // ManaCost so the legacy payload's exact serde shape is used.
    let legacy_mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        generic: 3,
    };
    let json = serde_json::json!({
        "Eternalize": serde_json::to_value(&legacy_mana).unwrap()
    });
    let kw: Keyword =
        serde_json::from_value(json).expect("legacy bare-mana eternalize must deserialize");
    let Keyword::Eternalize(EternalizeCost::Mana(mana)) = kw else {
        panic!("expected Eternalize(Mana), got {kw:?}");
    };
    assert_eq!(mana, legacy_mana);
}

// ---------------------------------------------------------------------------
// Runtime tests (end-to-end activation)
// ---------------------------------------------------------------------------

/// Put a creature with the given keyword (synthesized) into the graveyard, with
/// distinctive copiable characteristics so the copy exceptions are observable.
fn setup_graveyard_source(state: &mut GameState, keyword: Keyword) -> ObjectId {
    let source = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Copy Source".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = state.objects.get_mut(&source).unwrap();
        obj.power = Some(2);
        obj.base_power = Some(2);
        obj.toughness = Some(1);
        obj.base_toughness = Some(1);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Cat".to_string());
        obj.base_card_types.subtypes.push("Cat".to_string());
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        };
        obj.base_mana_cost = obj.mana_cost.clone();
        obj.keywords.push(keyword.clone());
    }
    let mut face = CardFace::default();
    face.keywords.push(keyword);
    synthesize_embalm_eternalize(&mut face);
    Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).extend(face.abilities);
    source
}

fn main_phase_state() -> GameState {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.phase = Phase::PreCombatMain;
    state
}

/// CR 702.128a / CR 702.129a: activatable from the graveyard at sorcery speed.
#[test]
fn token_copy_keyword_activatable_from_graveyard_at_sorcery_speed() {
    let mut state = main_phase_state();
    let source = setup_graveyard_source(
        &mut state,
        Keyword::Embalm(EmbalmCost::Mana(ManaCost::default())),
    );
    assert!(
        can_activate_ability_now(&state, PlayerId(0), source, 0),
        "Embalm must be activatable from graveyard in the sorcery window"
    );
}

/// CR 702.128a: cannot activate outside the sorcery-speed window.
#[test]
fn token_copy_keyword_rejects_instant_speed() {
    let mut state = main_phase_state();
    state.phase = Phase::Upkeep;
    let source = setup_graveyard_source(
        &mut state,
        Keyword::Embalm(EmbalmCost::Mana(ManaCost::default())),
    );
    assert!(
        !can_activate_ability_now(&state, PlayerId(0), source, 0),
        "Embalm must reject activation outside the sorcery-speed window"
    );
}

/// Find the single token created on the battlefield this resolution.
fn created_token(state: &GameState) -> ObjectId {
    *state
        .last_created_token_ids
        .first()
        .expect("a token must have been created")
}

/// CR 702.128a + CR 707.2: activating Embalm exiles the source as a cost and
/// creates a white Zombie copy with no mana cost that keeps the source's other
/// types and P/T.
#[test]
fn embalm_activation_creates_white_zombie_copy_with_no_mana_cost() {
    let mut state = main_phase_state();
    let source = setup_graveyard_source(
        &mut state,
        Keyword::Embalm(EmbalmCost::Mana(ManaCost::default())),
    );

    let mut events = Vec::new();
    handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events)
        .expect("activation must succeed");

    // CR 118.3: exile-self cost paid — same ObjectId now sits in exile.
    assert_eq!(state.objects[&source].zone, Zone::Exile);
    assert!(!state.stack.is_empty(), "ability is on the stack");

    resolve_top(&mut state, &mut Vec::new());

    let token = created_token(&state);
    let tok = &state.objects[&token];
    assert!(state.battlefield.contains(&token), "token on battlefield");
    // White, Zombie added to existing Cat, no mana cost, P/T copied (2/1).
    assert_eq!(tok.color, vec![ManaColor::White]);
    assert!(tok.card_types.subtypes.contains(&"Zombie".to_string()));
    assert!(
        tok.card_types.subtypes.contains(&"Cat".to_string()),
        "Zombie is added in addition to the copied types"
    );
    assert_eq!(tok.mana_cost, ManaCost::NoCost, "token has no mana cost");
    assert_eq!(tok.power, Some(2));
    assert_eq!(tok.toughness, Some(1));
}

/// CR 702.129a + CR 707.2: activating Eternalize creates a 4/4 black Zombie copy
/// with no mana cost that keeps the source's other types.
#[test]
fn eternalize_activation_creates_4_4_black_zombie_copy_with_no_mana_cost() {
    let mut state = main_phase_state();
    let source = setup_graveyard_source(
        &mut state,
        Keyword::Eternalize(EternalizeCost::Mana(ManaCost::default())),
    );

    let mut events = Vec::new();
    handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events)
        .expect("activation must succeed");
    assert_eq!(state.objects[&source].zone, Zone::Exile);

    resolve_top(&mut state, &mut Vec::new());

    let token = created_token(&state);
    let tok = &state.objects[&token];
    assert!(state.battlefield.contains(&token));
    assert_eq!(tok.color, vec![ManaColor::Black]);
    assert_eq!(tok.power, Some(4));
    assert_eq!(tok.toughness, Some(4));
    assert!(tok.card_types.subtypes.contains(&"Zombie".to_string()));
    assert!(tok.card_types.subtypes.contains(&"Cat".to_string()));
    assert_eq!(tok.mana_cost, ManaCost::NoCost);
}

// ---------------------------------------------------------------------------
// Real-pipeline integration (MTGJSON -> parse -> synthesize)
// ---------------------------------------------------------------------------

/// Build a minimal real `AtomicCard` for a creature whose Oracle text carries
/// the given keyword line, so `build_oracle_face` exercises the true production
/// path (MTGJSON keyword parse -> `synthesize_all` -> `synthesize_embalm_eternalize`).
fn atomic_creature(name: &str, mana_cost: &str, keyword: &str, oracle: &str) -> AtomicCard {
    AtomicCard {
        name: name.to_string(),
        mana_cost: Some(mana_cost.to_string()),
        colors: vec!["W".to_string()],
        color_identity: vec!["W".to_string()],
        text: Some(oracle.to_string()),
        power: Some("2".to_string()),
        toughness: Some("2".to_string()),
        loyalty: None,
        defense: None,
        layout: "normal".to_string(),
        type_line: Some("Creature — Human Cleric".to_string()),
        types: vec!["Creature".to_string()],
        subtypes: vec!["Human".to_string(), "Cleric".to_string()],
        supertypes: Vec::new(),
        keywords: Some(vec![keyword.to_string()]),
        side: None,
        face_name: None,
        mana_value: 4.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: crate::database::mtgjson::AtomicIdentifiers {
            scryfall_oracle_id: Some(format!("{name}-oracle")),
            scryfall_id: Some(format!("{name}-face")),
        },
        foreign_data: Vec::new(),
    }
}

/// Assert the face carries exactly one synthesized graveyard-activated
/// sorcery-speed `CopyTokenOf` ability — the runtime the keyword should produce.
fn assert_face_has_token_copy_ability(face: &CardFace) {
    let def = face
        .abilities
        .iter()
        .find(|d| {
            d.kind == AbilityKind::Activated
                && d.activation_zone == Some(Zone::Graveyard)
                && matches!(d.effect.as_ref(), Effect::CopyTokenOf { .. })
        })
        .expect("a graveyard-activated CopyTokenOf ability must be synthesized");
    assert!(def.sorcery_speed, "activate only as a sorcery");
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
    // CR 614: the synthesized ability must not contain an Unimplemented marker —
    // i.e. the card is now genuinely runnable, not a parse stub.
    assert!(
        !crate::game::coverage::card_face_has_unimplemented_parts(face),
        "face must have no Unimplemented parts after synthesis"
    );
}

/// Real Embalm card (Anointer Priest shape) routed through `build_oracle_face`.
#[test]
fn real_embalm_card_synthesizes_token_copy_ability() {
    let atomic = atomic_creature(
        "Anointer Priest",
        "{3}{W}",
        "Embalm",
        "Whenever a creature token you control enters, you gain 1 life.\n\
         Embalm {3}{W} ({3}{W}, Exile this card from your graveyard: Create a token \
         that's a copy of it, except it's a white Zombie. Embalm only as a sorcery.)",
    );
    let face = crate::database::synthesis::build_oracle_face(&atomic, None);
    assert!(
        face.keywords
            .iter()
            .any(|k| matches!(k, Keyword::Embalm(_))),
        "Embalm keyword must parse from MTGJSON"
    );
    assert_face_has_token_copy_ability(&face);
}

/// Real Eternalize card (Adorned Pouncer shape) routed through `build_oracle_face`.
#[test]
fn real_eternalize_card_synthesizes_token_copy_ability() {
    let atomic = atomic_creature(
        "Adorned Pouncer",
        "{1}{W}",
        "Eternalize",
        "Double strike\n\
         Eternalize {3}{B}{B} ({3}{B}{B}, Exile this card from your graveyard: Create a \
         token that's a copy of it, except it's a 4/4 black Zombie. Eternalize only as a sorcery.)",
    );
    let face = crate::database::synthesis::build_oracle_face(&atomic, None);
    assert!(
        face.keywords
            .iter()
            .any(|k| matches!(k, Keyword::Eternalize(_))),
        "Eternalize keyword must parse from MTGJSON"
    );
    assert_face_has_token_copy_ability(&face);
}
