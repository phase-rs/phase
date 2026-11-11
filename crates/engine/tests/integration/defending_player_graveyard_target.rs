//! CR 508.5 + CR 508.5a: "target card from defending player's graveyard" names
//! the graveyard of the ONE player the triggering creature is attacking, not
//! any player's graveyard.
//!
//! Every row is a real card's verbatim Oracle text, attacked through the real
//! pipeline in a three-player game so a non-defending opponent's graveyard and
//! the attacker's own graveyard are both live wrong answers. Each graveyard
//! holds two cards so the trigger always prompts (a single legal card would be
//! auto-chosen), and the assertion reads the exact legal-target set the engine
//! offers at that prompt (CR 603.3d). A parser that drops the owner binding
//! offers every graveyard's cards and fails.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const GRAVEN_ABOMINATION: &str =
    "Whenever this creature attacks, exile target card from defending player's graveyard.";
const RYSORIAN_BADGER: &str = "Whenever this creature attacks and isn't blocked, you may exile up to two target creature cards from defending player's graveyard. If you do, you gain 1 life for each card exiled this way and this creature assigns no combat damage this turn.";

/// Two creature cards in each player's graveyard, as `(owner, card)`.
fn scenario(attacker_oracle: &str) -> (GameScenario, ObjectId, Vec<(PlayerId, ObjectId)>) {
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for _ in 0..10 {
            sc.add_card_to_library_top(player, "Plains");
        }
    }
    let attacker = sc
        .add_creature_from_oracle(P0, "Attacker", 2, 2, attacker_oracle)
        .id();
    // Idle defenders so each defending player gets a declare-blockers prompt.
    sc.add_creature(P1, "P1 Bystander", 1, 1);
    sc.add_creature(P2, "P2 Bystander", 1, 1);
    let mut graveyards = Vec::new();
    for player in [P0, P1, P2] {
        for n in 0..2 {
            let id = sc
                .add_creature_to_graveyard(player, &format!("Corpse {n} of {player:?}"), 1, 1)
                .id();
            graveyards.push((player, id));
        }
    }
    (sc, attacker, graveyards)
}

/// Declare `attacker` at `defender`, decline all blocks, and return the legal
/// targets of the first trigger target prompt.
fn legal_targets_at_trigger_prompt(
    r: &mut GameRunner,
    attacker: ObjectId,
    defender: PlayerId,
) -> Vec<TargetRef> {
    r.advance_to_combat();
    r.declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("the attack is legal");
    for _ in 0..32 {
        match r.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => return target_slots[selection.current_slot].legal_targets.clone(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                r.act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .expect("ordering is legal");
            }
            WaitingFor::DeclareBlockers { .. } => {
                r.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declaring no blocks is legal");
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("unexpected prompt before the trigger target: {other:?}"),
        }
    }
    panic!(
        "the trigger never asked for a target: {:?}",
        r.state().waiting_for
    );
}

/// The legal set is exactly the defending player's two graveyard cards.
fn assert_only_defenders_cards(
    legal: &[TargetRef],
    graveyards: &[(PlayerId, ObjectId)],
    defender: PlayerId,
) {
    let mut offered: Vec<ObjectId> = legal
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    offered.sort();
    let mut expected: Vec<ObjectId> = graveyards
        .iter()
        .filter(|(owner, _)| *owner == defender)
        .map(|(_, card)| *card)
        .collect();
    expected.sort();
    assert_eq!(
        offered, expected,
        "only {defender:?}'s graveyard cards may be targeted: {legal:?}"
    );
}

/// CR 508.5a: the defending player is fixed per attack, so attacking P1 and
/// attacking P2 each offer only that player's graveyard.
#[test]
fn graven_abomination_targets_only_the_attacked_players_graveyard() {
    for defender in [P1, P2] {
        let (sc, attacker, graveyards) = scenario(GRAVEN_ABOMINATION);
        let mut r = sc.build();
        let legal = legal_targets_at_trigger_prompt(&mut r, attacker, defender);
        assert_only_defenders_cards(&legal, &graveyards, defender);
    }
}

