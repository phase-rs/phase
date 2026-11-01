//! Matt R15-3: simultaneous replacement prompts are made in APNAP order
//! (CR 101.4 + CR 616.1), not attack-declaration order.
//!
//! Fixture (3-player, P0 active): P0 attacks P1 with A and P2 with B (2/2s,
//! unblocked). P1 and P2 each control a Serra's Emissary naming Creature;
//! P0 controls Furnace of Rath. Each 2-damage event pauses on
//! [Furnace doubling, Emissary protection], chosen by the affected player.
//! Declaring in reverse seat order must still prompt P1 first: both runs
//! converge on the same prompt order and the same totals.
//!
//! Discriminating: pre-fix the batch keeps declaration order, so the
//! reversed run prompts P2 first.
//!
//! Furnace text is verbatim (MTGJSON Oracle, pinned in
//! `fixed_damage_amount_thresholds`); the Emissaries are real cards placed
//! directly, with the as-enters choice answered through production like
//! `selfless_squire_furnace_protection_ordering` (direct placement never runs
//! as-enters replacements, so the prompt is keyed to the permanent).

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::zones::Zone;

use crate::support::exact_named_choice_source;
use crate::support::shared_card_db as load_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Verbatim Furnace of Rath (shared with
/// `selfless_squire_furnace_protection_ordering`).
const FURNACE: &str = "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";

/// Verbatim Pariah (Aura) — shared with `pariah_attached_redirect`.
const PARIAH_TEXT: &str = "Enchant creature\nAll damage that would be dealt to you is dealt to enchanted creature instead.";

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    /// The affected player of each two-candidate prompt, in order.
    prompt_order: Vec<PlayerId>,
    lives: Vec<i32>,
}

struct Fixture {
    runner: GameRunner,
    a: ObjectId,
    b: ObjectId,
    furnace: ObjectId,
}

fn answer_emissary(runner: &mut GameRunner, emissary: ObjectId, player: PlayerId) {
    // The ETB Choose effect (a replacement) yields `WaitingFor::NamedChoice`
    // keyed to the entering permanent; answer Creature through production.
    let named_source = exact_named_choice_source(runner.state(), emissary);
    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        free_entry: None,
        player,
        choice_type: ChoiceType::card_type(),
        options: vec![
            "Artifact".to_string(),
            "Creature".to_string(),
            "Enchantment".to_string(),
            "Instant".to_string(),
            "Land".to_string(),
            "Planeswalker".to_string(),
            "Sorcery".to_string(),
        ],
        source: Some(named_source),
        persist_player: None,
    };
    runner
        .act(GameAction::ChooseOption {
            choice: "Creature".to_string(),
        })
        .expect("choosing Creature for the Emissary must resolve");
    assert_eq!(
        runner.state().objects[&emissary].chosen_card_type(),
        Some(CoreType::Creature),
        "reach guard: the Emissary must name Creature"
    );
}

fn setup(db: &CardDatabase) -> Fixture {
    let mut scenario = GameScenario::new_n_player(3, 0);
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Ember A", 2, 2).id();
    let b = scenario.add_creature(P0, "Ember B", 2, 2).id();
    let emissary1 = scenario.add_real_card(P1, "Serra's Emissary", Zone::Battlefield, db);
    let emissary2 = scenario.add_real_card(P2, "Serra's Emissary", Zone::Battlefield, db);
    let furnace = scenario
        .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE)
        .id();
    let mut runner = scenario.build();
    answer_emissary(&mut runner, emissary1, P1);
    answer_emissary(&mut runner, emissary2, P2);
    Fixture {
        runner,
        a,
        b,
        furnace,
    }
}

