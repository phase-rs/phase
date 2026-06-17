//! Regression guard for Peer into the Abyss (M21, {4}{B}{B}{B} sorcery):
//! "Target player draws cards equal to half the number of cards in their
//! library and loses half their life. Round up each time."
//!
//! Two coordinated parser changes make this card resolve correctly:
//!
//!   A. The clause splitter must split the conjugated "draws ... and loses
//!      ... life" conjunction whose second conjunct's count head is a fraction
//!      word ("half their life"). The fraction divisor is the split
//!      discriminator (`next_token_is_player_action_count` /
//!      `next_token_is_count` in `oracle_effect/sequence.rs`).
//!   B. The draw count "cards equal to half the number of cards in their
//!      library" must route through `parse_fraction_rounded` FIRST so it
//!      becomes a `DivideRounded` over the target's library count, before the
//!      generic semantic `parse_quantity_ref` fallback
//!      (`parse_dynamic_count_phrase` in `oracle_effect/imperative.rs`).
//!
//! The trailing "Round up each time." flips both `DivideRounded.rounding`
//! Down→Up. Library = 9 → ceil(9/2) = 5 drawn (Down would be 4); life = 7 →
//! ceil(7/2) = 4 lost, ending 3 (Down would lose 3). The odd counts
//! discriminate Up from Down. The caster being a distinct player from the
//! target discriminates target threading from a controller-default bind.
//!
//! This test drives the REAL cast pipeline (GameScenario + GameRunner::cast)
//! with `add_real_card` + rehydrate, NOT `from_oracle_text`, because the
//! conjunction splitter (change A) only runs on the database parse path; a
//! `from_oracle_text` round-trip would mask it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const PEER: &str = "Peer into the Abyss";
const TAMIYO: &str = "Tamiyo, Seasoned Scholar";

#[test]
fn peer_into_the_abyss_draws_and_loses_half_rounded_up_against_target() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Caster is P0. Target is the distinct player P1 (CR 115.1) — keeping the
    // subject elided across "draws ... and loses ..." against a SINGLE shared
    // target. Caster must be unaffected.
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 7); // odd → ceil(7/2) = 4 lost → ends at 3.

    let peer = scenario.add_real_card(P0, PEER, Zone::Hand, db);

    // Target library = 9 (odd → ceil(9/2) = 5 drawn). Use real cards so they
    // carry full card data; `add_real_card` push_backs onto the library.
    for _ in 0..9 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    // Caster needs SOME library so a draw-from-empty SBA can't muddy the test
    // (the caster never draws here, but keep the state well-formed).
    for _ in 0..5 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
    }

    // Fund {4}{B}{B}{B} from P0's pool (auto-pay): 3 Black + 4 Colorless.
    let mut pool: Vec<ManaUnit> = (0..3)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect();
    pool.extend((0..4).map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let target_lib_before = runner.state().players[1].library.len();
    assert_eq!(target_lib_before, 9, "target library should start at 9");

    // 4. No Effect::Unimplemented in the parsed ability chain — proves both the
    //    conjunction split (change A) and the fraction-first draw-count routing
    //    (change B) produced concrete effects rather than a parse gap. Walk the
    //    DB-parsed spell ability tree (effect + sub_ability + else_ability),
    //    mirroring the Pox Plague `walk_no_unimpl` precedent.
    let face = db.get_face_by_name(PEER).expect("Peer face in DB");
    for def in &face.abilities {
        assert_no_unimplemented(def);
    }

    let outcome = runner.cast(peer).target_player(P1).resolve();

    // 1. Target threading: the caster (P0) neither drew nor lost life. This
    //    discriminates a correct single-shared-target bind from a
    //    controller-default bind that would hit the caster.
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_life_delta(P0, 0);

    // 2. Draw count = ceil(9/2) = 5 (DivideRounded Up over the TARGET's library
    //    count). Down would be 4. Library drops by exactly 5.
    outcome.assert_hand_drawn(P1, 5);
    let target_lib_after = outcome.state().players[1].library.len();
    assert_eq!(
        target_lib_after,
        target_lib_before - 5,
        "target library must drop by 5 (ceil(9/2)), got {} -> {target_lib_after}",
        target_lib_before
    );

    // 3. Life loss = ceil(7/2) = 4 (DivideRounded Up over PlayerScope::Target
    //    life), ending at 3. Down would lose 3 (ending 4).
    outcome.assert_life_delta(P1, -4);
    assert_eq!(
        outcome.state().players[1].life,
        3,
        "target life must end at 3 (lost ceil(7/2) = 4 from 7)"
    );
}