/// An attacks-and-isn't-blocked trigger with an "up to two" graveyard target
/// binds the same single defending player.
#[test]
fn rysorian_badger_targets_only_the_attacked_players_graveyard() {
    for defender in [P1, P2] {
        let (sc, attacker, graveyards) = scenario(RYSORIAN_BADGER);
        let mut r = sc.build();
        let legal = legal_targets_at_trigger_prompt(&mut r, attacker, defender);
        assert_only_defenders_cards(&legal, &graveyards, defender);
    }
}

// ---------------------------------------------------------------------------------------------
// Full-combat outcomes: every printed decision resolved through the real pipeline.
// ---------------------------------------------------------------------------------------------

const GOBLIN_VANDAL: &str = "Whenever this creature attacks and isn't blocked, you may pay {R}. If you do, destroy target artifact defending player controls and this creature assigns no combat damage this turn.";
const RAKSHASA_DEBASER: &str = "Whenever this creature attacks, put target creature card from defending player's graveyard onto the battlefield under your control.";
const MEMORY_VAMPIRE: &str = "Whenever this creature deals combat damage to a player, any number of target players each mill that many cards. Then you may collect evidence 9. When you do, you may cast target nonland card from defending player's graveyard without paying its mana cost.";

/// How the attacking player answers each prompt.
struct Answers {
    /// Object targets, chosen in order whenever a slot offers them; a slot
    /// that offers none of the remaining picks is answered with no target.
    picks: Vec<ObjectId>,
    /// Every "you may" decision.
    accept: bool,
    /// Cards paid into a card-selection cost (collect evidence).
    pay_with: Vec<ObjectId>,
}

/// Declare `attacker` at `defender`, decline all blocks, answer every prompt
/// from `answers`, and run combat through damage until the postcombat main
/// phase opens with an empty stack. `on_priority` runs at every priority
/// window before it is passed. Returns every non-priority prompt answered, in
/// order; an evidence prompt is first probed with a cancel, recorded as
/// `"CancelRejected"` once the engine refuses it.
fn run_combat(
    r: &mut GameRunner,
    attacker: ObjectId,
    defender: PlayerId,
    answers: &Answers,
    on_priority: &mut dyn FnMut(&mut GameRunner),
) -> Vec<&'static str> {
    r.advance_to_combat();
    r.declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("the attack is legal");
    let mut picks = answers.picks.clone();
    let mut prompts = Vec::new();
    for _ in 0..200 {
        if r.state().phase == Phase::PostCombatMain
            && r.state().stack.is_empty()
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
        {
            return prompts;
        }
        if !matches!(r.state().waiting_for, WaitingFor::Priority { .. }) {
            prompts.push(r.state().waiting_for.variant_name());
        }
        match r.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                let choice = picks
                    .iter()
                    .position(|id| legal.contains(&TargetRef::Object(*id)))
                    .map(|index| TargetRef::Object(picks.remove(index)));
                r.act(GameAction::ChooseTarget { target: choice })
                    .expect("the target choice is legal");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                r.act(GameAction::DecideOptionalEffect {
                    accept: answers.accept,
                })
                .expect("the optional decision is legal");
            }
            WaitingFor::ManaPayment { .. } => {
                let (_, _, grouped) = engine::ai_support::legal_actions_full(r.state());
                let tap = grouped.values().flatten().find_map(|action| match action {
                    GameAction::TapLandForMana { selection } => Some(selection.clone()),
                    _ => None,
                });
                if let Some(selection) = tap {
                    r.act(GameAction::TapLandForMana { selection })
                        .expect("tapping the land is legal");
                }
                r.act(GameAction::PassPriority)
                    .expect("finalizing the mana payment is legal");
            }
            WaitingFor::CollectEvidenceChoice { cards: choices, .. } => {
                // CR 608.2d + CR 117.2e: the accepted evidence prompt belongs to
                // the resolving ability; backing out is refused and the prompt
                // stays, so a rejected cancel is recorded and then the evidence paid.
                let prompt = r.state().waiting_for.clone();
                assert!(
                    r.act(GameAction::CancelCast).is_err(),
                    "the evidence prompt can't be cancelled"
                );
                assert_eq!(
                    r.state().waiting_for,
                    prompt,
                    "the prompt stays after the cancel"
                );
                prompts.push("CancelRejected");
                let cards = answers
                    .pay_with
                    .iter()
                    .copied()
                    .filter(|id| choices.contains(id))
                    .collect();
                r.act(GameAction::SelectCards { cards })
                    .expect("the evidence payment is legal");
            }
            WaitingFor::PayCost { choices, .. } => {
                let cards = answers
                    .pay_with
                    .iter()
                    .copied()
                    .filter(|id| choices.contains(id))
                    .collect();
                r.act(GameAction::SelectCards { cards })
                    .expect("the card payment is legal");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                r.act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .expect("ordering is legal");
            }
            WaitingFor::DeclareBlockers { .. } => {
                r.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declaring no blocks is legal");
            }
            WaitingFor::Priority { .. } => {
                on_priority(r);
                r.act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("run_combat does not answer {}", other.variant_name()),
        }
    }
    panic!(
        "combat never settled: phase {:?}, {}",
        r.state().phase,
        r.state().waiting_for.variant_name()
    );
}

