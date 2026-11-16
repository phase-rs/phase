//! CR 400.7 + CR 700.2: "choose one that hasn't been chosen [this turn]"
//! history belongs to one object. A permanent that leaves the battlefield and
//! returns is a new object with no memory of its previous existence, so every
//! mode is available again. A control change is not a zone change and keeps
//! the history. CR 603.10a: a leaves-the-battlefield trigger that looks back
//! from a source that left at the same time still reads the departed
//! incarnation's history.
//!
//! The engine reuses `ObjectId` as storage identity across zone moves, so the
//! history is keyed by `ObjectIncarnationRef` (storage id + incarnation epoch).
//!
//! Oracle (Gandalf the Grey):
//!   Whenever you cast an instant or sorcery spell, choose one that hasn't been
//!   chosen —
//!   • You may tap or untap target permanent.
//!   • Gandalf the Grey deals 3 damage to each opponent.
//!   • Copy target instant or sorcery spell you control. You may choose new
//!     targets for the copy.
//!   • Put Gandalf the Grey on top of its owner's library.
//!
//! Oracle (Genku, Future Shaper — modal trigger):
//!   Whenever another nontoken permanent you control leaves the battlefield,
//!   choose one that hasn't been chosen this turn —
//!   • Create a 2/2 white Fox creature token with vigilance.
//!   • Create a 1/2 blue Moonfolk creature token with flying.
//!   • Create a 1/1 black Rat creature token with lifelink.
//!
//! Oracle (Ghostly Flicker):
//!   Exile two target artifacts, creatures, and/or lands you control, then
//!   return those cards to the battlefield under your control.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef, LEGACY_INCARNATION};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GANDALF_ORACLE: &str = "Whenever you cast an instant or sorcery spell, choose one that hasn't been chosen \u{2014}\n\
\u{2022} You may tap or untap target permanent.\n\
\u{2022} Gandalf the Grey deals 3 damage to each opponent.\n\
\u{2022} Copy target instant or sorcery spell you control. You may choose new targets for the copy.\n\
\u{2022} Put Gandalf the Grey on top of its owner's library.";

const GENKU_ORACLE: &str = "Whenever another nontoken permanent you control leaves the battlefield, choose one that hasn't been chosen this turn \u{2014}\n\
\u{2022} Create a 2/2 white Fox creature token with vigilance.\n\
\u{2022} Create a 1/2 blue Moonfolk creature token with flying.\n\
\u{2022} Create a 1/1 black Rat creature token with lifelink.";

const LIFE_ORACLE: &str = "You gain 1 life.";
const FODDER_ORACLE: &str = "Sacrifice this creature: You gain 1 life.";
const WIPE_ORACLE: &str = "Destroy all creatures.";
const GHOSTLY_FLICKER_ORACLE: &str =
    "Exile two target artifacts, creatures, and/or lands you control, \
     then return those cards to the battlefield under your control.";
const THREATEN_ORACLE: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.";

/// Gandalf's "deals 3 damage to each opponent" mode.
const DAMAGE_MODE: usize = 1;
/// Genku's token modes, in printed order.
const FOX: usize = 0;
const MOONFOLK: usize = 1;
const RAT: usize = 2;

/// One `AbilityModeChoice` prompt the engine surfaced.
#[derive(Debug, Clone, PartialEq)]
struct ModePrompt {
    player: PlayerId,
    source_id: ObjectId,
    unavailable: Vec<usize>,
}