/// CR 107.1a + CR 109.5 + CR 121.1: class-coverage sibling of Peer. Tamiyo,
/// Seasoned Scholar's −7: "Draw cards equal to half the number of cards in
/// your library, rounded up." Exercises ONLY Change B (single-clause loyalty
/// ability — no conjunction split), with CONTROLLER scope ("your library", vs
/// Peer's "their") and an INLINE ", rounded up" suffix consumed by
/// `parse_fraction_rounded` directly (vs Peer's trailing "Round up each
/// time." rewrite). Library = 9 → ceil(9/2) = 5 drawn by the CONTROLLER.
#[test]
fn tamiyo_minus_seven_draws_half_library_rounded_up_for_controller() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tamiyo = scenario.add_real_card(P0, TAMIYO, Zone::Battlefield, db);
    // Seed enough loyalty to pay the −7 (lands at 0, which is legal).
    scenario.with_counter(tamiyo, CounterType::Loyalty, 7);

    // Controller (P0) library = 9 (odd → ceil(9/2) = 5 drawn, Up).
    for _ in 0..9 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
    }
    // Opponent needs a library so SBAs don't fire.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // No Effect::Unimplemented in Tamiyo's parsed −7 chain.
    let face = db.get_face_by_name(TAMIYO).expect("Tamiyo face in DB");
    for def in &face.abilities {
        assert_no_unimplemented(def);
    }

    let lib_before = runner.state().players[0].library.len();
    assert_eq!(lib_before, 9, "controller library should start at 9");

    // Locate the −7 ability index on the battlefield object (don't hardcode).
    let minus_seven = runner.state().objects[&tamiyo]
        .abilities
        .iter()
        .position(is_minus_seven_draw)
        .expect("Tamiyo must expose its −7 draw ability after the fix");

    let outcome = runner.activate(tamiyo, minus_seven).resolve();

    // Controller drew ceil(9/2) = 5 (DivideRounded Up over ZoneCardCount
    // Controller library). Down would be 4. Library drops by exactly 5.
    outcome.assert_hand_drawn(P0, 5);
    let lib_after = outcome.state().players[0].library.len();
    assert_eq!(
        lib_after,
        lib_before - 5,
        "controller library must drop by 5 (ceil(9/2)), got {lib_before} -> {lib_after}"
    );

    // The opponent is unaffected (controller-scope draw, not a global draw).
    outcome.assert_hand_drawn(P1, 0);
}

/// True when an ability is Tamiyo's −7 loyalty draw (a Loyalty cost of −7 whose
/// effect is a `Draw` carrying a `DivideRounded` count).
fn is_minus_seven_draw(def: &engine::types::ability::AbilityDefinition) -> bool {
    use engine::types::ability::{AbilityCost, Effect, QuantityExpr};
    let is_minus_seven = matches!(def.cost, Some(AbilityCost::Loyalty { amount: -7 }));
    let is_fraction_draw = matches!(
        &*def.effect,
        Effect::Draw {
            count: QuantityExpr::DivideRounded { .. },
            ..
        }
    );
    is_minus_seven && is_fraction_draw
}

/// Asserts no `Effect::Unimplemented` anywhere in an ability definition chain
/// (effect + `sub_ability` + `else_ability`). Mirrors the Pox Plague
/// `walk_no_unimpl` precedent in `parser/oracle.rs`.
fn assert_no_unimplemented(def: &engine::types::ability::AbilityDefinition) {
    use engine::types::ability::Effect;
    assert!(
        !matches!(*def.effect, Effect::Unimplemented { .. }),
        "Effect::Unimplemented in Peer into the Abyss chain: {:?}",
        def.effect
    );
    if let Some(sub) = def.sub_ability.as_ref() {
        assert_no_unimplemented(sub);
    }
    if let Some(els) = def.else_ability.as_ref() {
        assert_no_unimplemented(els);
    }
}
