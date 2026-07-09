//! S25 — Quick Draw and the opponent-constrained target-player slot.
//!
//! Quick Draw {R} Instant:
//!   "Target creature you control gets +1/+1 and gains first strike until end of
//!    turn. Creatures target opponent controls lose first strike and double strike
//!    until end of turn."
//!
//! Sentence 2 exercises `ControllerRef::TargetOpponent`: a declared-target player
//! scope whose companion slot offers ONLY opponents (CR 109.4 + CR 102.2 / 102.3).
//! Its runtime read is identical to `TargetPlayer`; the sole difference is the
//! companion slot's legal-target set (self excluded).

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const QUICK_DRAW: &str = "Target creature you control gets +1/+1 and gains first strike \
     until end of turn. Creatures target opponent controls lose first strike and double \
     strike until end of turn.";

/// Collect every `Effect` node reachable through an ability's chain (effect,
/// sub_ability, else_ability, mode_abilities).
fn collect_effects<'a>(
    ability: &'a engine::types::ability::AbilityDefinition,
    out: &mut Vec<&'a Effect>,
) {
    out.push(&ability.effect);
    if let Some(sub) = ability.sub_ability.as_deref() {
        collect_effects(sub, out);
    }
    if let Some(alt) = ability.else_ability.as_deref() {
        collect_effects(alt, out);
    }
    for mode in &ability.mode_abilities {
        collect_effects(mode, out);
    }
}

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// TEST 1 (parser round-trip). Sentence 2 lowers to a `GenericEffect { target:
/// None }` whose static removes first strike and double strike from creatures a
/// `TargetOpponent`-scoped filter selects — NOT `Effect::Unimplemented`.
///
/// Revert-fail: dropping the `"target opponent controls"` parser arm (Step 2)
/// makes this clause fall through to `Effect::unimplemented`, so both the
/// "no Unimplemented" and the "TargetOpponent static exists" assertions fire.
#[test]
fn quick_draw_sentence_two_parses_target_opponent_keyword_removal() {
    let parsed = parse_oracle_text(QUICK_DRAW, "Quick Draw", &[], &["Instant".to_string()], &[]);

    let mut effects = Vec::new();
    for ability in &parsed.abilities {
        collect_effects(ability, &mut effects);
    }

    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Unimplemented { .. })),
        "Quick Draw must fully parse — no Unimplemented node: {:#?}",
        parsed.abilities
    );

    let found = effects.iter().any(|effect| {
        let Effect::GenericEffect {
            static_abilities,
            target: None,
            ..
        } = effect
        else {
            return false;
        };
        static_abilities.iter().any(|static_def| {
            let Some(TargetFilter::Typed(tf)) = static_def.affected.as_ref() else {
                return false;
            };
            let opponent_scoped = matches!(
                tf.controller,
                Some(engine::types::ability::ControllerRef::TargetOpponent)
            );
            let removes_fs = static_def.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::RemoveKeyword {
                        keyword: Keyword::FirstStrike
                    }
                )
            });
            let removes_ds = static_def.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::RemoveKeyword {
                        keyword: Keyword::DoubleStrike
                    }
                )
            });
            opponent_scoped && removes_fs && removes_ds
        })
    });

    assert!(
        found,
        "expected a GenericEffect{{target:None}} with a TargetOpponent-scoped static \
         removing FirstStrike + DoubleStrike, got: {:#?}",
        parsed.abilities
    );
}

/// Cast Quick Draw, answering each target slot from its own legal set. Returns the
/// runner post-resolution plus the opponent-slot legal targets (for self-exclusion
/// assertions).
fn cast_quick_draw(
    scenario: GameScenario,
    my_creature: ObjectId,
    opponent: PlayerId,
) -> (GameRunner, Vec<TargetRef>) {
    let mut runner = scenario.build();
    // find the spell in P0's hand
    let spell = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .and_then(|p| p.hand.front().copied())
        .expect("Quick Draw is in P0's hand");
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free instant must succeed");

    let mut opponent_slot_legal = Vec::new();
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => {
            // Answer each slot in written order from its own legal set: the player
            // slot (legal set = players) gets the opponent; the creature slot gets
            // my creature.
            let mut answer = Vec::new();
            for slot in &target_slots {
                let is_player_slot = slot
                    .legal_targets
                    .iter()
                    .any(|t| matches!(t, TargetRef::Player(_)));
                if is_player_slot {
                    opponent_slot_legal = slot.legal_targets.clone();
                    answer.push(TargetRef::Player(opponent));
                } else {
                    answer.push(TargetRef::Object(my_creature));
                }
            }
            runner
                .act(GameAction::SelectTargets { targets: answer })
                .expect("targeting my creature + the opponent must succeed");
        }
        other => panic!("expected TargetSelection, got {other:?}"),
    }

    runner.advance_until_stack_empty();
    (runner, opponent_slot_legal)
}

