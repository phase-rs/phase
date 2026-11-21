//! CR 111.1 + CR 704.5g + CR 732.2a (issue #10192): A token minted during a loop period is
//! growth, not a spent driving resource. When it enters with damage marked on it (e.g.,
//! Marauding Raptor dealing 2 damage to each entering creature), the period offer sign check
//! (`period_sign_check` -> `driving_resources_non_decreasing`) must restrict its per-object
//! damage/counter checks to permanents persisting across `prior` and `current`, so the loop
//! reads as unbounded rather than `Spend`.

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{LoopDetectionMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Build an optional token-growth loop using real cards where every newly minted Saproling token
/// enters at 3/3 (Glorious Anthem + Gaea's Anthem) and takes 2 damage from Marauding Raptor:
/// Witherbloom, the Balancer + Sprout Swarm (Buyback + Convoke) + 4 green Saproling tokens +
/// Marauding Raptor + Glorious Anthem + Gaea's Anthem.
fn build_entry_damage_token_loop() -> Option<(GameRunner, ObjectId, Vec<ObjectId>)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Witherbloom, the Balancer", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Glorious Anthem", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Gaea's Anthem", Zone::Battlefield, db);

    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }

    let sprout = scenario.add_real_card(P0, "Sprout Swarm", Zone::Hand, db);

    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            let object = st.objects.get_mut(&id).unwrap();
            object.color = vec![ManaColor::Green];
            object.is_token = true;
        }
    }

    Some((runner, sprout, fodder))
}

fn saproling_count(state: &engine::types::game_state::GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.name == "Saproling" && o.is_token)
        })
        .count()
}

fn drive_recast_to_offer(
    runner: &mut GameRunner,
    sprout: ObjectId,
    convoke_fodder: ObjectId,
) -> bool {
    let outcome = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3} (reduced by Witherbloom affinity for creatures)
        .convoke_with(&[convoke_fodder]) // tap green Saproling for {G}
        .commit()
        .resolve();
    if matches!(outcome.final_waiting_for(), WaitingFor::LoopShortcut { .. }) {
        return true;
    }
    // Simultaneous ETB/entry triggers (e.g. Marauding Raptor + associated entry triggers)
    // may ask to order triggers: resolve the stack cleanly until priority or LoopShortcut.
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => return true,
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return false;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for while settling recast: {other:?}"),
        }
    }
    false
}

/// CR 111.1 + CR 704.5g + CR 732.2a (issue #10192): When an optional token-growth loop mints a
/// token that enters with damage marked on it (Marauding Raptor dealing 2 damage to each entering
/// Saproling token, which survives at 3/3 under Glorious Anthem + Gaea's Anthem), `period_sign_check`
/// must exclude the newly minted token's entry damage and offer the loop as unbounded rather than
/// demoting it to `Spend`.
#[test]
fn entry_damage_on_minted_token_offers_unbounded_not_spend() {
    let Some((mut runner, sprout, fodder)) = build_entry_damage_token_loop() else {
        return;
    };

    // First recast mints one Saproling token with 2 entry damage; second recast completes two
    // periods and surfaces the loop offer.
    let mut offered = false;
    for &tap_id in &fodder[..3] {
        if drive_recast_to_offer(&mut runner, sprout, tap_id) {
            offered = true;
            break;
        }
    }
    assert!(
        offered,
        "optional token-growth loop with entry damage should produce a LoopShortcut offer, got {:?}",
        runner.state().waiting_for
    );

    // Reach-guard: newly minted Saproling tokens actually entered with 2 damage marked from
    // Marauding Raptor (proving the entry-damage condition was reached).
    let damaged_tokens = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.name == "Saproling" && o.is_token && !fodder.contains(&o.id))
        .collect::<Vec<_>>();
    assert!(
        !damaged_tokens.is_empty() && damaged_tokens.iter().all(|t| t.damage_marked == 2),
        "reach-guard: newly minted Saprolings must carry 2 entry damage from Marauding Raptor, got {:?}",
        damaged_tokens
            .iter()
            .map(|t| (t.id, t.damage_marked))
            .collect::<Vec<_>>()
    );

    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected LoopShortcut, got {:?}",
            runner.state().waiting_for
        );
    };

    assert_eq!(proposer, &P0);
    assert!(
        predicted_winner.is_none(),
        "token-growth loop should not name a determinate winner"
    );
    assert!(
        !schema.is_bounded(),
        "entry damage on newly minted tokens must NOT bound the offer as Spend (got bounded schema: {schema:?})"
    );
    assert!(
        !certificate.unbounded.is_empty(),
        "unbounded offer must carry unbounded axes (got empty: {certificate:?})"
    );
    assert!(
        certificate.per_cycle.is_none(),
        "unbounded offer must not carry a counted per_cycle signature: {certificate:?}"
    );

    // Take the unbounded offer for 3 repetitions and verify token growth executes.
    let before = saproling_count(runner.state());
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        })
        .expect("declare shortcut");
    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("accept shortcut");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "shortcut completion must return to priority"
    );
    assert_eq!(
        saproling_count(runner.state()),
        before + 3,
        "declaring 3 repetitions should mint 3 additional Saprolings"
    );
}

