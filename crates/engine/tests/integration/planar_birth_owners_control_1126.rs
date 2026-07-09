//! Issue #1126 — Planar Birth: "Return all basic land cards from all graveyards
//! to the battlefield tapped under their owners' control."
//!
//! Discriminating runtime test for the mass return-to-battlefield building block:
//!   * basic-land supertype filter (nonbasic lands and non-lands stay put),
//!   * `enter_tapped` is honored on a `ChangeZoneAll` return, and
//!   * with no `enters_under` override each card enters under its *owner's*
//!     control (CR 110.2a) — the opponent-owned land does NOT enter under the
//!     caster, unlike "under your control" reanimators (cf. Rise of the Dark
//!     Realms, #1973).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

const PLANAR_BIRTH_ORACLE: &str = "Return all basic land cards from all graveyards \
    to the battlefield tapped under their owners' control.";

fn graveyard_card(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
    core: CoreType,
    basic: bool,
) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Graveyard,
    );
    let obj = state.objects.get_mut(&oid).expect("just created");
    obj.card_types.core_types.push(core);
    if basic {
        obj.card_types.supertypes.push(Supertype::Basic);
    }
    obj.base_card_types = obj.card_types.clone();
    oid
}

#[test]
fn planar_birth_returns_basic_lands_tapped_under_owners_control() {
    let def = parse_effect_chain(PLANAR_BIRTH_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                enter_tapped: EtbTapState::Tapped,
                ..
            }
        ),
        "parsed shape: {:?}",
        def.effect
    );

    let mut state = GameState::new_two_player(1126);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Planar Birth".to_string(),
        Zone::Stack,
    );

    // Caster-owned basic land + opponent-owned basic land: both should return.
    let p0_plains = graveyard_card(&mut state, 10, PlayerId(0), "Plains", CoreType::Land, true);
    let p1_plains = graveyard_card(&mut state, 11, PlayerId(1), "Island", CoreType::Land, true);
    // Nonbasic land (no Basic supertype) and a non-land: both must stay put.
    let p0_nonbasic = graveyard_card(
        &mut state,
        12,
        PlayerId(0),
        "Wasteland",
        CoreType::Land,
        false,
    );
    let p1_creature = graveyard_card(
        &mut state,
        13,
        PlayerId(1),
        "Grizzly Bears",
        CoreType::Creature,
        false,
    );

    let ability = build_resolved_from_def(&def, source, PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("Planar Birth resolves");

    // Both basic lands return to the battlefield.
    assert_eq!(
        state.objects[&p0_plains].zone,
        Zone::Battlefield,
        "caster's basic land returns"
    );
    assert_eq!(
        state.objects[&p1_plains].zone,
        Zone::Battlefield,
        "opponent's basic land returns (all graveyards)"
    );

    // They enter tapped (CR: "to the battlefield tapped").
    assert!(
        state.objects[&p0_plains].tapped,
        "returned basic land enters tapped"
    );
    assert!(
        state.objects[&p1_plains].tapped,
        "opponent's returned basic land enters tapped"
    );

    // Under their OWNERS' control — the opponent's land does NOT enter under the
    // caster (CR 110.2a default = owner; no `enters_under` override parsed).
    assert_eq!(
        state.objects[&p0_plains].controller,
        PlayerId(0),
        "caster-owned land enters under caster (its owner)"
    );
    assert_eq!(
        state.objects[&p1_plains].controller,
        PlayerId(1),
        "opponent-owned land enters under its owner, not the caster"
    );

    // Filter discrimination: nonbasic land and non-land stay in the graveyard.
    assert_eq!(
        state.objects[&p0_nonbasic].zone,
        Zone::Graveyard,
        "nonbasic land is not returned"
    );
    assert_eq!(
        state.objects[&p1_creature].zone,
        Zone::Graveyard,
        "non-land card is not returned"
    );
}
