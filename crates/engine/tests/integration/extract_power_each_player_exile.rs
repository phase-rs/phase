//! Regression: "Extract Power" (Marvel Super Heroes Commander) must exile the
//! top card of EACH player's library, not just the controller's.
//!
//! Oracle (verbatim, verified against MTGJSON `MSC`):
//!   "Look at the top card of each player's library, then exile those cards face
//!    down. You may play them without paying their mana costs for as long as they
//!    remain exiled."
//!
//! Bug: the look-then-exile idiom ("look at the top card of each player's
//! library, then exile those cards") dropped the per-player scope. The `Dig`
//! owner recognizer (`parse_dig_library_owner`) had no "each player's library"
//! arm, so the `Dig` inherited `TargetFilter::Controller`; when the "then exile
//! those cards" continuation back-patched the `Dig` into an `Effect::ExileTop`,
//! that `ExileTop { player: Controller }` carried no `player_scope`, so at
//! runtime only the controller's top card was exiled.
//!
//! Fix (parser only): `parse_dig_library_owner` now maps "of each player's
//! library" to `TargetFilter::ScopedPlayer` (mirroring the direct-exile path's
//! `parse_library_player_suffix`), and the `ExileLookedAtCard` materialization
//! seam calls the shared `lift_distributive_exile_top_scope`, rewriting the
//! materialized `ExileTop { ScopedPlayer }` to `ExileTop { Controller }` +
//! `player_scope: All` — the exact production-proven shape of Etali / Nashi /
//! Lidless Gaze, whose `player_scope: All` fan-out (`resolve_chain_body`
//! rebinds the acting controller per player) exiles every library's top card.
//!
//! CR 401.1: each player's deck becomes their own library (one library per
//! player).
//! CR 406.3: the cards are exiled face down.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, PlayerFilter, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (MTGJSON `MSC`).
const EXTRACT_POWER: &str = "Look at the top card of each player's library, then exile those cards \
     face down. You may play them without paying their mana costs for as long as they remain exiled.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .zone
}

/// CR 401.1: Extract Power exiles the top card of EVERY player's library.
/// Three players, each library seeded with a distinct known top card and a
/// distinct second card. After resolution every player's TOP card is in exile
/// and every player's SECOND card is untouched in the library.
///
/// DISCRIMINATING ASSERTION: `zone_of(p1_top) == Exile` and
/// `zone_of(p2_top) == Exile`. Reverting either parser edit drops
/// `player_scope`, so the `ExileTop { player: Controller }` exiles only P0's top
/// card — P1's and P2's top cards stay in the library and both assertions fail.
/// The paired positive reach-guard `zone_of(p0_top) == Exile` proves the effect
/// resolved (not a vacuous no-op).
#[test]
fn extract_power_exiles_top_card_of_each_player() {
    let mut scenario = GameScenario::new_n_player(3, 7_431);
    scenario.at_phase(Phase::PreCombatMain);

    // Seed each library: add the "second" card first, then the "top" card, so
    // the top card ends up at library[0] and the second at library[1].
    let p0_second = scenario.add_card_to_library_top(P0, "P0 Second");
    let p0_top = scenario.add_card_to_library_top(P0, "P0 Top");
    let p1_second = scenario.add_card_to_library_top(P1, "P1 Second");
    let p1_top = scenario.add_card_to_library_top(P1, "P1 Top");
    let p2_second = scenario.add_card_to_library_top(P2, "P2 Second");
    let p2_top = scenario.add_card_to_library_top(P2, "P2 Top");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Extract Power", false, EXTRACT_POWER)
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Extract Power (no mana cost) must be accepted");
    runner.advance_until_stack_empty();

    // Positive reach-guard: the controller's top card IS exiled — the effect
    // resolved.
    assert_eq!(
        zone_of(&runner, p0_top),
        Zone::Exile,
        "the controller's top card must be exiled"
    );
    // Multi-authority discriminators: EVERY other player's top card is exiled
    // too. These flip to Library on revert (player_scope dropped).
    assert_eq!(
        zone_of(&runner, p1_top),
        Zone::Exile,
        "P1's top card must be exiled — each player's library, not just the controller's"
    );
    assert_eq!(
        zone_of(&runner, p2_top),
        Zone::Exile,
        "P2's top card must be exiled — each player's library, not just the controller's"
    );

    // Exactly the top card of each library leaves — the second card stays.
    assert_eq!(
        zone_of(&runner, p0_second),
        Zone::Library,
        "only the TOP card is exiled — P0's second card stays in the library"
    );
    assert_eq!(
        zone_of(&runner, p1_second),
        Zone::Library,
        "only the TOP card is exiled — P1's second card stays in the library"
    );
    assert_eq!(
        zone_of(&runner, p2_second),
        Zone::Library,
        "only the TOP card is exiled — P2's second card stays in the library"
    );

    // CR 406.3: the exiled cards are exiled face down.
    for top in [p0_top, p1_top, p2_top] {
        assert!(
            runner.state().objects[&top].face_down,
            "exiled cards must be face down (CR 406.3)"
        );
    }
}

