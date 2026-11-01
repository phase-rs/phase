//! S25-P3 — Rhys, the Evermore: "{W}, {T}: Remove any number of counters from
//! target creature you control. Activate only as a sorcery." plus the shared
//! "remove any number of counters" interactive effect (Tetravus class).
//!
//! CR 107.1c (any number, incl. zero), CR 608.2d (resolution-time choice),
//! CR 608.2h (the removed count feeds a downstream "create that many" rider).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CounterRemoveChoice, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

// Verbatim activated-ability line (the Flash + ETB-persist lines parse independently
// and are irrelevant to the removal branch under test).
const RHYS_ACTIVATED: &str =
    "{W}, {T}: Remove any number of counters from target creature you control. \
Activate only as a sorcery.";

// Full verbatim Oracle text (Scryfall) for the parser-shape reach guard.
const RHYS_FULL: &str = "Flash\n\
When Rhys enters, another target creature you control gains persist until end of turn. \
(When it dies, if it had no -1/-1 counters on it, return it to the battlefield under its \
owner's control with a -1/-1 counter on it.)\n\
{W}, {T}: Remove any number of counters from target creature you control. Activate only as a sorcery.";

const TETRAVUS: &str = "Flying\n\
This creature enters with three +1/+1 counters on it.\n\
At the beginning of your upkeep, you may remove any number of +1/+1 counters from this creature. \
If you do, create that many 1/1 colorless Tetravite artifact creature tokens. They each have \
flying and \"This token can't be enchanted.\"\n\
At the beginning of your upkeep, you may exile any number of tokens created with this creature. \
If you do, put that many +1/+1 counters on this creature.";

const CHARGE: &str = "charge";

fn counters(state: &GameState, id: ObjectId, ct: &CounterType) -> u32 {
    state
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(ct).copied())
        .unwrap_or(0)
}

fn charge() -> CounterType {
    CounterType::Generic(CHARGE.to_string())
}

/// Position of the activated remove-counter ability in Rhys's ability list.
/// Reverting the parser arm leaves the effect `Unimplemented`, so this lookup
/// returns `None` and the test fails at setup — a built-in revert guard.
fn remove_ability_index(state: &GameState, id: ObjectId) -> usize {
    state.objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::RemoveCounter { .. }))
        .expect("Rhys must expose a RemoveCounter activated ability")
}

fn start_main_phase(runner: &mut engine::game::scenario::GameRunner) {
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::PreCombatMain;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
}

// ── Test 5: parser shape (proves the root-cause parser arm) ──────────────────

#[test]
fn rhys_parses_remove_any_number_as_up_to_sentinel_not_unimplemented() {
    let parsed = engine::parser::parse_oracle_text(
        RHYS_FULL,
        "Rhys, the Evermore",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::RemoveCounter { .. }))
        .expect("activated remove-counter ability must parse");
    match ability.effect.as_ref() {
        Effect::RemoveCounter {
            counter_type,
            count,
            target,
        } => {
            assert_eq!(*counter_type, None, "untyped: any number of *counters*");
            // Positive reach guard: the "any number of" arm encodes UpTo over the
            // remove-all sentinel; reverting the arm yields Unimplemented instead.
            match count {
                QuantityExpr::UpTo { max } => assert!(
                    matches!(max.as_ref(), QuantityExpr::Fixed { value: -1 }),
                    "UpTo must wrap the remove-all sentinel, got {max:?}"
                ),
                other => panic!("expected UpTo{{Fixed{{-1}}}}, got {other:?}"),
            }
            // "target creature you control" — a real target filter, not SelfRef/None.
            assert!(
                !matches!(target, engine::types::ability::TargetFilter::SelfRef),
                "Rhys targets another creature, not itself: {target:?}"
            );
        }
        _ => unreachable!(),
    }
    // Positive reach guard scoped to the fix: the remove-counter clause must not
    // have degraded to Unimplemented. (Flash / the ETB persist grant are separate
    // abilities outside this change's scope.)
    let unimpl: Vec<String> = parsed
        .abilities
        .iter()
        .filter_map(|a| match a.effect.as_ref() {
            Effect::Unimplemented { name, description } => Some(format!("{name}: {description:?}")),
            _ => None,
        })
        .collect();
    assert!(
        !unimpl.iter().any(|s| s.to_lowercase().contains("remove")),
        "the remove-counter clause must not be Unimplemented; offenders: {unimpl:?}"
    );
}