/// Drive the engine from its current wait until the stack is empty, answering
/// trigger ordering, the single object target `target` (if any), and every
/// modal prompt through `pick`. `pick` returning `None` stops at that prompt
/// (left pending). Returns every mode prompt seen, in order.
fn drive(
    runner: &mut GameRunner,
    target: Option<ObjectId>,
    mut pick: impl FnMut(&ModePrompt) -> Option<Vec<usize>>,
) -> Vec<ModePrompt> {
    let mut prompts = Vec::new();
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering triggers must be accepted");
            }
            WaitingFor::AbilityModeChoice {
                player,
                source_id,
                unavailable_modes,
                ..
            } => {
                let prompt = ModePrompt {
                    player,
                    source_id,
                    unavailable: unavailable_modes,
                };
                let answer = pick(&prompt);
                prompts.push(prompt);
                let Some(indices) = answer else {
                    return prompts;
                };
                runner
                    .act(GameAction::SelectModes { indices })
                    .expect("choosing an available mode must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                let target = target.expect("a target prompt needs a declared target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(target)),
                    })
                    .expect("the declared target must be legal");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return prompts,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
            }
            other => panic!("unexpected wait: {}", other.variant_name()),
        }
    }
    panic!("the stack did not empty");
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must be accepted");
}

fn activate(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activating the ability must be accepted");
}

/// Effect-driven move through the replacement-aware zone pipeline.
fn move_through_pipeline(runner: &mut GameRunner, object: ObjectId, to: Zone) {
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(object, to, object),
            &mut events,
        ),
        "the move to {to:?} must complete without a replacement choice"
    );
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn incarnation(runner: &GameRunner, object: ObjectId) -> u64 {
    runner.state().objects[&object].incarnation
}

// ---------------------------------------------------------------------------
// Gandalf the Grey (NoRepeatThisGame)
// ---------------------------------------------------------------------------

struct Gandalf {
    runner: GameRunner,
    gandalf: ObjectId,
    instants: Vec<ObjectId>,
    threaten: ObjectId,
    p1_instant: ObjectId,
}