/// CR 704.5g + CR 732.2a (issue #10192 control): damage accumulating on PERMANENTS THAT PERSIST
/// across the compared frames is a spent driving resource, so the newly-minted-token exclusion
/// must not over-exclude it.
///
/// Board (printed cards via `add_real_card`): the Sprout Swarm (buyback + convoke) recast loop of
/// `build_entry_damage_token_loop`, with Marauding Raptor swapped for Terror of the Peaks
/// ("Whenever another creature you control enters, this creature deals damage equal to that
/// creature's power to any target.") whose per-entry damage the drive aims (pinned
/// `TriggerTargetSelection`) at the persisting Armored Whirl Turtle (0/5 vanilla). The minted
/// Saprolings' entry damage is fixed (each enters, holds, never gains more), so the exclusion
/// covers nothing here: instead the Turtle's marked damage GROWS by one every repetition as it
/// nears its toughness (CR 704.5g) — a finite resource the period spends on a permanent that
/// survives every compared frame, exactly the shape that the minted-token exclusion must NOT hide.
///
/// The offer is asserted unconditionally: a counted loop this shape must still be offered (CR
/// 732.2a permits "a loop that repeats a specified number of times"), and the offer must be the
/// bounded one — counted, with empty unbounded axes. Suppressing shortcut offers entirely would
/// leave the loop unprompted and must fail here.
#[test]
fn persisting_permanent_damage_is_never_offered_unbounded() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Witherbloom, the Balancer", Zone::Battlefield, db);
    let sponge = scenario.add_real_card(P0, "Armored Whirl Turtle", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Terror of the Peaks", Zone::Battlefield, db);
    let mut fodder = Vec::new();
    for _ in 0..6 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = scenario.add_real_card(P0, "Sprout Swarm", Zone::Hand, db);

    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            let object = st.objects.get_mut(&id).unwrap();
            object.color = vec![ManaColor::Green];
            object.is_token = true;
        }
    }

    assert_eq!(
        runner.state().objects[&sponge].damage_marked,
        0,
        "the Turtle starts undamaged"
    );
    let mut offered = false;
    'recasts: for &tap_id in &fodder[..3] {
        runner
            .cast(sprout)
            .accept_optional()
            .convoke_with(&[tap_id])
            .commit();
        for _ in 0..60 {
            match runner.state().waiting_for.clone() {
                WaitingFor::LoopShortcut { .. } => {
                    offered = true;
                    break 'recasts;
                }
                WaitingFor::TriggerTargetSelection { player, .. } if player == P0 => {
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(engine::types::ability::TargetRef::Object(sponge)),
                        })
                        .expect("aim Terror of the Peaks' entry damage at the persisting Turtle");
                }
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let order: Vec<usize> = (0..triggers.len()).collect();
                    runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("order triggers");
                }
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    runner.act(GameAction::PassPriority).expect("pass priority");
                }
                other => panic!("unexpected waiting_for in the Terror loop: {other:?}"),
            }
        }
    }
    // CR 704.5g + CR 732.2a: the offer is unconditional — a counted loop of this shape must still
    // be offered (a loop that repeats a specified number of times), so suppressing shortcut
    // offers entirely cannot satisfy the control.
    assert!(
        offered,
        "persisting-damage loop must still be offered a shortcut, got {:?}",
        runner.state().waiting_for
    );

    // Reach guard: the Turtle started undamaged (asserted before the drive) and the confirmed
    // period aimed Terror's entry damage at it, so its marked damage grew between the compared
    // frames on a permanent that persists across all of them (it survives with damage marked).
    // The bounded reading below is therefore about persisting damage, not a minted token's
    // entry damage.
    let sponge_obj = runner
        .state()
        .objects
        .get(&sponge)
        .expect("Armored Whirl Turtle object exists");
    assert_eq!(
        sponge_obj.zone,
        Zone::Battlefield,
        "Armored Whirl Turtle must survive the driven repetitions"
    );
    assert!(
        sponge_obj.damage_marked >= 1,
        "reach-guard: Terror's entry damage must be marked on the persisting Turtle, got {}",
        sponge_obj.damage_marked
    );
    let sponge_remaining =
        sponge_obj.toughness.expect("Turtle has toughness") - sponge_obj.damage_marked as i32;

    let WaitingFor::LoopShortcut {
        certificate,
        schema,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!()
    };
    assert!(
        schema.is_bounded(),
        "damage accumulating on persisting permanents must bound the offer as Spend: {schema:?}"
    );
    assert!(
        certificate.unbounded.is_empty(),
        "bounded offer must carry no unbounded axes: {certificate:?}"
    );
    assert!(
        certificate.per_cycle.is_some(),
        "counted Spend offer must carry a per-cycle signature: {certificate:?}"
    );
    // CR 704.5g: one point lands on the Turtle per repetition, so the measured bound is the
    // repetitions left before its marked damage reaches its toughness. This pins the bound to
    // the persisting Turtle rather than any other axis of the period.
    assert_eq!(
        i64::from(schema.deliverable_capacity),
        i64::from(sponge_remaining),
        "the offer's bound must be the Turtle's remaining toughness: {schema:?}"
    );
}