// ── Test 5b: multi-source "from among" stays out of scope (Unimplemented) ────

fn parses_remove_counter(clause: &str) -> bool {
    // Wrap the verbatim remove clause in an activated-ability shell so the effect
    // dispatch routes it through `try_parse_remove_counter` (the branch the guard
    // protects), then check whether it produced a single-source RemoveCounter.
    let text = format!("{{T}}: {clause}");
    let parsed = engine::parser::parse_oracle_text(&text, "X", &[], &["Creature".to_string()], &[]);
    parsed.abilities.iter().any(|a| {
        matches!(a.effect.as_ref(), Effect::RemoveCounter { .. })
            || a.sub_ability
                .as_ref()
                .is_some_and(|s| matches!(s.effect.as_ref(), Effect::RemoveCounter { .. }))
    })
}

#[test]
fn multi_source_from_among_removal_stays_unimplemented() {
    // CR 608.2d: "remove any number of counters from among <objects>" distributes
    // the removal among untargeted permanents — a multi-source choice the
    // single-source interactive path cannot model.
    // The `among` guard must leave these Unimplemented, not collapse them to a
    // single-source RemoveCounter (which would flip the card to "supported" with
    // wrong runtime semantics — the coverage-regression caught Galloping Lizrog
    // gaining without this guard).
    //
    // Positive reach guard (non-vacuous): the single-source variant of the same
    // clause DOES parse to RemoveCounter, proving the guard — not some unrelated
    // parse failure — is what suppresses the "from among" forms.
    assert!(
        parses_remove_counter(
            "Remove any number of +1/+1 counters from target creature you control."
        ),
        "reach guard: the single-source clause must reach try_parse_remove_counter"
    );
    // Verbatim remove clauses from the two out-of-scope multi-source cards.
    // Reverting the guard makes BOTH of these return true.
    assert!(
        !parses_remove_counter(
            "Remove any number of +1/+1 counters from among creatures you control."
        ),
        "Galloping Lizrog's 'from among creatures you control' must stay Unimplemented"
    );
    assert!(
        !parses_remove_counter(
            "Remove any number of counters from among permanents on the battlefield."
        ),
        "Eventide's Shadow's 'from among permanents on the battlefield' must stay Unimplemented"
    );
}

// ── Test 1: core runtime round-trip via the real activation pipeline ─────────

#[test]
fn rhys_activated_ability_removes_chosen_counter_subset() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rhys = scenario
        .add_creature_from_oracle(P0, "Rhys, the Evermore", 3, 3, RHYS_ACTIVATED)
        .id();
    let bearer = scenario.add_creature(P0, "Counter Bearer", 2, 2).id();
    scenario.with_counter(bearer, CounterType::Plus1Plus1, 3);
    scenario.with_counter(bearer, charge(), 2);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    start_main_phase(&mut runner);
    let idx = remove_ability_index(runner.state(), rhys);

    runner
        .act(GameAction::ActivateAbility {
            source_id: rhys,
            ability_index: idx,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Rhys's remove-counter ability");

    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 40, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bearer)),
                    })
                    .expect("target the counter bearer");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize {W} from pool");
            }
            WaitingFor::RemoveCountersChoice { available, .. } => {
                // Available must expose both public per-type counts.
                assert!(available
                    .iter()
                    .any(|(ct, n)| *ct == CounterType::Plus1Plus1 && *n == 3));
                assert!(available.iter().any(|(ct, n)| *ct == charge() && *n == 2));
                runner
                    .act(GameAction::ChooseCountersToRemove {
                        selections: vec![
                            CounterRemoveChoice {
                                counter_type: CounterType::Plus1Plus1,
                                count: 2,
                            },
                            CounterRemoveChoice {
                                counter_type: charge(),
                                count: 1,
                            },
                        ],
                    })
                    .expect("remove 2 of 3 +1/+1 and 1 of 2 charge");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    assert_eq!(
        counters(runner.state(), bearer, &CounterType::Plus1Plus1),
        1,
        "removed exactly 2 of 3 +1/+1 counters"
    );
    assert_eq!(
        counters(runner.state(), bearer, &charge()),
        1,
        "removed exactly 1 of 2 charge counters"
    );
}

// ── Test 2: zero-legal empty selection (CR 107.1c) ───────────────────────────