fn zone(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

fn defenders_cards(graveyards: &[(PlayerId, ObjectId)], defender: PlayerId) -> Vec<ObjectId> {
    graveyards
        .iter()
        .filter(|(owner, _)| *owner == defender)
        .map(|(_, card)| *card)
        .collect()
}

/// CR 510.1a: an attacker that "assigns no combat damage" deals none; without
/// the rider an unblocked 2/2 deals 2. "If you do" gates the life gain and the
/// rider on the optional exile, so declining leaves combat damage intact.
/// CR 115.6 + CR 118.12: "up to two" permits zero targets, and the optional
/// effect is keyed on choosing to perform it — accepting with no targets still
/// takes the "if you do" branch, so the Badger exiles and gains nothing yet
/// assigns no combat damage.
#[test]
fn rysorian_badger_rider_follows_the_exile_decision_through_combat() {
    struct Row {
        label: &'static str,
        accept: bool,
        pick_count: usize,
        exiled: usize,
        life_gained: i32,
        combat_damage: i32,
    }
    for row in [
        Row {
            label: "accept, two targets",
            accept: true,
            pick_count: 2,
            exiled: 2,
            life_gained: 2,
            combat_damage: 0,
        },
        Row {
            label: "accept, one target",
            accept: true,
            pick_count: 1,
            exiled: 1,
            life_gained: 1,
            combat_damage: 0,
        },
        Row {
            label: "accept, zero targets",
            accept: true,
            pick_count: 0,
            exiled: 0,
            life_gained: 0,
            combat_damage: 0,
        },
        Row {
            label: "decline",
            accept: false,
            pick_count: 2,
            exiled: 0,
            life_gained: 0,
            combat_damage: 2,
        },
    ] {
        let (sc, attacker, graveyards) = scenario(RYSORIAN_BADGER);
        let mut r = sc.build();
        let targets = defenders_cards(&graveyards, P1);
        let picks: Vec<ObjectId> = targets.iter().copied().take(row.pick_count).collect();
        let (p0_life, p1_life) = (r.life(P0), r.life(P1));
        let prompts = run_combat(
            &mut r,
            attacker,
            P1,
            &Answers {
                picks,
                accept: row.accept,
                pay_with: vec![],
            },
            &mut |_| {},
        );
        // Reach guard: the target prompt and the "you may" prompt were offered.
        assert!(
            prompts.contains(&"TriggerTargetSelection"),
            "{}: target prompt offered: {prompts:?}",
            row.label
        );
        assert!(
            prompts.contains(&"OptionalEffectChoice"),
            "{}: optional prompt offered: {prompts:?}",
            row.label
        );
        let exiled = targets
            .iter()
            .filter(|card| zone(&r, **card) == Zone::Exile)
            .count();
        assert_eq!(exiled, row.exiled, "{}: cards exiled", row.label);
        assert_eq!(
            r.life(P0) - p0_life,
            row.life_gained,
            "{}: life gained for each card exiled",
            row.label
        );
        assert_eq!(
            p1_life - r.life(P1),
            row.combat_damage,
            "{}: combat damage to the defending player",
            row.label
        );
        // Reach guard: the non-defending players' graveyards are untouched.
        for (owner, card) in &graveyards {
            if *owner != P1 {
                assert_eq!(zone(&r, *card), Zone::Graveyard, "{}", row.label);
            }
        }
    }
}

/// Goblin Vandal against P1, who controls two artifacts (so the target prompt
/// is never auto-answered), while P2 controls a third.
/// Returns `(runner, vandal, [P1's artifacts], P2's artifact)`.
fn goblin_vandal_scenario() -> (GameRunner, ObjectId, [ObjectId; 2], ObjectId) {
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for _ in 0..10 {
            sc.add_card_to_library_top(player, "Plains");
        }
    }
    let vandal = sc
        .add_creature_from_oracle(P0, "Goblin Vandal", 1, 1, GOBLIN_VANDAL)
        .id();
    sc.add_basic_land(P0, ManaColor::Red);
    sc.add_creature(P1, "P1 Bystander", 1, 1);
    let defenders = [
        sc.add_artifact_from_oracle(P1, "P1 Trinket", "").id(),
        sc.add_artifact_from_oracle(P1, "P1 Bauble", "").id(),
    ];
    let other_artifact = sc.add_artifact_from_oracle(P2, "P2 Trinket", "").id();
    (sc.build(), vandal, defenders, other_artifact)
}