/// Drive combat with the given declaration order; answer every two-candidate
/// prompt protection-first and drain CR 616.1f remainders. Returns the prompt
/// order observed plus the final lives.
fn drive(fixture: &mut Fixture, reversed: bool) -> Outcome {
    let runner = &mut fixture.runner;
    let attacks = if reversed {
        vec![
            (fixture.b, AttackTarget::Player(P2)),
            (fixture.a, AttackTarget::Player(P1)),
        ]
    } else {
        vec![
            (fixture.a, AttackTarget::Player(P1)),
            (fixture.b, AttackTarget::Player(P2)),
        ]
    };
    let mut prompt_order = Vec::new();
    let mut log: Vec<GameEvent> = Vec::new();
    let mut declared = false;
    for _ in 0..128 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } => {
                assert_eq!(player, P0, "P0 is the active player");
                assert!(!declared, "attack declaration must occur only once");
                declared = true;
                log.extend(
                    runner
                        .declare_attackers(&attacks)
                        .expect("declare the attackers")
                        .events,
                );
            }
            WaitingFor::DeclareBlockers { .. } => {
                // No blockers anywhere; each defender declines in turn.
                log.extend(
                    runner
                        .declare_blockers(&[])
                        .expect("decline to block")
                        .events,
                );
            }
            WaitingFor::ReplacementChoice {
                player,
                candidate_count,
                candidates,
                ..
            } => {
                if candidate_count == 2 {
                    // The affected player chooses between the Furnace doubling
                    // and their Emissary's protection; the parked event must
                    // name that player's own damage.
                    let proposed_target = match runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .expect("a parked event must back the prompt")
                        .proposed
                        .clone()
                    {
                        ProposedEvent::Damage { target, .. } => target,
                        other => panic!("the prompt must park damage, got {other:?}"),
                    };
                    assert_eq!(
                        proposed_target,
                        TargetRef::Player(player),
                        "the prompt must name its chooser's own event"
                    );
                    assert!(
                        candidates.iter().any(|c| c.source_id == fixture.furnace),
                        "the Furnace doubling must be offered, got {candidates:?}"
                    );
                    let protection = candidates
                        .iter()
                        .position(|c| {
                            c.description
                                .starts_with("Prevent damage with protection from ")
                        })
                        .unwrap_or_else(|| {
                            panic!("the Emissary protection must be offered, got {candidates:?}")
                        });
                    prompt_order.push(player);
                    log.extend(
                        runner
                            .act(GameAction::ChooseReplacement { index: protection })
                            .expect("answer protection-first")
                            .events,
                    );
                } else {
                    assert_eq!(
                        candidate_count, 1,
                        "only CR 616.1f lone remainders may follow, got {candidates:?}"
                    );
                    log.extend(
                        runner
                            .act(GameAction::ChooseReplacement { index: 0 })
                            .expect("answer the lone remainder")
                            .events,
                    );
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                log.extend(
                    runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("order triggers")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                log.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("advance combat")
                        .events,
                );
            }
            other => panic!("unexpected combat prompt: {other:?}"),
        }
    }
    assert!(
        declared,
        "stall guard: attacks must actually have been declared"
    );
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat must complete"
    );
    assert!(
        log.iter().all(|event| !matches!(
            event,
            GameEvent::DamageDealt {
                target: TargetRef::Player(_),
                ..
            }
        )),
        "no player may be dealt damage through"
    );
    let lives = runner
        .state()
        .players
        .iter()
        .map(|p| p.life)
        .collect::<Vec<_>>();
    Outcome {
        prompt_order,
        lives,
    }
}

#[test]
fn simultaneous_replacement_prompts_follow_apnap_not_declaration_order() {
    let db = load_db().expect("the card database must load");
    let mut forward = setup(db);
    let forward_outcome = drive(&mut forward, false);
    assert_eq!(
        forward_outcome.prompt_order,
        vec![P1, P2],
        "seat order already prompts P1 first"
    );
    assert_eq!(
        forward_outcome.lives,
        vec![20, 20, 20],
        "protection-first prevents every 2 (doubling a prevented 0 deals 0)"
    );

    let mut reversed = setup(db);
    let reversed_outcome = drive(&mut reversed, true);
    assert_eq!(
        reversed_outcome.prompt_order,
        vec![P1, P2],
        "reversed declaration must still prompt P1 first (APNAP)"
    );
    assert_eq!(
        reversed_outcome, forward_outcome,
        "both declaration orders converge on the same prompts and totals"
    );
}