#[test]
fn rhys_empty_selection_removes_nothing_and_finishes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rhys = scenario
        .add_creature_from_oracle(P0, "Rhys, the Evermore", 3, 3, RHYS_ACTIVATED)
        .id();
    let bearer = scenario.add_creature(P0, "Counter Bearer", 2, 2).id();
    scenario.with_counter(bearer, CounterType::Plus1Plus1, 2);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    start_main_phase(&mut runner);
    let idx = remove_ability_index(runner.state(), rhys);

    runner
        .act(GameAction::ActivateAbility {
            source_id: rhys,
            ability_index: idx,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");

    let mut reached_prompt = false;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 40, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bearer)),
                    })
                    .expect("target");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay");
            }
            WaitingFor::RemoveCountersChoice { .. } => {
                reached_prompt = true; // positive reach guard: not a vacuous negative
                runner
                    .act(GameAction::ChooseCountersToRemove { selections: vec![] })
                    .expect("CR 107.1c: choosing zero is legal");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    assert!(reached_prompt, "the removal prompt must have been reached");
    assert_eq!(
        counters(runner.state(), bearer, &CounterType::Plus1Plus1),
        2,
        "empty selection removes nothing"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// ── Test 3: sorcery-speed timing (existing AsSorcery guard) ──────────────────

#[test]
fn rhys_ability_rejected_at_instant_speed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rhys = scenario
        .add_creature_from_oracle(P0, "Rhys, the Evermore", 3, 3, RHYS_ACTIVATED)
        .id();
    let _bearer = scenario
        .add_creature(P0, "Counter Bearer", 2, 2)
        .with_plus_counters(2)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    start_main_phase(&mut runner);
    // CR 602.5d + is_sorcery_speed_window: outside a main phase (here the upkeep
    // step) it is not a sorcery-speed window even for the active player with
    // priority, so the "Activate only as a sorcery" restriction must reject.
    runner.state_mut().phase = Phase::Upkeep;
    let idx = remove_ability_index(runner.state(), rhys);

    let result = runner.act(GameAction::ActivateAbility {
        source_id: rhys,
        ability_index: idx,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    });
    assert!(
        result.is_err(),
        "CR 602.5d: sorcery-speed ability cannot be activated outside a main phase"
    );
}

// ── Test 4: Tetravus stamp + continuation (#6) — EventContextAmount ──────────

fn tetravite_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.name.contains("Tetravite"))
        .count()
}

#[test]
fn tetravus_remove_all_counters_creates_that_many_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let tetravus = scenario
        .add_creature_from_oracle(P0, "Tetravus", 0, 0, TETRAVUS)
        .with_plus_counters(3)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.auto_advance_to_main_phase();

    let mut removed = false;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 80, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { description, .. } => {
                // Accept the remove-counters trigger; decline the exile-tokens one
                // (declining keeps the created tokens on the battlefield).
                let is_remove = description
                    .as_deref()
                    .map(|d| d.to_lowercase().contains("remove"))
                    .unwrap_or(true);
                runner
                    .act(GameAction::DecideOptionalEffect { accept: is_remove })
                    .expect("decide optional upkeep trigger");
            }
            WaitingFor::RemoveCountersChoice { available, .. } => {
                removed = true;
                let selections = available
                    .iter()
                    .map(|(ct, n)| CounterRemoveChoice {
                        counter_type: ct.clone(),
                        count: *n,
                    })
                    .collect();
                runner
                    .act(GameAction::ChooseCountersToRemove { selections })
                    .expect("remove all +1/+1 counters");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    assert!(removed, "the removal prompt must have been reached");
    assert_eq!(
        counters(runner.state(), tetravus, &CounterType::Plus1Plus1),
        0,
        "all three +1/+1 counters removed"
    );
    assert_eq!(
        tetravite_count(runner.state()),
        3,
        "CR 608.2h: 'create that many' reads the stamped removed count (3)"
    );
}

// ── Test 7: AI enumerates at least one legal ChooseCountersToRemove ──────────

