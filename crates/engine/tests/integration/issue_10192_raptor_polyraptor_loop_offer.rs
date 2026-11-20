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
/// Board (printed cards via `add_real_card`): the entry-damage token loop of
/// `build_entry_damage_token_loop`, but with Marauding Raptor swapped for Forerunner of the
/// Empire ("Whenever a Dinosaur you control enters, you may have this creature deal 1 damage to
/// each creature.") and Maskwood Nexus ("Creatures you control are every creature type."), so
/// every minted Saproling is a Dinosaur. Each repetition then marks 1 damage on Forerunner and on
/// every Saproling already in play. Those objects persist across repetitions while their marked
/// damage grows toward toughness (CR 704.5g), so it is a finite resource the period spends: the
/// minted-object exclusion must not hide it, and the engine must never offer this loop as an
/// unbounded shortcut. (The paired tests in this module run the same scaffolding with entry-only
/// damage and are offered unbounded.)
#[test]
fn persisting_permanent_damage_is_never_offered_unbounded() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_real_card(P0, "Witherbloom, the Balancer", Zone::Battlefield, db);
    let forerunner = scenario.add_real_card(P0, "Forerunner of the Empire", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Maskwood Nexus", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Glorious Anthem", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Gaea's Anthem", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Intangible Virtue", Zone::Battlefield, db);
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

    // Maskwood Nexus expands "every creature type" against the game's creature-type list, which
    // deck loading fills from the card pool; a scenario has no deck, so supply the two types the
    // loop reads (sorted, as deck loading leaves it).
    runner.state_mut().all_creature_types = vec!["Dinosaur".to_string(), "Saproling".to_string()];

    'recasts: for &tap_id in &fodder[..3] {
        runner
            .cast(sprout)
            .accept_optional()
            .convoke_with(&[tap_id])
            .commit();
        for _ in 0..60 {
            match runner.state().waiting_for.clone() {
                WaitingFor::LoopShortcut { .. } => {
                    break 'recasts;
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept Forerunner's may-damage");
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
                other => panic!("unexpected waiting_for in the Forerunner loop: {other:?}"),
            }
        }
    }
    // Reach guards: the loop was actually driven through public actions, and the damage landed on
    // permanents that persisted (Forerunner survives with damage marked).
    let forerunner_obj = runner
        .state()
        .objects
        .get(&forerunner)
        .expect("Forerunner object exists");
    assert_eq!(
        forerunner_obj.zone,
        Zone::Battlefield,
        "Forerunner must survive the driven repetitions"
    );
    assert!(
        forerunner_obj.damage_marked >= 2,
        "reach-guard: Forerunner must accumulate damage across repetitions, got {}",
        forerunner_obj.damage_marked
    );

    // CR 704.5g + CR 732.2a: persisting damage bounds the offer — whenever a shortcut is
    // offered, it must not be the unbounded one.
    if let WaitingFor::LoopShortcut {
        certificate,
        schema,
        ..
    } = &runner.state().waiting_for
    {
        assert!(
            schema.is_bounded() && certificate.unbounded.is_empty(),
            "damage accumulating on persisting permanents must not be offered unbounded: {schema:?} {certificate:?}"
        );
    }
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