/// CR 508.5a: "target artifact defending player controls" offers exactly the
/// attacked player's artifacts, never the third player's.
#[test]
fn goblin_vandal_targets_only_the_defending_players_artifacts() {
    let (mut r, vandal, defenders, _) = goblin_vandal_scenario();
    let legal = legal_targets_at_trigger_prompt(&mut r, vandal, P1);
    // The same exact-set check as the graveyard rows, over P1's artifacts.
    let owned: Vec<(PlayerId, ObjectId)> = defenders.iter().map(|id| (P1, *id)).collect();
    assert_only_defenders_cards(&legal, &owned, P1);
}

/// Goblin Vandal: paying {R} destroys the chosen defending artifact and the
/// Vandal assigns no combat damage; declining leaves it and deals damage. Both
/// rows must reach the target and payment prompts, so a trigger that never
/// fires can't pass the decline row.
#[test]
fn goblin_vandal_rider_follows_the_payment_decision_through_combat() {
    for (accept, destroyed, combat_damage) in [(true, true, 0), (false, false, 1)] {
        let (mut r, vandal, [artifact, unchosen], other_artifact) = goblin_vandal_scenario();
        let p1_life = r.life(P1);
        let prompts = run_combat(
            &mut r,
            vandal,
            P1,
            &Answers {
                picks: vec![artifact],
                accept,
                pay_with: vec![],
            },
            &mut |_| {},
        );
        // Reach guard: the trigger chose its target and offered the payment.
        assert!(
            prompts.contains(&"TriggerTargetSelection"),
            "accept={accept}: target prompt offered: {prompts:?}"
        );
        assert!(
            prompts.contains(&"OptionalEffectChoice"),
            "accept={accept}: payment offered: {prompts:?}"
        );
        let expected = if destroyed {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(zone(&r, artifact), expected, "accept={accept}: artifact");
        assert_eq!(
            zone(&r, unchosen),
            Zone::Battlefield,
            "accept={accept}: the untargeted defending artifact survives"
        );
        assert_eq!(
            zone(&r, other_artifact),
            Zone::Battlefield,
            "accept={accept}: a non-defending player's artifact is never affected"
        );
        assert_eq!(
            p1_life - r.life(P1),
            combat_damage,
            "accept={accept}: combat damage to the defending player"
        );
    }
}

/// Graven Abomination resolves: the chosen card leaves the defending player's
/// graveyard for exile, and every other graveyard is untouched.
#[test]
fn graven_abomination_exiles_the_chosen_defenders_card() {
    let (sc, attacker, graveyards) = scenario(GRAVEN_ABOMINATION);
    let mut r = sc.build();
    let chosen = defenders_cards(&graveyards, P2)[0];
    run_combat(
        &mut r,
        attacker,
        P2,
        &Answers {
            picks: vec![chosen],
            accept: true,
            pay_with: vec![],
        },
        &mut |_| {},
    );
    for (_, card) in &graveyards {
        let expected = if *card == chosen {
            Zone::Exile
        } else {
            Zone::Graveyard
        };
        assert_eq!(zone(&r, *card), expected, "{card:?}");
    }
}

/// Rakshasa Debaser resolves: the chosen creature card enters from the
/// defending player's graveyard under the attacking player's control.
#[test]
fn rakshasa_debaser_reanimates_the_chosen_defenders_creature() {
    let (sc, attacker, graveyards) = scenario(RAKSHASA_DEBASER);
    let mut r = sc.build();
    let chosen = defenders_cards(&graveyards, P1)[1];
    run_combat(
        &mut r,
        attacker,
        P1,
        &Answers {
            picks: vec![chosen],
            accept: true,
            pay_with: vec![],
        },
        &mut |_| {},
    );
    let object = &r.state().objects[&chosen];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.controller, P0, "under your control");
    for (_, card) in &graveyards {
        if *card != chosen {
            assert_eq!(zone(&r, *card), Zone::Graveyard, "{card:?}");
        }
    }
}