struct RedirectFixture {
    runner: GameRunner,
    a: ObjectId,
    b: ObjectId,
    furnace: ObjectId,
    pariah: ObjectId,
    host: ObjectId,
}

/// 4-player redirect fixture (P0 active): P0 attacks P1 with A and P2 with B
/// (2/2s, unblocked). P1 controls Pariah enchanting a creature P3 controls;
/// P2 and P3 each control a Serra's Emissary naming Creature; P0 controls
/// Furnace of Rath. P1 faces [Furnace doubling, Pariah redirect], P2 faces
/// [Furnace doubling, Emissary protection].
fn setup_redirect(db: &CardDatabase) -> RedirectFixture {
    let mut scenario = GameScenario::new_n_player(4, 0);
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Ember A", 2, 2).id();
    let b = scenario.add_creature(P0, "Ember B", 2, 2).id();
    let host = scenario.add_creature(P3, "Pariah Host", 0, 5).id();
    let pariah = scenario
        .add_creature(P1, "Pariah", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(PARIAH_TEXT)
        .id();
    let emissary2 = scenario.add_real_card(P2, "Serra's Emissary", Zone::Battlefield, db);
    let emissary3 = scenario.add_real_card(P3, "Serra's Emissary", Zone::Battlefield, db);
    let furnace = scenario
        .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE)
        .id();
    let mut runner = scenario.build();
    // Attach BEFORE any priority pass: an unattached Aura dies to SBA 704.5m
    // at the first state-based check (the Emissary answers below pass
    // priority), and `attach_to` does not validate the zone.
    attach_to(runner.state_mut(), pariah, host);
    answer_emissary(&mut runner, emissary2, P2);
    answer_emissary(&mut runner, emissary3, P3);
    assert_eq!(
        runner.state().objects[&pariah].attached_to,
        Some(AttachTarget::Object(host)),
        "reach guard: Pariah must be attached through the production authority"
    );
    assert_eq!(
        runner.state().objects[&pariah].zone,
        Zone::Battlefield,
        "reach guard: the Aura must survive to the battlefield attached"
    );
    RedirectFixture {
        runner,
        a,
        b,
        furnace,
        pariah,
        host,
    }
}