#[test]
fn ai_enumerates_counter_removal_candidates_including_decline() {
    let mut scenario = GameScenario::new();
    let bearer = scenario
        .add_creature(P0, "Counter Bearer", 2, 2)
        .with_plus_counters(2)
        .id();
    let mut runner = scenario.build();

    let pending = engine::types::ability::ResolvedAbility::new(
        Effect::RemoveCounter {
            counter_type: None,
            count: QuantityExpr::up_to(QuantityExpr::Fixed { value: -1 }),
            target: engine::types::ability::TargetFilter::SelfRef,
        },
        vec![TargetRef::Object(bearer)],
        bearer,
        P0,
    );
    runner.state_mut().waiting_for = WaitingFor::RemoveCountersChoice {
        player: P0,
        source_id: bearer,
        counter_type: None,
        available: vec![(CounterType::Plus1Plus1, 2)],
        max_total: None,
        pending_effect: Box::new(pending),
    };

    let actions = engine::ai_support::legal_actions(runner.state());
    let removals: Vec<_> = actions
        .iter()
        .filter(|a| matches!(a, GameAction::ChooseCountersToRemove { .. }))
        .collect();
    assert!(
        !removals.is_empty(),
        "AI must offer at least one ChooseCountersToRemove candidate"
    );
    assert!(
        removals.iter().any(|a| matches!(
            a,
            GameAction::ChooseCountersToRemove { selections } if selections.is_empty()
        )),
        "the decline (empty) selection must be offered (CR 107.1c)"
    );
    let _ = P1; // keep the import used across the suite
}

// ── "Remove up to N counters": the capped form of the same choice ───────────
//
// CR 608.2d: "remove up to N counters" is announced while the effect is applied:
// the controller chooses which kinds and how many, zero through N in total.
// CR 107.3a: Hex Parasite's N is the X announced for its activation cost.
// CR 608.2c: its "for each counter removed this way" rider reads how many the
// removal actually removed.

// Verbatim Oracle text (MTGJSON AtomicCards, Hex Parasite).
const HEX_PARASITE: &str = "{X}{B/P}: Remove up to X counters from target permanent. \
For each counter removed this way, this creature gets +1/+0 until end of turn. \
({B/P} can be paid with either {B} or 2 life.)";

struct HexRun {
    runner: engine::game::scenario::GameRunner,
    hex: ObjectId,
    bearer: ObjectId,
}

/// Real Hex Parasite (1/1) and an opposing permanent with two +1/+1 counters
/// and one charge counter; the pool holds `x` colorless for the X and the
/// {B/P} is paid with 2 life. Drives the activation to the stack and passes
/// until it resolves or asks a resolution-time question.
fn hex_run(x: u32) -> HexRun {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hex = scenario
        .add_creature_from_oracle(P0, "Hex Parasite", 1, 1, HEX_PARASITE)
        .id();
    let bearer = scenario.add_creature(P1, "Counter Bearer", 2, 2).id();
    scenario.with_counter(bearer, CounterType::Plus1Plus1, 2);
    scenario.with_counter(bearer, charge(), 1);
    scenario.with_mana_pool(
        P0,
        (0..x)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    start_main_phase(&mut runner);
    let index = remove_ability_index(runner.state(), hex);
    runner
        .act(GameAction::ActivateAbility {
            source_id: hex,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Hex Parasite");
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 40, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: x })
                    .expect("announce X");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bearer)),
                    })
                    .expect("target the counter bearer");
            }
            WaitingFor::PhyrexianPayment { shards, .. } => {
                runner
                    .act(GameAction::SubmitPhyrexianChoices {
                        choices: vec![
                            engine::types::game_state::ShardChoice::PayLife;
                            shards.len()
                        ],
                    })
                    .expect("pay {B/P} with 2 life");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay X from the pool");
            }
            // The activation is on the stack: pass until it resolves or asks.
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    HexRun {
        runner,
        hex,
        bearer,
    }
}

/// Pass priority until the stack is empty (the activation has resolved).
fn settle(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 20, "stuck at {:?}", runner.state().waiting_for);
        runner.act(GameAction::PassPriority).expect("pass");
    }
}

fn power(state: &GameState, id: ObjectId) -> Option<i32> {
    state.objects.get(&id).and_then(|obj| obj.power)
}