/// CR 508.5 + CR 113.7a: Memory Vampire leaves the battlefield while its
/// reflexive "cast target nonland card from defending player's graveyard"
/// trigger is on the stack. The ability exists independently of its source,
/// and "defending player" still names the player the Vampire was attacking,
/// so the cast is admitted and the card resolves under the caster's control.
#[test]
fn memory_vampire_casts_from_the_defenders_graveyard_after_leaving_play() {
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for _ in 0..10 {
            sc.add_card_to_library_top(player, "Plains");
        }
    }
    let vampire = sc
        .add_creature_from_oracle(P0, "Memory Vampire", 4, 4, MEMORY_VAMPIRE)
        .id();
    sc.add_creature(P1, "P1 Bystander", 1, 1);
    let evidence = sc
        .add_creature_to_graveyard(P0, "Evidence", 1, 1)
        .with_mana_cost(ManaCost::generic(9))
        .id();
    let wanted = sc
        .add_creature_to_graveyard(P1, "P1 Recruit", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let other = sc
        .add_creature_to_graveyard(P2, "P2 Recruit", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut r = sc.build();

    let mut removed = false;
    let mut hook = |r: &mut GameRunner| {
        // The reflexive trigger is on the stack with its object target chosen.
        let reflexive_on_stack = r.state().stack.iter().any(|entry| {
            entry.source_id == vampire
                && matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if ability.targets.contains(&TargetRef::Object(wanted))
                )
        });
        if reflexive_on_stack && !removed {
            zones::move_to_zone(r.state_mut(), vampire, Zone::Graveyard, &mut Vec::new());
            removed = true;
        }
    };
    let prompts = run_combat(
        &mut r,
        vampire,
        P1,
        &Answers {
            picks: vec![wanted, other],
            accept: true,
            pay_with: vec![evidence],
        },
        &mut hook,
    );
    assert!(
        removed,
        "reach guard: the reflexive trigger targeted the defender's card and the Vampire left play"
    );
    // CR 701.59a + CR 603.12: evidence was actually paid, and the reflexive
    // trigger chose its target only after that payment.
    assert_eq!(zone(&r, evidence), Zone::Exile, "evidence was collected");
    let paid = prompts
        .iter()
        .position(|p| *p == "CollectEvidenceChoice")
        .unwrap_or_else(|| panic!("the evidence prompt was answered: {prompts:?}"));
    let reflexive_target = prompts
        .iter()
        .rposition(|p| *p == "TriggerTargetSelection")
        .unwrap_or_else(|| panic!("the reflexive trigger chose a target: {prompts:?}"));
    assert!(
        paid < reflexive_target,
        "the reflexive trigger waits for evidence: {prompts:?}"
    );
    assert_eq!(
        prompts.get(paid + 1),
        Some(&"CancelRejected"),
        "cancelling the accepted evidence prompt was refused: {prompts:?}"
    );
    let cast = &r.state().objects[&wanted];
    assert_eq!(cast.zone, Zone::Battlefield, "the defender's card was cast");
    assert_eq!(cast.controller, P0);
    assert_eq!(
        zone(&r, other),
        Zone::Graveyard,
        "a non-defending player's card is never cast"
    );
}

