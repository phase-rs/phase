//! Cruel Calculations — runtime regression for the where-X quantity "the number
//! of cards that were put into target player's graveyard from their library
//! this turn".
//!
//! Oracle text (verbatim): "Draw X cards, where X is the number of cards that
//! were put into target player's graveyard from their library this turn."
//!
//! CR 107.3c: X defined by the spell's text is evaluated at resolution.
//! CR 701.17a: mill moves library -> graveyard. CR 121.1: a player draws by
//! putting the top card of their library into their hand. Revert-failing
//! assertion: without the target-player zone-list quantity combinator X stays
//! `Unimplemented` and no cards are drawn.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{ControllerRef, Effect, FilterProp, QuantityExpr, QuantityRef};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CRUEL_CALCULATIONS: &str = "Draw X cards, where X is the number of cards that were put into target player's graveyard from their library this turn.";

struct Staged {
    runner: GameRunner,
    spell: ObjectId,
    milled: Vec<ObjectId>,
}

/// Stage Cruel Calculations plus a prior-this-turn mill of `mill_count` cards
/// from `mill_owner`'s library, each moved through the production
/// `move_to_zone` path so it is recorded exactly as in a real game.
fn stage(mill_owner: PlayerId, mill_count: usize) -> Staged {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cruel Calculations", false, CRUEL_CALCULATIONS)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    // P0's library (top first): drawn cards, then padding.
    scenario.with_library_top(
        P0,
        &["Draw One", "Draw Two", "Draw Three", "Pad A", "Pad B"],
    );
    // P1's library (top first): milled cards, then padding.
    scenario.with_library_top(
        P1,
        &["Mill One", "Mill Two", "Mill Three", "Pad A", "Pad B"],
    );
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    let mut milled = Vec::new();
    {
        let libs: Vec<ObjectId> = {
            let state = runner.state();
            let player = state
                .players
                .iter()
                .find(|p| p.id == mill_owner)
                .expect("player exists");
            (0..mill_count).map(|i| player.library[i]).collect()
        };
        let state = runner.state_mut();
        let mut events = Vec::new();
        for id in libs {
            move_to_zone(state, id, Zone::Graveyard, &mut events);
            milled.push(id);
        }
    }
    Staged {
        runner,
        spell,
        milled,
    }
}

fn cast_targeting_p1(staged: &mut Staged) -> CastOutcome {
    staged.runner.cast(staged.spell).target_player(P1).resolve()
}

/// The milled target player's count drives the draw: mill 3 from P1, target
/// P1, draw 3.
#[test]
fn cruel_calculations_draws_milled_count_for_targeted_player() {
    let mut staged = stage(P1, 3);
    assert_eq!(staged.milled.len(), 3, "setup: three cards milled");
    for id in &staged.milled {
        assert_eq!(
            staged.runner.state().objects[id].zone,
            Zone::Graveyard,
            "setup: milled card is in the graveyard"
        );
    }
    let outcome = cast_targeting_p1(&mut staged);
    outcome.assert_hand_drawn(P0, 3);
}

/// The drawer is the caster (P0), never the targeted player: P1 draws nothing
/// and P0's library shrinks by exactly the drawn count.
#[test]
fn cruel_calculations_drawer_is_caster_not_target() {
    let mut staged = stage(P1, 2);
    let lib_before = staged.runner.state().players[0].library.len();
    let outcome = cast_targeting_p1(&mut staged);
    // CR 121.1: the draw puts the top cards of P0's library into P0's hand.
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_hand_drawn(P1, 0);
    assert_eq!(
        outcome.state().players[0].library.len(),
        lib_before - 2,
        "P0's library shrinks by the drawn count"
    );
}

/// Hostile: only P0's own library was milled, so targeting P1 counts 0. Paired
/// with the positive control above (same staging shape, P1 milled) which draws,
/// proving the 0 is the quantity and not a short-circuit.
#[test]
fn cruel_calculations_ignores_other_players_mill() {
    let mut control = stage(P1, 2);
    let control_outcome = cast_targeting_p1(&mut control);
    control_outcome.assert_hand_drawn(P0, 2);

    let mut staged = stage(P0, 2);
    assert_eq!(staged.milled.len(), 2, "reach-guard: P0 milled two");
    let outcome = cast_targeting_p1(&mut staged);
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_hand_drawn(P1, 0);
}

/// The Draw clause parses fully typed: its X is the TargetPlayer-owned
/// Library -> Graveyard ref and no clause stays unimplemented.
#[test]
fn cruel_calculations_parse_binds_target_player_mill_count() {
    let mut scenario = GameScenario::new();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cruel Calculations", false, CRUEL_CALCULATIONS)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&spell];
    let mut def = obj.abilities.first().expect("spell ability").clone();
    let mut found = false;
    loop {
        assert!(
            !matches!(&*def.effect, Effect::Unimplemented { .. }),
            "no clause may stay unimplemented"
        );
        if let Effect::Draw { count, .. } = &*def.effect {
            found = true;
            assert!(
                matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeCountThisTurn {
                            from: Some(Zone::Library),
                            to: Some(Zone::Graveyard),
                            ..
                        }
                    }
                ),
                "X must be the Library -> Graveyard zone-change count, got {count:?}"
            );
            let QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn { filter, .. },
            } = count
            else {
                unreachable!()
            };
            assert!(
                matches!(filter, engine::types::ability::TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::Owned { controller: ControllerRef::TargetPlayer }) && tf.properties.contains(&FilterProp::NonToken)),
                "count must be owned by the targeted player, got {filter:?}"
            );
        }
        match def.sub_ability.clone() {
            Some(next) => def = *next,
            None => break,
        }
    }
    assert!(found, "a Draw effect must be present");
}

/// The clause parses with no warnings: the card is supported, not a gap.
#[test]
fn cruel_calculations_parses_without_warnings() {
    let parsed = engine::parser::parse_oracle_text(
        CRUEL_CALCULATIONS,
        "Cruel Calculations",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "Cruel Calculations must parse cleanly, got {:?}",
        parsed.parse_warnings
    );
}