/// Drive the redirect fixture: P1 answers Pariah on their first two-candidate
/// prompt; every other two-candidate prompt answers protection-first; CR
/// 616.1f lone remainders drain. Returns the prompt order plus final lives.
fn drive_redirect(fixture: &mut RedirectFixture) -> Outcome {
    let runner = &mut fixture.runner;
    let attacks = vec![
        (fixture.a, AttackTarget::Player(P1)),
        (fixture.b, AttackTarget::Player(P2)),
    ];
    let mut prompt_order = Vec::new();
    let mut log: Vec<GameEvent> = Vec::new();
    let mut declared = false;
    let mut pariah_chosen = false;
    for _ in 0..128 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } => {
                assert_eq!(player, P0, "P0 is the active player");
                assert!(!declared, "attack declaration must occur only once");
                declared = true;
                log.extend(
                    runner
                        .declare_attackers(&attacks)
                        .expect("declare the attackers")
                        .events,
                );
            }
            WaitingFor::DeclareBlockers { .. } => {
                log.extend(
                    runner
                        .declare_blockers(&[])
                        .expect("decline to block")
                        .events,
                );
            }
            WaitingFor::ReplacementChoice {
                player,
                candidate_count,
                candidates,
                ..
            } => {
                if candidate_count == 2 {
                    let proposed_target = match runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .expect("a parked event must back the prompt")
                        .proposed
                        .clone()
                    {
                        ProposedEvent::Damage { target, .. } => target,
                        other => panic!("the prompt must park damage, got {other:?}"),
                    };
                    if player == P1 && !pariah_chosen {
                        assert_eq!(
                            proposed_target,
                            TargetRef::Player(P1),
                            "P1's prompt must name their own event"
                        );
                        assert!(
                            candidates.iter().any(|c| c.source_id == fixture.furnace),
                            "the Furnace doubling must be offered, got {candidates:?}"
                        );
                        let pariah_door = candidates
                            .iter()
                            .position(|c| c.source_id == fixture.pariah)
                            .unwrap_or_else(|| {
                                panic!("the Pariah redirect must be offered, got {candidates:?}")
                            });
                        pariah_chosen = true;
                        prompt_order.push(player);
                        log.extend(
                            runner
                                .act(GameAction::ChooseReplacement { index: pariah_door })
                                .expect("P1 chooses Pariah")
                                .events,
                        );
                    } else {
                        if player == P3 {
                            assert_eq!(
                                proposed_target,
                                TargetRef::Object(fixture.host),
                                "P3's prompt must name the redirected host, proving \
                                 the redirect applied"
                            );
                        } else {
                            assert_eq!(
                                proposed_target,
                                TargetRef::Player(player),
                                "the prompt must name its chooser's own event"
                            );
                        }
                        assert!(
                            candidates.iter().any(|c| c.source_id == fixture.furnace),
                            "the Furnace doubling must be offered, got {candidates:?}"
                        );
                        let protection = candidates
                            .iter()
                            .position(|c| {
                                c.description
                                    .starts_with("Prevent damage with protection from ")
                            })
                            .unwrap_or_else(|| {
                                panic!(
                                    "the Emissary protection must be offered, got {candidates:?}"
                                )
                            });
                        prompt_order.push(player);
                        log.extend(
                            runner
                                .act(GameAction::ChooseReplacement { index: protection })
                                .expect("answer protection-first")
                                .events,
                        );
                    }
                } else {
                    assert_eq!(
                        candidate_count, 1,
                        "only CR 616.1f lone remainders may follow, got {candidates:?}"
                    );
                    log.extend(
                        runner
                            .act(GameAction::ChooseReplacement { index: 0 })
                            .expect("answer the lone remainder")
                            .events,
                    );
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                log.extend(
                    runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("order triggers")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                log.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("advance combat")
                        .events,
                );
            }
            other => panic!("unexpected combat prompt: {other:?}"),
        }
    }
    assert!(
        declared,
        "stall guard: attacks must actually have been declared"
    );
    assert!(
        pariah_chosen,
        "reach guard: P1 must have faced the Furnace/Pariah choice"
    );
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat must complete"
    );
    assert!(
        log.iter().all(|event| !matches!(
            event,
            GameEvent::DamageDealt {
                target: TargetRef::Player(_),
                ..
            }
        )),
        "no player may be dealt damage through"
    );
    let lives = runner
        .state()
        .players
        .iter()
        .map(|p| p.life)
        .collect::<Vec<_>>();
    Outcome {
        prompt_order,
        lives,
    }
}

/// CR 616.1 + CR 616.1f: a redirection that moves a batch member to a later
/// APNAP seat does not drag that seat's prompt ahead of earlier outstanding
/// choices. P1 chooses Pariah (their 2 damage redirects to P3's creature, now
/// facing [Furnace, protection]); P2's outstanding [Furnace, protection]
/// choice precedes P3's.
///
/// Discriminating: pre-fix the re-park serves P3 immediately, prompting
/// P1 -> P3 -> P2.
#[test]
fn redirected_recipient_reorders_outstanding_prompts_by_apnap() {
    let db = load_db().expect("the card database must load");
    let mut fixture = setup_redirect(db);
    let outcome = drive_redirect(&mut fixture);
    assert_eq!(
        outcome.prompt_order,
        vec![P1, P2, P3],
        "P1 chooses Pariah, then P2's outstanding choice precedes P3's (APNAP)"
    );
    assert_eq!(
        outcome.lives,
        vec![20, 20, 20, 20],
        "P1's damage redirects away; protection prevents the rest"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.host].damage_marked,
        0,
        "the redirected damage is prevented by the Emissary protection"
    );
}