/// CR 603.12: declining "you may collect evidence 9" means the "When you do"
/// event never happens, so no evidence is exiled and nothing is cast.
#[test]
fn memory_vampire_declining_evidence_casts_nothing() {
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for _ in 0..10 {
            sc.add_card_to_library_top(player, "Plains");
        }
    }
    let vampire = sc
        .add_creature_from_oracle(P0, "Memory Vampire", 4, 4, MEMORY_VAMPIRE)
        .id();
    sc.add_creature(P1, "P1 Bystander", 1, 1);
    let evidence = sc
        .add_creature_to_graveyard(P0, "Evidence", 1, 1)
        .with_mana_cost(ManaCost::generic(9))
        .id();
    let wanted = sc
        .add_creature_to_graveyard(P1, "P1 Recruit", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut r = sc.build();
    let prompts = run_combat(
        &mut r,
        vampire,
        P1,
        &Answers {
            picks: vec![wanted],
            accept: false,
            pay_with: vec![evidence],
        },
        &mut |_| {},
    );
    assert!(
        prompts.contains(&"OptionalEffectChoice"),
        "reach guard: the evidence offer was made: {prompts:?}"
    );
    assert!(
        !prompts.contains(&"CollectEvidenceChoice"),
        "declined evidence is never chosen: {prompts:?}"
    );
    assert_eq!(zone(&r, evidence), Zone::Graveyard);
    assert_eq!(zone(&r, wanted), Zone::Graveyard, "nothing was cast");
}

/// CR 701.59b + CR 603.12: with only mana value 8 in the graveyard, the
/// controller can't choose to collect evidence 9. The offer is never made,
/// nothing is exiled, and the "When you do" reflexive never reaches the stack,
/// so the defender's card is never offered as a cast target.
#[test]
fn memory_vampire_unaffordable_evidence_opens_no_cast() {
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for _ in 0..10 {
            sc.add_card_to_library_top(player, "Plains");
        }
    }
    let vampire = sc
        .add_creature_from_oracle(P0, "Memory Vampire", 4, 4, MEMORY_VAMPIRE)
        .id();
    sc.add_creature(P1, "P1 Bystander", 1, 1);
    let short = sc
        .add_creature_to_graveyard(P0, "Short Evidence", 1, 1)
        .with_mana_cost(ManaCost::generic(8))
        .id();
    let wanted = sc
        .add_creature_to_graveyard(P1, "P1 Recruit", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut r = sc.build();
    let p1_life = r.life(P1);

    let (mut damage_trigger_seen, mut reflexive_seen) = (false, false);
    let mut hook = |r: &mut GameRunner| {
        for entry in r.state().stack.iter().filter(|e| e.source_id == vampire) {
            if let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind {
                if ability.targets.contains(&TargetRef::Object(wanted)) {
                    reflexive_seen = true;
                } else {
                    damage_trigger_seen = true;
                }
            }
        }
    };
    let prompts = run_combat(
        &mut r,
        vampire,
        P1,
        &Answers {
            picks: vec![wanted],
            accept: true,
            pay_with: vec![short],
        },
        &mut hook,
    );
    // Reach guard: the combat damage trigger went on the stack and resolved.
    assert_eq!(p1_life - r.life(P1), 4, "the Vampire dealt combat damage");
    assert!(
        damage_trigger_seen,
        "the combat damage trigger was put on the stack"
    );
    assert!(
        !prompts.contains(&"OptionalEffectChoice"),
        "an unaffordable evidence offer is never made: {prompts:?}"
    );
    assert!(
        !prompts.contains(&"CollectEvidenceChoice"),
        "no evidence is chosen: {prompts:?}"
    );
    assert!(
        !reflexive_seen,
        "the cast reflexive never reached the stack"
    );
    assert_eq!(zone(&r, short), Zone::Graveyard, "nothing was exiled");
    assert_eq!(zone(&r, wanted), Zone::Graveyard, "nothing was cast");
}