fn gandalf_board() -> Gandalf {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gandalf = scenario
        .add_creature_from_oracle(P0, "Gandalf the Grey", 4, 5, GANDALF_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let instants = (0..2)
        .map(|i| {
            scenario
                .add_spell_to_hand_from_oracle(P0, &format!("Life Instant {i}"), true, LIFE_ORACLE)
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let threaten = scenario
        .add_spell_to_hand_from_oracle(P1, "Threaten Instant", true, THREATEN_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_instant = scenario
        .add_spell_to_hand_from_oracle(P1, "P1 Life Instant", true, LIFE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    Gandalf {
        runner: scenario.build(),
        gandalf,
        instants,
        threaten,
        p1_instant,
    }
}

/// Cast `instant` and answer Gandalf's prompt with the damage mode.
fn cast_choosing_damage(b: &mut Gandalf, instant: ObjectId) -> Vec<ModePrompt> {
    cast(&mut b.runner, instant);
    drive(&mut b.runner, None, |_| Some(vec![DAMAGE_MODE]))
}

/// Cast `instant` and stop at the first mode prompt.
fn cast_until_prompt(b: &mut Gandalf, instant: ObjectId) -> ModePrompt {
    cast(&mut b.runner, instant);
    let prompts = drive(&mut b.runner, None, |_| None);
    assert_eq!(prompts.len(), 1, "reach: exactly one mode prompt surfaces");
    prompts.into_iter().next().unwrap()
}

/// First cast: every mode open, the damage mode resolves.
fn first_damage(b: &mut Gandalf) -> i32 {
    let life_start = life(&b.runner, P1);
    let first = b.instants[0];
    let prompts = cast_choosing_damage(b, first);
    assert_eq!(
        prompts,
        vec![ModePrompt {
            player: P0,
            source_id: b.gandalf,
            unavailable: vec![],
        }],
        "first trigger: every mode is open"
    );
    assert_eq!(
        life(&b.runner, P1),
        life_start - 3,
        "reach: the damage mode resolved"
    );
    life_start
}

/// Control: with no zone change, the second trigger cannot repeat the damage
/// mode.
#[test]
fn gandalf_mode_stays_unavailable_without_zone_change() {
    let mut b = gandalf_board();
    first_damage(&mut b);

    let second = b.instants[1];
    let prompt = cast_until_prompt(&mut b, second);
    assert_eq!(
        prompt.source_id, b.gandalf,
        "reach: the prompt is Gandalf's"
    );
    assert_eq!(
        prompt.unavailable,
        vec![DAMAGE_MODE],
        "CR 700.2: the same object cannot choose the damage mode twice"
    );
    assert!(
        b.runner
            .act(GameAction::SelectModes {
                indices: vec![DAMAGE_MODE],
            })
            .is_err(),
        "CR 700.2: re-choosing a used mode is rejected"
    );
}

/// CR 400.7: bouncing Gandalf and returning it makes a new object, so the
/// damage mode is available again and resolves a second time.
#[test]
fn gandalf_mode_history_resets_after_leaving_and_returning() {
    let mut b = gandalf_board();
    let life_start = first_damage(&mut b);

    let gandalf = b.gandalf;
    let before = incarnation(&b.runner, gandalf);
    move_through_pipeline(&mut b.runner, gandalf, Zone::Hand);
    move_through_pipeline(&mut b.runner, gandalf, Zone::Battlefield);
    assert_eq!(
        b.runner.state().objects[&gandalf].zone,
        Zone::Battlefield,
        "reach: Gandalf is back on the battlefield"
    );
    assert!(
        incarnation(&b.runner, gandalf) > before,
        "reach: CR 400.7 the returned Gandalf is a new object"
    );

    let second = b.instants[1];
    let prompts = cast_choosing_damage(&mut b, second);
    assert_eq!(
        prompts,
        vec![ModePrompt {
            player: P0,
            source_id: gandalf,
            unavailable: vec![],
        }],
        "CR 400.7: the returned Gandalf has no memory of earlier choices"
    );
    assert_eq!(
        life(&b.runner, P1),
        life_start - 6,
        "the returned Gandalf deals 3 damage again"
    );
}

/// CR 400.7: bouncing Gandalf and recasting it through the real cast pipeline
/// makes a new object with an empty history.
#[test]
fn gandalf_mode_history_resets_after_recast() {
    let mut b = gandalf_board();
    let life_start = first_damage(&mut b);

    let gandalf = b.gandalf;
    move_through_pipeline(&mut b.runner, gandalf, Zone::Hand);
    cast(&mut b.runner, gandalf);
    let recast_prompts = drive(&mut b.runner, None, |_| None);
    assert!(
        recast_prompts.is_empty(),
        "casting a creature spell does not trigger Gandalf"
    );
    assert_eq!(
        b.runner.state().objects[&gandalf].zone,
        Zone::Battlefield,
        "reach: the recast Gandalf resolved onto the battlefield"
    );

    let second = b.instants[1];
    let prompts = cast_choosing_damage(&mut b, second);
    assert_eq!(
        prompts,
        vec![ModePrompt {
            player: P0,
            source_id: gandalf,
            unavailable: vec![],
        }],
        "CR 400.7: the recast Gandalf has no memory of earlier choices"
    );
    assert_eq!(
        life(&b.runner, P1),
        life_start - 6,
        "the recast Gandalf deals 3 damage again"
    );
}

/// A control change is not a zone change: the permanent remains the same
/// object and keeps its no-repeat history. Control moves through a real
/// gain-control spell, and the new controller's trigger is read through the
/// engine's mode prompt.
#[test]
fn gandalf_mode_history_survives_control_change() {
    let mut b = gandalf_board();
    first_damage(&mut b);
    let gandalf = b.gandalf;

    // P0 passes on an empty stack, giving P1 priority in the same step.
    b.runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    assert!(
        matches!(b.runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "reach: P1 holds priority"
    );

    let threaten = b.threaten;
    cast(&mut b.runner, threaten);
    let prompts = drive(&mut b.runner, Some(gandalf), |_| None);
    assert!(
        prompts.is_empty(),
        "P1's spell does not trigger P0's Gandalf"
    );
    assert_eq!(
        b.runner.state().objects[&gandalf].controller,
        P1,
        "reach: the gain-control spell resolved"
    );

    if matches!(b.runner.state().waiting_for, WaitingFor::Priority { player } if player == P0) {
        b.runner
            .act(GameAction::PassPriority)
            .expect("P0 passes priority");
    }
    let p1_instant = b.p1_instant;
    cast(&mut b.runner, p1_instant);
    let prompts = drive(&mut b.runner, None, |_| None);
    assert_eq!(
        prompts,
        vec![ModePrompt {
            player: P1,
            source_id: gandalf,
            unavailable: vec![DAMAGE_MODE],
        }],
        "CR 700.2: the same object keeps its history under a new controller"
    );
    assert!(
        b.runner
            .act(GameAction::SelectModes {
                indices: vec![DAMAGE_MODE],
            })
            .is_err(),
        "CR 700.2: the new controller cannot re-choose a used mode"
    );
}

/// Legacy saves keyed the history by bare `ObjectId`; they still load, bound to
/// `LEGACY_INCARNATION`, which matches no live object. The new shape round-trips.
#[test]
fn modal_history_serde_loads_legacy_and_round_trips() {
    let mut b = gandalf_board();
    first_damage(&mut b);
    let gandalf = b.gandalf;
    let live = ObjectIncarnationRef::of(gandalf, incarnation(&b.runner, gandalf));
    assert!(
        b.runner
            .state()
            .modal_modes_chosen_this_game
            .contains(&(live, DAMAGE_MODE)),
        "reach: the live incarnation's choice is recorded"
    );

    let current = serde_json::to_value(b.runner.state()).expect("serialize");
    let round_trip: GameState = serde_json::from_value(current.clone()).expect("deserialize");
    assert_eq!(
        round_trip.modal_modes_chosen_this_game,
        b.runner.state().modal_modes_chosen_this_game,
        "the incarnation-keyed shape round-trips"
    );

    let mut legacy = current;
    legacy["modal_modes_chosen_this_game"] = serde_json::json!([[gandalf.0, DAMAGE_MODE]]);
    let loaded: GameState = serde_json::from_value(legacy).expect("legacy save loads");
    assert!(
        loaded.modal_modes_chosen_this_game.contains(&(
            ObjectIncarnationRef::of(gandalf, LEGACY_INCARNATION),
            DAMAGE_MODE
        )),
        "a bare-id entry binds the legacy sentinel incarnation"
    );
    assert!(
        !loaded
            .modal_modes_chosen_this_game
            .contains(&(live, DAMAGE_MODE)),
        "a legacy entry never matches the live incarnation"
    );
}

// ---------------------------------------------------------------------------
// Genku, Future Shaper (NoRepeatThisTurn, CR 603.10a look-back)
// ---------------------------------------------------------------------------

struct Genku {
    runner: GameRunner,
    genku: ObjectId,
    fodder: Vec<ObjectId>,
    bystanders: Vec<ObjectId>,
    wipe: ObjectId,
    flicker: ObjectId,
}

/// Genku, `fodder_count` sacrifice outlets, `bystanders` vanilla nontoken
/// creatures, and a zero-cost "Destroy all creatures." and Ghostly Flicker in
/// P0's hand.
fn genku_board(fodder_count: usize, bystanders: usize) -> Genku {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let genku = scenario
        .add_creature_from_oracle(P0, "Genku, Future Shaper", 2, 4, GENKU_ORACLE)
        .id();
    let fodder = (0..fodder_count)
        .map(|i| {
            scenario
                .add_creature_from_oracle(P0, &format!("Fodder {i}"), 1, 1, FODDER_ORACLE)
                .id()
        })
        .collect();
    let bystanders = (0..bystanders)
        .map(|_| scenario.add_vanilla(P0, 2, 2))
        .collect();
    let wipe = scenario
        .add_spell_to_hand_from_oracle(P0, "Wipe", false, WIPE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    Genku {
        runner: scenario.build(),
        genku,
        fodder,
        bystanders,
        wipe,
        flicker,
    }
}

/// Sacrifice each fodder in turn while Genku is alive, choosing `picks[i]` for
/// its trigger. Asserts the live prompts accumulate the turn's history.
fn sacrifice_fodder(k: &mut Genku, picks: &[usize]) {
    for (i, &mode) in picks.iter().enumerate() {
        let fodder = k.fodder[i];
        activate(&mut k.runner, fodder);
        let prompts = drive(&mut k.runner, None, |_| Some(vec![mode]));
        let mut expected: Vec<usize> = picks[..i].to_vec();
        expected.sort_unstable();
        assert_eq!(
            prompts,
            vec![ModePrompt {
                player: P0,
                source_id: k.genku,
                unavailable: expected,
            }],
            "live Genku trigger #{i} offers every mode not yet chosen this turn"
        );
    }
}

/// Cast the wipe; each Genku look-back prompt takes the lowest available mode.
fn wipe(k: &mut Genku) -> Vec<ModePrompt> {
    let wipe = k.wipe;
    cast(&mut k.runner, wipe);
    let prompts = drive(&mut k.runner, None, |prompt| {
        Some(vec![(0..3)
            .find(|m| !prompt.unavailable.contains(m))
            .expect("a prompt always offers an available mode")])
    });
    assert_eq!(
        k.runner.state().objects[&k.genku].zone,
        Zone::Graveyard,
        "reach: Genku left the battlefield"
    );
    for bystander in &k.bystanders {
        assert_eq!(
            k.runner.state().objects[bystander].zone,
            Zone::Graveyard,
            "reach: a nontoken permanent left alongside Genku (the look-back event)"
        );
    }
    prompts
}

fn tokens_with_subtype(runner: &GameRunner, subtype: &str) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.is_token && obj.card_types.subtypes.iter().any(|s| s == subtype)
        })
        .count()
}

fn token_count(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].is_token)
        .count()
}

/// CR 603.10a + CR 700.2: Genku leaves simultaneously with two other nontoken
/// permanents after already choosing the Fox mode this turn. Both look-back
/// triggers read the departed Genku's history: the Fox mode stays unavailable,
/// and the second trigger cannot repeat the first's mode.
#[test]
fn genku_lookback_triggers_keep_departed_history() {
    let mut k = genku_board(1, 2);
    sacrifice_fodder(&mut k, &[FOX]);
    assert_eq!(tokens_with_subtype(&k.runner, "Fox"), 1, "reach: Fox made");

    let prompts = wipe(&mut k);
    let genku = k.genku;
    assert_eq!(
        prompts,
        vec![
            ModePrompt {
                player: P0,
                source_id: genku,
                unavailable: vec![FOX],
            },
            ModePrompt {
                player: P0,
                source_id: genku,
                unavailable: vec![FOX, MOONFOLK],
            },
        ],
        "CR 603.10a: the departed Genku's turn history carries into its look-back triggers"
    );
    assert_eq!(tokens_with_subtype(&k.runner, "Moonfolk"), 1);
    assert_eq!(tokens_with_subtype(&k.runner, "Rat"), 1);
    assert_eq!(
        tokens_with_subtype(&k.runner, "Fox"),
        0,
        "no second Fox and the first one was destroyed"
    );
}

/// CR 603.10a + CR 603.3c: every mode was chosen this turn before Genku left
/// with another nontoken permanent; the look-back trigger has no legal mode
/// and is removed, so nothing prompts and no token is created.
#[test]
fn genku_lookback_trigger_with_all_modes_used_is_removed() {
    let mut k = genku_board(3, 1);
    sacrifice_fodder(&mut k, &[FOX, MOONFOLK, RAT]);
    assert_eq!(token_count(&k.runner), 3, "reach: three tokens made");

    let prompts = wipe(&mut k);
    assert!(
        prompts.is_empty(),
        "CR 603.3c: no mode can be chosen, got {prompts:?}"
    );
    assert_eq!(token_count(&k.runner), 0, "no token is created");
    assert!(
        k.runner.state().stack.is_empty(),
        "the dropped trigger leaves nothing on the stack"
    );
}

/// CR 603.10a: two modes used, two simultaneous look-back triggers — the
/// first takes the last open mode, the second has none left and is removed.
#[test]
fn genku_second_lookback_trigger_is_removed_after_last_mode() {
    let mut k = genku_board(2, 2);
    sacrifice_fodder(&mut k, &[FOX, MOONFOLK]);

    let prompts = wipe(&mut k);
    assert_eq!(
        prompts,
        vec![ModePrompt {
            player: P0,
            source_id: k.genku,
            unavailable: vec![FOX, MOONFOLK],
        }],
        "exactly one look-back trigger can choose a mode"
    );
    assert_eq!(tokens_with_subtype(&k.runner, "Rat"), 1, "exactly one Rat");
    assert_eq!(token_count(&k.runner), 1, "the Rat is the only token");
}

/// CR 603.10a + CR 400.7 + CR 700.2: Ghostly Flicker exiles Genku with another
/// nontoken permanent and returns both. Genku's look-back trigger was captured
/// from the departed incarnation, but its mode is chosen only after the return,
/// while a new Genku occupies the same `ObjectId`. The old trigger reads and
/// writes the departed incarnation's history; the returned Genku's history is
/// fresh.
#[test]
fn genku_flicker_old_trigger_uses_departed_history_while_returned_genku_is_fresh() {
    let mut k = genku_board(2, 1);
    sacrifice_fodder(&mut k, &[FOX]);
    let genku = k.genku;
    let departed = ObjectIncarnationRef::of(genku, incarnation(&k.runner, genku));
    assert!(
        k.runner
            .state()
            .modal_modes_chosen_this_turn
            .contains(&(departed, FOX)),
        "reach: the Fox choice is recorded under Genku's first incarnation"
    );

    let flicker = k.flicker;
    let bystander = k.bystanders[0];
    k.runner
        .cast(flicker)
        .target_objects(&[genku, bystander])
        .commit();
    let prompts = drive(&mut k.runner, None, |_| None);
    let returned = ObjectIncarnationRef::of(genku, incarnation(&k.runner, genku));
    assert_eq!(
        k.runner.state().objects[&genku].zone,
        Zone::Battlefield,
        "reach: Ghostly Flicker returned Genku before the trigger's mode choice"
    );
    assert_ne!(
        returned, departed,
        "reach: CR 400.7 the returned Genku is a new object on the same ObjectId"
    );
    assert_eq!(
        prompts,
        vec![ModePrompt {
            player: P0,
            source_id: genku,
            unavailable: vec![FOX],
        }],
        "CR 603.10a: the departed Genku's trigger still excludes the mode it chose"
    );
    assert!(
        k.runner
            .act(GameAction::SelectModes { indices: vec![FOX] })
            .is_err(),
        "CR 700.2: the departed Genku's trigger cannot re-choose its used mode"
    );
    k.runner
        .act(GameAction::SelectModes {
            indices: vec![MOONFOLK],
        })
        .expect("an unchosen mode is accepted");
    let rest = drive(&mut k.runner, None, |_| None);
    assert!(rest.is_empty(), "no further mode prompt: {rest:?}");
    assert_eq!(
        tokens_with_subtype(&k.runner, "Moonfolk"),
        1,
        "reach: Moonfolk made"
    );
    let turn = &k.runner.state().modal_modes_chosen_this_turn;
    assert!(
        turn.contains(&(departed, MOONFOLK)),
        "CR 603.10a: the old trigger's choice is written under the departed incarnation"
    );
    assert!(
        !turn.contains(&(returned, MOONFOLK)),
        "CR 400.7: the old trigger's choice is not written to the returned Genku"
    );

    let fodder = k.fodder[1];
    activate(&mut k.runner, fodder);
    let prompts = drive(&mut k.runner, None, |_| Some(vec![FOX]));
    assert_eq!(
        prompts,
        vec![ModePrompt {
            player: P0,
            source_id: genku,
            unavailable: vec![],
        }],
        "CR 400.7: the returned Genku has no memory of either incarnation's choices"
    );
    assert_eq!(
        tokens_with_subtype(&k.runner, "Fox"),
        2,
        "the returned Genku made a Fox"
    );
    assert!(
        k.runner
            .state()
            .modal_modes_chosen_this_turn
            .contains(&(returned, FOX)),
        "the returned Genku's choice is written under its own incarnation"
    );
}