#[test]
fn extract_power_all_player_exile_grants_p0_a_usable_cast_permission() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new_n_player(3, 7_433);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db);
    let extract_power = scenario
        .add_spell_to_hand_from_oracle(P0, "Extract Power", false, EXTRACT_POWER)
        .id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&extract_power].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: extract_power,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Extract Power must be castable");
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, p1_bears), Zone::Exile);
    assert!(
        legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == p1_bears
        )),
        "P0 must be able to cast P1's exiled card through Extract Power's tracked permission"
    );

    let card_id = runner.state().objects[&p1_bears].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: p1_bears,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 must be able to cast P1's exiled Grizzly Bears for free");
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, p1_bears), Zone::Battlefield);
}

/// Hostile fixture — CR 609.3 (an effect does only as much as possible; moving
/// cards out of a library moves as many as possible): a player with an EMPTY
/// library contributes nothing and does not error. P2's library is left empty;
/// P0 and P1 each have a top card. Resolution completes cleanly and the two
/// non-empty libraries' top cards are exiled.
#[test]
fn extract_power_empty_library_resolves_without_error() {
    let mut scenario = GameScenario::new_n_player(3, 7_432);
    scenario.at_phase(Phase::PreCombatMain);

    let p0_top = scenario.add_card_to_library_top(P0, "P0 Top");
    let p1_top = scenario.add_card_to_library_top(P1, "P1 Top");
    // P2: library intentionally left empty.

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Extract Power", false, EXTRACT_POWER)
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting must be accepted even with an empty opponent library");
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, p0_top),
        Zone::Exile,
        "the controller's top card is exiled"
    );
    assert_eq!(
        zone_of(&runner, p1_top),
        Zone::Exile,
        "the non-empty opponent's top card is exiled"
    );
    // P2's empty library is a no-op: the game did not panic and resolution
    // reached this assertion.
    assert!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P2)
            .expect("P2 present")
            .library
            .is_empty(),
        "P2's library stays empty — nothing to exile, no error"
    );
}

/// Find the `ExileTop` ability in a parsed card.
fn find_exile_top(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> &engine::types::ability::AbilityDefinition {
    parsed
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::ExileTop { .. }))
        .expect("an ExileTop ability must be present")
}

/// SHAPE (parser structure). Extract Power's verbatim Oracle text must parse to
/// the each-player fan-out shape: the ability carries `player_scope: All`, its
/// primary effect is `ExileTop { player: Controller, face_down: true }`, and its
/// sub-ability is the `CastFromZone` play permission.
///
/// Positive reach-guard: zero `Effect::Unimplemented` anywhere in the parse (so
/// the `player_scope` assertion can't pass vacuously on a parse failure) and the
/// `CastFromZone` sub is present.
///
/// DISCRIMINATING ASSERTION: `player_scope == Some(PlayerFilter::All)`. Reverting
/// either parser edit leaves it `None`.
///
/// Sibling non-regression: Lidless Gaze reaches Extract Power via the DIRECT
/// "exile the top card of each player's library" path (`parse_library_player_suffix`),
/// which these edits do not touch — it must still parse to the identical
/// `ExileTop { Controller }` + `player_scope: All` shape.
#[test]
fn extract_power_ability_carries_all_player_scope() {
    let parsed = parse_oracle_text(
        EXTRACT_POWER,
        "Extract Power",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "Extract Power must parse with zero Unimplemented nodes; got:\n{dbg}"
    );

    let ability = find_exile_top(&parsed);
    assert_eq!(
        ability.player_scope,
        Some(PlayerFilter::All),
        "the each-player look-then-exile ability must carry player_scope: All;\n{dbg}"
    );
    match &*ability.effect {
        Effect::ExileTop {
            player, face_down, ..
        } => {
            assert_eq!(
                *player,
                TargetFilter::Controller,
                "the lifted ExileTop rebinds the acting player to Controller (re-scoped per fan-out iteration)"
            );
            assert!(
                *face_down,
                "Extract Power exiles the cards face down (CR 406.3)"
            );
        }
        other => panic!("expected ExileTop, got {other:?}"),
    }
    // The "you may play them ..." permission must survive as the sub-ability.
    let sub = ability
        .sub_ability
        .as_deref()
        .expect("the play permission must attach as a sub-ability");
    assert!(
        matches!(&*sub.effect, Effect::CastFromZone { .. }),
        "the sub-ability must be the CastFromZone play permission; got {:?}",
        sub.effect
    );

    // Sibling non-regression: the untouched direct-exile path is unchanged.
    let lidless = parse_oracle_text(
        "Exile the top card of each player's library. Until the end of your next turn, you may \
         play those cards, and mana of any type can be spent to cast those spells.\nFlashback \
         {2}{B}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)",
        "Lidless Gaze",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let lidless_dbg = format!("{lidless:#?}");
    let lidless_exile = find_exile_top(&lidless);
    assert_eq!(
        lidless_exile.player_scope,
        Some(PlayerFilter::All),
        "the direct-exile sibling (Lidless Gaze) must still carry player_scope: All;\n{lidless_dbg}"
    );
    assert!(
        matches!(
            &*lidless_exile.effect,
            Effect::ExileTop {
                player: TargetFilter::Controller,
                ..
            }
        ),
        "the direct-exile sibling's ExileTop must remain player: Controller;\n{lidless_dbg}"
    );
}