/// TEST 2 (runtime, primary discriminator). Opponent's creature loses first strike
/// AND double strike this turn and regains both next turn (CR 611.2c EOT); my
/// creature gains +1/+1 and first strike.
///
/// Revert-fail: if Step 3c (the opponent-only companion-slot discriminator) is
/// reverted, the slot becomes bare `TargetFilter::Player`; nothing about *this*
/// assertion changes yet. The assertion that flips on reverting Step 2 (parser)
/// is the keyword-removal booleans below — with sentence 2 unimplemented the
/// opponent keeps first strike + double strike and both `!has_kw` asserts fail.
#[test]
fn quick_draw_strips_opponent_keywords_until_end_of_turn() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    // Cast in the POSTcombat main so `advance_to_upkeep()` below crosses this turn's
    // cleanup (CR 514.2) directly — there is no combat ahead to halt at
    // DeclareAttackers (an eligible untapped attacker would otherwise stop the
    // advance mid-turn, making the EOT-expiry assertion unreachable).
    scenario.at_phase(Phase::PostCombatMain);

    let my_creature = scenario.add_creature(P0, "My Bear", 2, 2).id();
    let opp_striker = scenario
        .add_creature(P1, "Opp Striker", 3, 3)
        .with_keyword(Keyword::FirstStrike)
        .with_keyword(Keyword::DoubleStrike)
        .id();

    scenario
        .add_spell_to_hand_from_oracle(P0, "Quick Draw", true, QUICK_DRAW)
        .with_mana_cost(ManaCost::zero());

    let (mut runner, _) = cast_quick_draw(scenario, my_creature, P1);

    // Opponent's creature lost BOTH keywords this turn.
    assert!(
        !has_kw(&mut runner, opp_striker, &Keyword::FirstStrike),
        "opponent creature must lose first strike this turn"
    );
    assert!(
        !has_kw(&mut runner, opp_striker, &Keyword::DoubleStrike),
        "opponent creature must lose double strike this turn"
    );

    // My creature gained +1/+1 and first strike.
    assert!(
        has_kw(&mut runner, my_creature, &Keyword::FirstStrike),
        "my creature must gain first strike"
    );
    {
        let obj = &runner.state().objects[&my_creature];
        assert_eq!(
            (obj.power, obj.toughness),
            (Some(3), Some(3)),
            "my 2/2 must be a 3/3 after +1/+1"
        );
    }

    // CR 611.2c + CR 514.2: the "until end of turn" set ends at cleanup; advancing
    // to the next turn's upkeep restores both keywords.
    runner.advance_to_upkeep();
    assert!(
        has_kw(&mut runner, opp_striker, &Keyword::FirstStrike),
        "opponent creature regains first strike next turn"
    );
    assert!(
        has_kw(&mut runner, opp_striker, &Keyword::DoubleStrike),
        "opponent creature regains double strike next turn"
    );
}

/// TEST 3 (hostile self-exclusion, rules-correctness proof). The companion slot for
/// "target opponent controls" offers opponents ONLY — self is excluded (CR 109.4 +
/// CR 102.2 / 102.3). In a 3-player game BOTH opponents are offered.
///
/// Revert-fail: this is the discriminator for Step 3c. Reverting 3c makes the slot
/// a bare `TargetFilter::Player`, which includes self — the
/// `!contains(Player(P0))` assertion fails.
#[test]
fn quick_draw_opponent_slot_excludes_self_offers_all_opponents() {
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);

    let my_creature = scenario.add_creature(P0, "My Bear", 2, 2).id();
    scenario.add_creature(P1, "Opp One", 1, 1);
    let p2 = PlayerId(2);
    scenario.add_creature(p2, "Opp Two", 1, 1);

    scenario
        .add_spell_to_hand_from_oracle(P0, "Quick Draw", true, QUICK_DRAW)
        .with_mana_cost(ManaCost::zero());

    let (_runner, opponent_slot_legal) = cast_quick_draw(scenario, my_creature, P1);

    assert!(
        !opponent_slot_legal.contains(&TargetRef::Player(P0)),
        "self (P0) must NOT be a legal target of the opponent slot: {opponent_slot_legal:?}"
    );
    assert!(
        opponent_slot_legal.contains(&TargetRef::Player(P1)),
        "opponent P1 must be offered: {opponent_slot_legal:?}"
    );
    assert!(
        opponent_slot_legal.contains(&TargetRef::Player(PlayerId(2))),
        "opponent P2 must be offered: {opponent_slot_legal:?}"
    );
}

/// TEST 4 (back-compat). A plain "target player controls" mass effect still offers
/// ALL players including self in its companion slot — the discriminator constrains
/// ONLY `TargetOpponent`, never `TargetPlayer`.
#[test]
fn target_player_controls_companion_slot_still_includes_self() {
    const MINUS: &str = "Creatures target player controls get -1/-1 until end of turn.";
    let mut scenario = GameScenario::new_n_player(2, 3);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "My Bear", 2, 2);
    scenario.add_creature(P1, "Their Bear", 2, 2);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Weaken All", true, MINUS)
        .with_mana_cost(ManaCost::zero());

    let mut runner = scenario.build();
    let spell = runner.state().players[0]
        .hand
        .front()
        .copied()
        .expect("spell in hand");
    let spell_card = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must succeed");

    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => {
            let player_slot = target_slots
                .iter()
                .find(|s| {
                    s.legal_targets
                        .iter()
                        .any(|t| matches!(t, TargetRef::Player(_)))
                })
                .expect("a player companion slot exists");
            assert!(
                player_slot.legal_targets.contains(&TargetRef::Player(P0)),
                "plain 'target player controls' must still offer self: {:?}",
                player_slot.legal_targets
            );
        }
        other => panic!("expected TargetSelection, got {other:?}"),
    }
}
