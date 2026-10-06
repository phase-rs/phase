//! Welcome the Dead — runtime regression for the where-X quantity "the number of
//! cards that were put into your graveyard from your hand or library this turn".
//!
//! Oracle text (verbatim): "Draw two cards, then discard a card and you lose 2
//! life. Create X tapped 2/2 black Zombie Druid creature tokens, where X is the
//! number of cards that were put into your graveyard from your hand or library
//! this turn.\nFlashback {5}{B}"
//!
//! CR 107.3c: X defined by the spell's text is evaluated at resolution, after
//! the discard. CR 701.9a: discard moves hand -> graveyard. CR 701.17a: mill
//! moves library -> graveyard. CR 404.1 + CR 111.7: only cards owned by the
//! controller count; tokens are not cards. Revert-failing assertion: without the
//! zone-list quantity combinator X is `Unimplemented` and no tokens are made.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{Effect, QuantityExpr, QuantityRef};
use engine::types::game_state::CastingVariant;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WELCOME_THE_DEAD: &str = "Draw two cards, then discard a card and you lose 2 life. Create X tapped 2/2 black Zombie Druid creature tokens, where X is the number of cards that were put into your graveyard from your hand or library this turn.\nFlashback {5}{B}";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

struct Staged {
    runner: GameRunner,
    spell: ObjectId,
    discard_target: ObjectId,
    /// Library -> graveyard before the cast (mill).
    milled: ObjectId,
    /// Hand -> exile before the cast (not a graveyard move).
    exiled_from_hand: ObjectId,
}

struct Options {
    rest_in_peace: bool,
    from_graveyard: bool,
}

/// Stage Welcome the Dead plus a mix of prior-this-turn zone changes, each moved
/// through the production `move_to_zone` path so they are recorded exactly as in a
/// real game.
fn stage(opts: Options) -> Staged {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = if opts.from_graveyard {
        scenario
            .add_spell_to_graveyard(P0, "Welcome the Dead", false)
            .from_oracle_text(WELCOME_THE_DEAD)
            .id()
    } else {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Welcome the Dead", false, WELCOME_THE_DEAD)
            .id()
    };
    let discard_target = scenario.add_card_to_hand(P0, "Discard Fodder");
    scenario.add_card_to_hand(P0, "Hand Filler One");
    scenario.add_card_to_hand(P0, "Hand Filler Two");
    let exiled_from_hand = scenario.add_card_to_hand(P0, "Exiled From Hand");
    let opp_card = scenario.add_card_to_hand(P1, "Opponent Discard");
    // Library (top first): two draws, the milled card, padding.
    scenario.with_library_top(
        P0,
        &["Draw One", "Draw Two", "Milled Card", "Pad A", "Pad B"],
    );
    for _ in 0..3 {
        scenario.add_card_to_library_top(P1, "Opp Pad");
    }
    let dying_creature = scenario.add_vanilla(P0, 1, 1);
    let dying_token = scenario.add_vanilla(P0, 1, 1);
    if opts.rest_in_peace {
        scenario
            .add_creature(P0, "Rest in Peace", 0, 0)
            .as_enchantment()
            .from_oracle_text(REST_IN_PEACE);
    }
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let mut runner = scenario.build();

    let milled = runner.state().players[0].library[2];
    let mut events = Vec::new();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&dying_token).unwrap().is_token = true;
        if !opts.rest_in_peace {
            // Prior-this-turn moves that SHOULD count (library -> graveyard) ...
            move_to_zone(state, milled, Zone::Graveyard, &mut events);
        }
        // ... and ones that must NOT count.
        move_to_zone(state, exiled_from_hand, Zone::Exile, &mut events);
        move_to_zone(state, opp_card, Zone::Graveyard, &mut events);
        move_to_zone(state, dying_creature, Zone::Graveyard, &mut events);
        move_to_zone(state, dying_token, Zone::Graveyard, &mut events);
        if opts.from_graveyard {
            state.add_mana_to_pool(
                P0,
                ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            );
            for _ in 0..5 {
                state.add_mana_to_pool(
                    P0,
                    ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                );
            }
        }
    }
    Staged {
        runner,
        spell,
        discard_target,
        milled,
        exiled_from_hand,
    }
}

fn cast(staged: &mut Staged, from_graveyard: bool) -> CastOutcome {
    let mut cast = staged
        .runner
        .cast(staged.spell)
        .discard(&[staged.discard_target]);
    if from_graveyard {
        cast = cast.casting_variant(CastingVariant::Flashback);
    }
    cast.resolve()
}

fn zombie_druid_tokens(outcome: &CastOutcome) -> Vec<ObjectId> {
    outcome
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &outcome.state().objects[id];
            obj.is_token && obj.controller == P0 && obj.name == "Zombie Druid"
        })
        .collect()
}

fn assert_zombie_druid_shape(outcome: &CastOutcome, tokens: &[ObjectId]) {
    for id in tokens {
        let obj = &outcome.state().objects[id];
        assert!(obj.tapped, "tokens enter tapped");
        assert_eq!((obj.power, obj.toughness), (Some(2), Some(2)));
        assert_eq!(obj.color, vec![ManaColor::Black]);
        assert!(obj.card_types.subtypes.iter().any(|s| s == "Zombie"));
        assert!(obj.card_types.subtypes.iter().any(|s| s == "Druid"));
    }
}

