//! Integration tests for Kwain, Itinerant Meddler's "each player who drew a card
//! this way gains 1 life" (CR 121.1 + CR 608.2c + CR 109.5).
//!
//! Oracle text (verbatim, Scryfall): "{T}: Each player may draw a card, then each
//! player who drew a card this way gains 1 life."
//!
//! The misparse this fixes: the player-scope SUBJECT "each player who drew a card
//! this way" dropped its "who drew a card this way" restriction, so the GainLife
//! clause parsed with `player_scope: All` — every player gained 1 life even if
//! they declined the optional "may draw" or had an empty library. The corrected
//! parse scopes the life gain to `PerformedActionThisWay { All, Draw }`, resolved
//! from the same `player_actions_this_way` ledger the quantity sibling (Cut a
//! Deal's "for each opponent who drew a card this way") reads.
//!
//! This is the subject-scope twin of `cut_a_deal_draw_this_way_count.rs`: the
//! shared `parse_who_action_this_way` this-way verb table now feeds both the
//! quantity path and the player-scope subject path.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, PlayerFilter, PlayerRelation, QuantityExpr,
};
use engine::types::events::PlayerActionKind;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const KWAIN_ORACLE: &str =
    "{T}: Each player may draw a card, then each player who drew a card this way gains 1 life.";

/// Walk an ability's `sub_ability` chain and return the first clause whose effect
/// is `GainLife` (Kwain's second clause — the "then … gains 1 life" tail).
fn gain_life_clause(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
    let mut cur = def;
    loop {
        if matches!(cur.effect.as_ref(), Effect::GainLife { .. }) {
            return Some(cur);
        }
        cur = cur.sub_ability.as_deref()?;
    }
}

fn parse_kwain() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        KWAIN_ORACLE,
        "Kwain, Itinerant Meddler",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Rogue".to_string()],
    )
}

fn life(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// CR 121.1 + CR 608.2c + CR 109.5: The parsed AST must scope the life gain to
/// the drawers. The "each player who drew a card this way" subject lowers to a
/// `GainLife` clause carrying `player_scope: PerformedActionThisWay { All, Draw }`
/// — NOT the `player_scope: All` the dropped-restriction misparse produced.
///
/// Revert-failing: reverting `strip_performed_action_this_way_clause` leaves the
/// "who drew a card this way" clause unconsumed, so the scope falls back to `All`
/// and this `assert_eq!` flips.
#[test]
fn kwain_scopes_life_gain_to_players_who_drew_this_way() {
    let parsed = parse_kwain();
    assert!(
        !parsed.abilities.is_empty(),
        "Kwain must produce an activated ability, got {:?}",
        parsed.abilities
    );

    let gain = parsed
        .abilities
        .iter()
        .find_map(gain_life_clause)
        .expect("Kwain must produce a GainLife clause for the 'gains 1 life' tail");

    assert!(
        matches!(
            gain.effect.as_ref(),
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ),
        "the second clause gains exactly 1 life, got {:?}",
        gain.effect
    );
    assert_eq!(
        gain.player_scope,
        Some(PlayerFilter::PerformedActionThisWay {
            relation: PlayerRelation::All,
            action: PlayerActionKind::Draw,
        }),
        "'each player who drew a card this way' must scope the life gain to the \
         drawers (the misparse dropped the restriction and produced player_scope \
         All)"
    );
}

/// CR 121.1 + CR 608.2c + CR 109.5: Mixed runtime discriminator through the real
/// resolution pipeline. P0 and P1 drew a card this way (recorded in
/// `player_actions_this_way`); P2 did NOT (declined the optional draw / empty
/// library). Resolving Kwain's parsed GainLife clause must gain 1 life for P0 and
/// P1 only, leaving P2 untouched.
///
/// Positive reach-guard (same test): P0 and P1 each go 20 → 21, proving the
/// scoped `GainLife` instruction is reached and applied to the drawers. Negative
/// discriminator (same test): P2 stays at 20. Reverting the parser fix restores
/// `player_scope: All`, which gains life for every player including the
/// non-drawer P2, flipping the P2 assertion.
///
/// Resolved at depth=1 (like `tempt_with_discovery.rs`) so the pre-populated
/// this-way ledger survives — a depth=0 top-level chain entry clears it.
#[test]
fn kwain_gain_life_reaches_only_the_players_who_drew() {
    let parsed = parse_kwain();
    let gain = parsed
        .abilities
        .iter()
        .find_map(gain_life_clause)
        .expect("Kwain must produce a GainLife clause");

    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    // P0 (controller) and P1 drew a card this way; P2 did not.
    state
        .player_actions_this_way
        .insert((PlayerId(0), PlayerActionKind::Draw));
    state
        .player_actions_this_way
        .insert((PlayerId(1), PlayerActionKind::Draw));

    let ability = build_resolved_from_def(gain, ObjectId(9000), PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

    assert_eq!(
        life(&state, PlayerId(0)),
        21,
        "the controller drew this way and must gain 1 life"
    );
    assert_eq!(
        life(&state, PlayerId(1)),
        21,
        "P1 drew this way and must gain 1 life"
    );
    assert_eq!(
        life(&state, PlayerId(2)),
        20,
        "P2 did NOT draw this way and must NOT gain life — a change to 21 means \
         the restriction was dropped and the life gain over-applied to every \
         player (player_scope All)"
    );
}

/// CR 121.1 + CR 608.2c + CR 109.5: End-to-end reach-guard through the production
/// activation pipeline. Three players, all with libraries, all accept Kwain's
/// optional "may draw"; each draws, records itself in the this-way ledger, and
/// then gains 1 life. Proves the parsed scope resolves correctly across the real
/// activate → per-player optional draw → "then" GainLife path (not just the
/// isolated clause).
#[test]
fn kwain_all_drawers_gain_life_through_activation() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for _ in 0..2 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Plains");
        scenario.add_card_to_library_top(PlayerId(2), "Forest");
    }

    let kwain = scenario
        .add_creature(P0, "Kwain, Itinerant Meddler", 1, 3)
        .from_oracle_text(KWAIN_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.activate(kwain, 0).accept_optional().resolve();

    let state = runner.state();
    for pid in [P0, P1, PlayerId(2)] {
        let player = state
            .players
            .iter()
            .find(|p| p.id == pid)
            .expect("player exists");
        assert_eq!(
            player.life, 21,
            "{pid:?} accepted the optional draw and must gain 1 life this way"
        );
        assert!(
            !player.hand.is_empty(),
            "{pid:?} must have drawn a card via Kwain's optional draw"
        );
    }
}