/// CR 608.2d + CR 107.3a + CR 608.2c: X = 2 caps the removal at two counters in
/// total; the controller removes fewer (one +1/+1 counter), the other counters
/// stay, and the rider gives +1/+0 exactly once. A selection over the cap is
/// refused and leaves the prompt and the counters as they were.
#[test]
fn hex_parasite_removes_up_to_x_counters_of_the_chosen_kinds() {
    let HexRun {
        mut runner,
        hex,
        bearer,
    } = hex_run(2);
    let WaitingFor::RemoveCountersChoice {
        available,
        max_total,
        source_id,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the removal asks the controller: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(source_id, bearer, "the declared target");
    assert_eq!(max_total, Some(2), "CR 107.3a: capped at the announced X");
    assert!(
        available.contains(&(CounterType::Plus1Plus1, 2)),
        "{available:?}"
    );
    assert!(available.contains(&(charge(), 1)), "{available:?}");

    let prompt = runner.state().waiting_for.clone();
    let over_cap = runner.act(GameAction::ChooseCountersToRemove {
        selections: vec![
            CounterRemoveChoice {
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            },
            CounterRemoveChoice {
                counter_type: charge(),
                count: 1,
            },
        ],
    });
    assert!(over_cap.is_err(), "three counters exceed up to X = 2");
    assert_eq!(runner.state().waiting_for, prompt, "the prompt stays");
    assert_eq!(
        counters(runner.state(), bearer, &CounterType::Plus1Plus1),
        2
    );

    runner
        .act(GameAction::ChooseCountersToRemove {
            selections: vec![CounterRemoveChoice {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            }],
        })
        .expect("CR 608.2d: one counter is fewer than X");
    settle(&mut runner);

    assert_eq!(
        counters(runner.state(), bearer, &CounterType::Plus1Plus1),
        1,
        "exactly the chosen +1/+1 counter was removed"
    );
    assert_eq!(
        counters(runner.state(), bearer, &charge()),
        1,
        "the unchosen kind stays"
    );
    assert_eq!(
        power(runner.state(), hex),
        Some(2),
        "CR 608.2c: +1/+0 for the one counter removed this way"
    );
    assert_eq!(
        runner.state().players[0].life,
        18,
        "the {{B/P}} cost 2 life"
    );
}

/// The two-kinds sibling: one counter of EACH kind is within X = 2 and gives
/// +1/+0 twice.
#[test]
fn hex_parasite_removes_one_counter_of_each_kind_within_x() {
    let HexRun {
        mut runner,
        hex,
        bearer,
    } = hex_run(2);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RemoveCountersChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseCountersToRemove {
            selections: vec![
                CounterRemoveChoice {
                    counter_type: CounterType::Plus1Plus1,
                    count: 1,
                },
                CounterRemoveChoice {
                    counter_type: charge(),
                    count: 1,
                },
            ],
        })
        .expect("two counters of two kinds");
    settle(&mut runner);
    assert_eq!(
        counters(runner.state(), bearer, &CounterType::Plus1Plus1),
        1
    );
    assert_eq!(counters(runner.state(), bearer, &charge()), 0);
    assert_eq!(power(runner.state(), hex), Some(3), "+1/+0 twice");
}

/// CR 608.2d + CR 107.1c: X = 0 allows only the empty selection, so there is no
/// prompt; the activation still resolves (the life was paid), nothing is
/// removed and the rider repeats zero times.
#[test]
fn hex_parasite_with_x_zero_removes_nothing_without_a_prompt() {
    let HexRun {
        mut runner,
        hex,
        bearer,
    } = hex_run(0);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::RemoveCountersChoice { .. }
        ),
        "no choice is left to make: {:?}",
        runner.state().waiting_for
    );
    settle(&mut runner);
    assert!(runner.state().stack.is_empty(), "reach guard: it resolved");
    assert_eq!(
        runner.state().players[0].life,
        18,
        "reach guard: it was paid"
    );
    assert_eq!(
        counters(runner.state(), bearer, &CounterType::Plus1Plus1),
        2
    );
    assert_eq!(counters(runner.state(), bearer, &charge()), 1);
    assert_eq!(power(runner.state(), hex), Some(1), "no counter, no boost");
}

/// The AI's "remove the most" candidate respects the cap, so it never wedges on
/// a capped prompt; the decline is still offered.
#[test]
fn ai_counter_removal_candidates_respect_the_up_to_cap() {
    let HexRun { runner, .. } = hex_run(2);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RemoveCountersChoice { .. }
        ),
        "reach guard: {:?}",
        runner.state().waiting_for
    );
    let actions = engine::ai_support::legal_actions(runner.state());
    let totals: Vec<u32> = actions
        .iter()
        .filter_map(|action| match action {
            GameAction::ChooseCountersToRemove { selections } => {
                Some(selections.iter().map(|s| s.count).sum())
            }
            _ => None,
        })
        .collect();
    assert!(totals.contains(&0), "the decline is offered: {totals:?}");
    assert!(
        totals.contains(&2),
        "the capped maximum is offered: {totals:?}"
    );
    assert!(totals.iter().all(|total| *total <= 2), "{totals:?}");
}