/// A: the Sum arms are exercised separately (mill = library -> graveyard, discard
/// = hand -> graveyard) and everything else this turn is excluded.
#[test]
fn welcome_the_dead_counts_mill_and_discard_only() {
    let mut staged = stage(Options {
        rest_in_peace: false,
        from_graveyard: false,
    });
    assert_eq!(
        staged.runner.state().objects[&staged.milled].zone,
        Zone::Graveyard,
        "setup: the milled card is in the graveyard"
    );
    assert_eq!(
        staged.runner.state().objects[&staged.exiled_from_hand].zone,
        Zone::Exile,
        "setup: the hand card went to exile, not the graveyard"
    );
    let outcome = cast(&mut staged, false);

    let tokens = zombie_druid_tokens(&outcome);
    // 1 mill (library) + 1 discard (hand); the exiled hand card, the opponent's
    // card, the dead creature, and the dead token are excluded.
    assert_eq!(tokens.len(), 2, "X = milled card + discarded card");
    assert_zombie_druid_shape(&outcome, &tokens);
    outcome.assert_life_delta(P0, -2);
    outcome.assert_hand_drawn(P0, 1); // +2 drawn, -1 discarded
    outcome.assert_zone(&[staged.discard_target], Zone::Graveyard);
}

/// X = 0: Rest in Peace sends the discard hand -> exile, so nothing was put into
/// the graveyard. Paired with the control scenario above (same staging, no
/// replacement) which makes tokens, proving the 0 is the quantity and not a
/// short-circuit.
#[test]
fn welcome_the_dead_with_graveyard_replacement_makes_no_tokens() {
    let mut control = stage(Options {
        rest_in_peace: false,
        from_graveyard: false,
    });
    let control_outcome = cast(&mut control, false);
    assert_eq!(zombie_druid_tokens(&control_outcome).len(), 2, "control");

    let mut staged = stage(Options {
        rest_in_peace: true,
        from_graveyard: false,
    });
    let outcome = cast(&mut staged, false);
    outcome.assert_zone(&[staged.discard_target], Zone::Exile);
    assert!(zombie_druid_tokens(&outcome).is_empty(), "X = 0");
    outcome.assert_life_delta(P0, -2);
}

/// Flashback exiles the spell; X is computed identically (the spell itself is
/// never a hand/library -> graveyard move).
#[test]
fn welcome_the_dead_flashback_yields_same_x() {
    let mut staged = stage(Options {
        rest_in_peace: false,
        from_graveyard: true,
    });
    let outcome = cast(&mut staged, true);
    outcome.assert_zone(&[staged.spell], Zone::Exile);
    let tokens = zombie_druid_tokens(&outcome);
    assert_eq!(tokens.len(), 2, "flashback X matches the hand cast");
    assert_zombie_druid_shape(&outcome, &tokens);
}

/// Per-turn tracking: a library -> graveyard move from a previous turn is not
/// counted. Paired with the same-turn control (test A).
#[test]
fn welcome_the_dead_ignores_previous_turn_moves() {
    let mut staged = stage(Options {
        rest_in_peace: false,
        from_graveyard: false,
    });
    // End the turn boundary the production way: records are cleared per turn.
    {
        let state = staged.runner.state_mut();
        assert!(
            !state.zone_changes_this_turn.is_empty(),
            "reach-guard: records exist before the turn boundary"
        );
        let mut events = Vec::new();
        engine::game::turns::start_next_turn(state, &mut events);
        assert!(
            state
                .zone_changes_this_turn
                .iter()
                .all(|r| r.object_id != staged.milled),
            "the previous turn's mill record is gone"
        );
        // Resume P0's precombat main for the sorcery-speed cast.
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P0 };
    }
    let outcome = cast(&mut staged, false);
    // Only the discard (this turn) counts.
    assert_eq!(zombie_druid_tokens(&outcome).len(), 1, "X = discard only");
}

/// The Create clause parses fully typed: its X is the Sum of the two origin refs
/// and the " this turn" stays with the quantity (no stray effect duration).
#[test]
fn welcome_the_dead_parse_keeps_this_turn_with_the_quantity() {
    let mut scenario = GameScenario::new();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Welcome the Dead", false, WELCOME_THE_DEAD)
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
        if let Effect::Token { count, .. } = &*def.effect {
            found = true;
            assert!(
                matches!(count, QuantityExpr::Sum { exprs } if exprs.len() == 2 && exprs.iter().all(|e| matches!(e, QuantityExpr::Ref { qty: QuantityRef::ZoneChangeCountThisTurn { .. } }))),
                "X must be the per-origin Sum, got {count:?}"
            );
            assert_eq!(
                def.duration, None,
                "'this turn' belongs to the quantity, not an effect duration"
            );
        }
        match def.sub_ability.clone() {
            Some(next) => def = *next,
            None => break,
        }
    }
    assert!(found, "a Token effect must be present");
}