/// CR 614.6 + CR 800.4a + CR 616.1: after Pariah has redirected damage,
/// its original recipient's departure cannot erase the surviving host's event.
#[test]
fn redirected_damage_survives_original_recipient_conceding_at_replacement_pause() {
    // Verbatim Scryfall Oracle (retrieved 2026-10-29).
    const GISELA: &str = "Flying, first strike\nIf a source would deal damage to an opponent or a permanent an opponent controls, that source deals double that damage to that player or permanent instead.\nIf a source would deal damage to you or a permanent you control, prevent half that damage, rounded up.";
    let mut scenario = GameScenario::new_n_player(3, 0);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let host = scenario.add_creature(P2, "Pariah Host", 0, 10).id();
    let pariah = scenario
        .add_creature(P1, "Pariah", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(PARIAH_TEXT)
        .id();
    let gisela = scenario
        .add_creature_from_oracle(P2, "Gisela, Blade of Goldnight", 5, 5, GISELA)
        .id();
    let furnace = scenario
        .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), pariah, host);
    let mut redirected = false;
    let mut conceded = false;
    let mut log = Vec::new();
    for _ in 0..96 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } => {
                assert_eq!(player, P0);
                runner.declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            }
            WaitingFor::DeclareBlockers { .. } => runner.declare_blockers(&[]),
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } if !redirected => {
                assert_eq!(player, P1);
                let index = candidates
                    .iter()
                    .position(|c| c.source_id == pariah)
                    .expect("the original recipient must be offered Pariah");
                redirected = true;
                runner.act(GameAction::ChooseReplacement { index })
            }
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } if !conceded => {
                assert_eq!(
                    player, P2,
                    "the host's controller owns the redirected choice"
                );
                assert!(
                    matches!(
                        &runner.state().pending_replacement.as_ref().unwrap().proposed,
                        ProposedEvent::Damage { source_id, target, amount: 2, .. }
                            if *source_id == attacker && *target == TargetRef::Object(host)
                    ),
                    "reach guard: the parked event already names the surviving host"
                );
                assert_eq!(candidates.len(), 2);
                assert!(candidates.iter().any(|c| c.source_id == gisela));
                assert!(candidates.iter().any(|c| c.source_id == furnace));
                log.extend(
                    runner
                        .act(GameAction::Concede { player_id: P1 })
                        .expect("the original recipient may concede during another player's choice")
                        .events,
                );
                assert!(runner.state().players[P1.0 as usize].is_eliminated);
                let record = runner
                    .state()
                    .pending_combat_damage
                    .as_ref()
                    .expect("the surviving recipient's combat event remains parked");
                assert!(
                    !record.paused_entry_pruned,
                    "the historical assignment to P1 cannot prune damage now owed to the host"
                );
                let WaitingFor::ReplacementChoice {
                    player, candidates, ..
                } = &runner.state().waiting_for
                else {
                    panic!("the surviving host's prompt must remain live");
                };
                assert_eq!(*player, P2);
                let index = candidates
                    .iter()
                    .position(|c| c.source_id == furnace)
                    .expect("choose Furnace before Gisela's half prevention");
                conceded = true;
                runner.act(GameAction::ChooseReplacement { index })
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                assert_eq!(candidates.len(), 1, "only Gisela's remainder may follow");
                runner.act(GameAction::ChooseReplacement { index: 0 })
            }
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            other => panic!("unexpected combat prompt: {other:?}"),
        }
        .expect("combat must advance through public actions");
        log.extend(result.events);
    }
    assert!(
        redirected && conceded,
        "both the redirect and departure must occur"
    );
    assert!(matches!(
        runner.state().phase,
        Phase::EndCombat | Phase::PostCombatMain
    ));
    assert!(runner.state().pending_combat_damage.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&host].damage_marked,
        2,
        "Furnace doubles 2 to 4, then Gisela prevents 2; the remaining 2 must land"
    );
    assert!(
        log.iter().any(|event| matches!(event,
            GameEvent::DamageDealt { target, amount: 2, .. } if *target == TargetRef::Object(host)
        )),
        "positive damage delivery rules out a vacuous fully prevented event"
    );
}
