//! MSH Wave 3 — Divine Visitation: full creature-token substitution.
//!
//! Oracle: "If one or more creature tokens would be created under your control,
//! that many 4/4 white Angel creature tokens with flying and vigilance are
//! created instead." (CR 614.1a replacement; CR 111.1 token characteristics.)
//!
//! Runtime seam: `create_token_applier` reads the substitute `Effect::Token`
//! carried in the replacement's `execute` field (Approach A — no new
//! ReplacementDefinition field), resolves it to a `TokenSpec`, and swaps it for
//! the proposed token while keeping the count and owner. The creature-type gate
//! (`ReplacementCondition::TokenCoreTypeMatches { [Creature] }`) and the
//! `token_owner_scope(You)` owner gate are evaluated in
//! `find_applicable_replacements` before the applier runs.

use std::sync::Arc;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, ReplacementCondition, ReplacementDefinition};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DIVINE_VISITATION: &str = "If one or more creature tokens would be created under your \
control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn divine_visitation_replacements() -> Vec<ReplacementDefinition> {
    let parsed = parse_oracle_text(
        DIVINE_VISITATION,
        "Divine Visitation",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert!(
        !parsed.replacements.is_empty(),
        "Divine Visitation must parse to a token-creation replacement"
    );
    parsed.replacements
}

/// Put a Divine Visitation enchantment on the battlefield under `controller`
/// carrying the parsed substitution replacement.
fn install_divine_visitation(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(900),
        controller,
        "Divine Visitation".to_string(),
        Zone::Battlefield,
    );
    let reps = divine_visitation_replacements();
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

/// Resolve a token-creating ability (`oracle`) controlled by `controller`,
/// driving the real token pipeline (propose → replace_event → applier).
fn resolve_token_effect(state: &mut GameState, controller: PlayerId, oracle: &str) {
    let parsed = parse_oracle_text(oracle, "Token Source", &[], &["Sorcery".to_string()], &[]);
    let definition = parsed
        .abilities
        .first()
        .expect("token source should parse to an ability");
    let source_id = create_object(
        state,
        CardId(901),
        controller,
        "Token Source".to_string(),
        Zone::Stack,
    );
    let ability = build_resolved_from_def(definition, source_id, controller);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(state, &ability, &mut events, 0).expect("token effect should resolve");
}

fn token_objects(
    state: &GameState,
    controller: PlayerId,
) -> Vec<&engine::game::game_object::GameObject> {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.is_token && obj.controller == controller)
        .collect()
}

/// Primary fix: a 1/1 Soldier creature token created under your control becomes
/// a 4/4 white Angel with flying and vigilance; zero originals remain.
/// Revert-fail: the original Soldier is created instead (no Angel).
#[test]
fn creature_token_becomes_angel_under_your_control() {
    let mut state = GameState::new_two_player(7);
    install_divine_visitation(&mut state, P0);

    resolve_token_effect(&mut state, P0, "Create a 1/1 white Soldier creature token.");

    let tokens = token_objects(&state, P0);
    assert_eq!(tokens.len(), 1, "exactly one substituted token expected");
    let angel = tokens[0];
    assert_eq!(angel.power, Some(4), "substituted token is 4/4");
    assert_eq!(angel.toughness, Some(4), "substituted token is 4/4");
    assert!(
        angel.color.contains(&ManaColor::White),
        "substituted token is white, got {:?}",
        angel.color
    );
    assert!(
        angel
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Angel")),
        "substituted token is an Angel, got {:?}",
        angel.card_types.subtypes
    );
    assert!(
        angel.card_types.core_types.contains(&CoreType::Creature),
        "substituted token is a creature"
    );
    assert!(
        angel.has_keyword(&Keyword::Flying) && angel.has_keyword(&Keyword::Vigilance),
        "substituted Angel has flying and vigilance"
    );
    assert!(
        !angel
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Soldier")),
        "no original Soldier token should remain"
    );
}

/// Negative — core-type gate: a non-creature Treasure token is NOT substituted
/// (TokenCoreTypeMatches { Creature } fails). Revert-fail (if the gate is
/// dropped): the Treasure would turn into an Angel.
#[test]
fn treasure_token_is_not_substituted() {
    let mut state = GameState::new_two_player(7);
    install_divine_visitation(&mut state, P0);

    resolve_token_effect(&mut state, P0, "Create a Treasure token.");

    let tokens = token_objects(&state, P0);
    assert_eq!(tokens.len(), 1, "exactly one token expected");
    let treasure = tokens[0];
    assert!(
        treasure
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Treasure")),
        "Treasure token must stay a Treasure, got {:?}",
        treasure.card_types.subtypes
    );
    assert!(
        !treasure.card_types.core_types.contains(&CoreType::Creature),
        "Treasure token must not become a creature"
    );
}

/// Negative — owner scope: a creature token created under an OPPONENT's control
/// is NOT substituted (token_owner_scope(You)). Revert-fail (if owner scope is
/// dropped): the opponent's Soldier would become an Angel.
#[test]
fn opponent_creature_token_is_not_substituted() {
    let mut state = GameState::new_two_player(7);
    install_divine_visitation(&mut state, P0);

    resolve_token_effect(&mut state, P1, "Create a 1/1 white Soldier creature token.");

    let tokens = token_objects(&state, P1);
    assert_eq!(tokens.len(), 1, "exactly one opponent token expected");
    let soldier = tokens[0];
    assert!(
        soldier
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Soldier")),
        "opponent's Soldier token must remain a Soldier, got {:?}",
        soldier.card_types.subtypes
    );
    assert_ne!(
        soldier.power,
        Some(4),
        "opponent's token must not be a 4/4 Angel"
    );
}

/// Parser unit: Divine Visitation parses to a CreateToken replacement carrying
/// the substitute `Effect::Token` (4/4 Angel) in `execute`, the creature
/// core-type gate, and the You owner scope.
#[test]
fn divine_visitation_parses_substitution_replacement() {
    use engine::types::ability::{ControllerRef, PtValue};

    let reps = divine_visitation_replacements();
    let dv = reps
        .iter()
        .find(|r| r.execute.is_some())
        .expect("expected a substitution replacement with an execute payload");

    assert_eq!(
        dv.condition,
        Some(ReplacementCondition::TokenCoreTypeMatches {
            core_types: vec![CoreType::Creature],
        }),
        "must gate on creature tokens"
    );
    assert_eq!(
        dv.token_owner_scope,
        Some(ControllerRef::You),
        "'under your control' → You owner scope"
    );

    let execute = dv.execute.as_deref().expect("execute payload");
    match execute.effect.as_ref() {
        Effect::Token {
            power,
            toughness,
            colors,
            keywords,
            ..
        } => {
            assert_eq!(*power, PtValue::Fixed(4), "Angel is 4/4");
            assert_eq!(*toughness, PtValue::Fixed(4), "Angel is 4/4");
            assert!(colors.contains(&ManaColor::White), "Angel is white");
            assert!(
                keywords.contains(&Keyword::Flying) && keywords.contains(&Keyword::Vigilance),
                "Angel has flying and vigilance, got {keywords:?}"
            );
        }
        other => panic!("expected Effect::Token substitute, got {other:?}"),
    }
}