/// CR 111.1 + CR 704.5g + CR 732.2a (issue #10192): Optional "Enrage — you may create a token copy"
/// loop (Sprouting Phytohydra + Marauding Raptor + Glorious Anthem) where each newly minted
/// 1/3 Phytohydra token enters with 2 damage marked and prompts "you may create a token that's a
/// copy". Verifies `period_sign_check` sees an unbounded offer rather than `Spend`.
#[test]
fn optional_enrage_copy_entry_damage_offers_unbounded() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Glorious Anthem", Zone::Battlefield, db);

    // Sprouting Phytohydra (verbatim Oracle text from Scryfall) in hand with {3}{G} in pool
    // (reduced from {4}{G} to {3}{G} by Marauding Raptor).
    let mut hydra = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Sprouting Phytohydra",
        0,
        2,
        "Defender (This creature can't attack.)\nWhenever this creature is dealt damage, you may create a token that's a copy of this creature.",
    );
    hydra.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 4,
    });
    let hydra_id = hydra.id();
    scenario.with_mana_pool(
        P0,
        vec![
            engine::types::mana::ManaUnit::new(ManaColor::Green.into(), ObjectId(0), false, vec![]),
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ),
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ),
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.cast(hydra_id).commit();

    let mut offered = false;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => {
                offered = true;
                break;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept may create copy");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::GameOver { .. } => break,
            other => panic!("unexpected waiting_for in Phytohydra loop: {other:?}"),
        }
    }

    assert!(
        offered,
        "Sprouting Phytohydra + Marauding Raptor + Glorious Anthem should produce LoopShortcut offer, got {:?}",
        runner.state().waiting_for
    );
    let WaitingFor::LoopShortcut {
        certificate,
        schema,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!()
    };
    assert!(
        !schema.is_bounded(),
        "Phytohydra entry damage must not bound the offer as Spend: {schema:?}"
    );
    assert!(
        !certificate.unbounded.is_empty(),
        "Phytohydra offer must be unbounded: {certificate:?}"
    );
}
