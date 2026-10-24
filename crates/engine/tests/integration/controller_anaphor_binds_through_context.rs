//! Controls for the controller-phrase grammar and binder: an anaphoric
//! controller phrase keeps binding to the player its ability introduced.
//!
//! - Balefire Dragon: "that player" is the damaged player (trigger path).
//! - Reservoir Kraken: "they" is the accepting opponent, not the caster's
//!   opponent set evaluated from the accepting player's frame.
//! - Temporal Distortion: "that player" is the upkeep player, bound through
//!   the counter-target fallback that reads the caller's player antecedents.
//! - Counter-placement watchers: "they" in "another creature they control" /
//!   "one or more creatures they control" is the opponent who placed the
//!   counters, bound through every subject arm that reads the actor scope.
//! - Declared damage recipients (Anathemancer, Price of Progress): "that player"
//!   in the amount is the target player, or each player in turn.
//! - Subject antecedents: "that player" / "they" in a predicate is the subject's
//!   target player (Imperial Edict and its family, Shadrix Silverquill, Break
//!   Under Pressure, Consume) or the controller of the subject's target object
//!   (Alpha Brawl, Dong Zhou).
//! - Siren's Call: "that player" in the delayed paragraph is the active player
//!   the coerce paragraph names (one spell ability, CR 113.3a).
//! - Accepting-player bodies ("any opponent may …"): "they" binds to the
//!   accepting player only in a direct operand chosen on resolution; anaphors
//!   anywhere else keep their unseeded reading.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, EffectKind, OpponentMayScope, TargetChoiceTiming,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const BALEFIRE_DRAGON: &str = "Flying\nWhenever this creature deals combat damage to a player, it deals that much damage to each creature that player controls.";

const TEMPORAL_DISTORTION: &str = "Whenever a creature or land becomes tapped, put an hourglass counter on it.\nEach permanent with an hourglass counter on it doesn't untap during its controller's untap step.\nAt the beginning of each player's upkeep, remove all hourglass counters from permanents that player controls.";

const RESERVOIR_KRAKEN: &str = "Trample, ward {2}\nAt the beginning of each combat, if this creature is untapped, any opponent may tap an untapped creature they control. If they do, tap this creature and create a 1/1 blue Fish creature token with \"This token can't be blocked.\"";

/// Pass the declare-attackers priority window and declare no blockers, leaving
/// the runner ready for `combat_damage`.
fn declare_no_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
                return;
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::DeclareAttackers => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
}

/// CR 510.2 + CR 603.2 + CR 608.2c: Balefire Dragon's trigger names the damaged
/// player, so "each creature that player controls" is that player's creatures
/// and never the attacker's controller's.
#[test]
fn balefire_dragon_damages_only_the_damaged_players_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(P0, "Balefire Dragon", 6, 6, BALEFIRE_DRAGON)
        .id();
    let own_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let their_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let their_giant = scenario.add_creature(P1, "P1 Giant", 5, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(dragon, AttackTarget::Player(P1))])
        .expect("declare Balefire Dragon");
    declare_no_blockers(&mut runner);
    let outcome = runner.combat_damage();

    // Reach guard: the dragon dealt its 6 combat damage and its trigger resolved.
    assert_eq!(outcome.life_delta(P1), -6, "P1 takes 6 combat damage");
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { source_id, .. } if *source_id == dragon
        )),
        "Balefire Dragon's trigger must resolve; events={:?}",
        outcome.events()
    );

    assert_eq!(outcome.zone_of(their_bear), Zone::Graveyard);
    assert_eq!(outcome.zone_of(their_giant), Zone::Graveyard);
    assert_eq!(
        outcome.zone_of(own_bear),
        Zone::Battlefield,
        "the attacker's controller's creature is not the damaged player's"
    );
}

/// CR 608.2c: in "any opponent may tap an untapped creature they control",
/// "they" is the opponent who accepts, so the choice is among that opponent's
/// creatures.
#[test]
fn reservoir_kraken_accepting_opponent_taps_their_own_creature() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let kraken = scenario
        .add_creature_from_oracle(P0, "Reservoir Kraken", 6, 6, RESERVOIR_KRAKEN)
        .id();
    let own_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let their_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    let mut prompted = None;
    let mut legal_for_acceptor = None;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                prompted = Some(player);
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::MultiTargetSelection {
                player,
                legal_targets,
                ..
            } => {
                legal_for_acceptor = Some((player, legal_targets.clone()));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![their_bear],
                    })
                    .expect("pick P1's creature");
            }
            // CR 115.10a + CR 608.2d: the accepting body calls its creature no
            // "target", so the accepting player chooses it on resolution.
            WaitingFor::TriggerTargetSelection { .. } => panic!(
                "the accepting body declares no target, so no stack-time target prompt: {:?}",
                runner.state().waiting_for
            ),
            WaitingFor::Priority { .. } if runner.state().phase == Phase::BeginCombat => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => break,
        }
    }

    // Reach guard: the accepting opponent was asked.
    assert_eq!(prompted, Some(P1), "P1 must be offered the choice");
    assert_eq!(
        legal_for_acceptor,
        Some((P1, vec![their_bear])),
        "the accepting opponent chooses among their own creatures"
    );
    let state = runner.state();
    assert!(state.objects[&their_bear].tapped, "P1's creature is tapped");
    assert!(state.objects[&kraken].tapped, "Reservoir Kraken is tapped");
    assert!(
        !state.objects[&own_bear].tapped,
        "P0's creature is not the accepting opponent's"
    );
}

/// CR 608.2c: in Temporal Distortion's "at the beginning of each player's
/// upkeep, remove all hourglass counters from permanents that player
/// controls", "that player" is the upkeep player, so the source controller's
/// permanents keep their counters.
///
/// The filter currently becomes a stack-time target slot (a pre-existing
/// defect, #9899), so the drive answers that prompt when it appears and checks
/// only what the binding decides: whose permanents are offered and reached.
/// "Every P1 permanent loses its counters" (CR 115.10a) is not asserted.
#[test]
fn temporal_distortion_removes_counters_from_the_upkeep_players_permanents() {
    let hourglass = || CounterType::Generic("hourglass".to_string());
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.with_library_top(P0, &["P0 Top A", "P0 Top B", "P0 Top C"]);
    scenario.with_library_top(P1, &["P1 Top A", "P1 Top B", "P1 Top C"]);
    scenario.at_phase(Phase::End);
    let td = scenario
        .add_enchantment_from_oracle(P0, "Temporal Distortion", TEMPORAL_DISTORTION)
        .id();
    let c0 = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let c1a = scenario.add_creature(P1, "P1 Bear A", 2, 2).id();
    let c1b = scenario.add_creature(P1, "P1 Bear B", 2, 2).id();
    for creature in [c0, c1a, c1b] {
        scenario.with_counter(creature, hourglass(), 1);
    }
    let mut runner = scenario.build();

    // Advance once: re-calling `advance_to_phase` during P1's upkeep restarts
    // the step and fires the upkeep trigger again. Settle with `act` only.
    runner.advance_to_phase(Phase::Draw);
    let mut offered: Vec<TargetRef> = Vec::new();
    for _ in 0..40 {
        let state = runner.state();
        if state.phase == Phase::Draw && state.active_player == P1 {
            break;
        }
        match state.waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                source_id: Some(source),
                target_slots,
                ..
            } if source == td => {
                let slot = target_slots
                    .iter()
                    .find(|slot| slot.effect_kind == EffectKind::RemoveCounter)
                    .expect("Temporal Distortion's prompt has a RemoveCounter slot");
                offered.extend(slot.legal_targets.iter().cloned());
                let first = slot.legal_targets[0].clone();
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(first),
                    })
                    .expect("answer Temporal Distortion's target prompt");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for during P1's upkeep: {other:?}"),
        }
    }

    let state = runner.state();
    // Reach guard: the drive got past P1's upkeep.
    assert!(
        state.phase == Phase::Draw && state.active_player == P1,
        "the drive must reach P1's draw step; phase={:?} active={:?} waiting_for={:?}",
        state.phase,
        state.active_player,
        state.waiting_for
    );
    let counters = |id| {
        state.objects[&id]
            .counters
            .get(&hourglass())
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(
        counters(c0),
        1,
        "the source controller's creature is not the upkeep player's"
    );
    assert!(
        counters(c1a) == 0 || counters(c1b) == 0,
        "an upkeep player's creature loses its hourglass counter; (c1a, c1b) = ({}, {})",
        counters(c1a),
        counters(c1b)
    );
    assert!(
        offered
            .iter()
            .all(|target| *target == TargetRef::Object(c1a) || *target == TargetRef::Object(c1b)),
        "every offered target is the upkeep player's; offered={offered:?}"
    );
}

/// An opponent (P1) casts "Put a +1/+1 counter on target creature" at a
/// creature controlled by `recipient_controller` while P0's watcher, built from
/// `watcher_oracle`, looks on. Returns how many of the watcher's effects
/// resolved and P0's life change.
fn watcher_response_to_opponent_counter(
    watcher_oracle: &str,
    recipient_controller: PlayerId,
) -> (usize, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario
        .add_creature_from_oracle(P0, "Counter Watcher", 2, 2, watcher_oracle)
        .id();
    let recipient = scenario
        .add_creature(recipient_controller, "Recipient Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Opp Bolster",
            true,
            "Put a +1/+1 counter on target creature.",
        )
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner.cast(spell).target_objects(&[recipient]).resolve();

    // Reach guard: the opponent's counter landed.
    outcome.assert_counters(recipient, CounterType::Plus1Plus1, 1);
    let firings = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == watcher
            )
        })
        .count();
    (firings, outcome.life_delta(P0))
}

/// CR 608.2c + CR 603.2: in "whenever an opponent puts one or more counters
/// on another creature they control" and "… on one or more creatures they
/// control", "they" is the opponent who placed the counters (the trigger's
/// actor), not the watcher's controller, so only a placement on that
/// opponent's creature matches the trigger event. The two forms reach the
/// "another " and "one or more " subject arms, which must read the actor scope
/// like the "a "/"an " arm (Bold Plagiarist).
///
/// Per form: a counter on the placing opponent's creature fires once (CR
/// 603.2c: 1 firing, +1 life); a counter on the watcher controller's creature
/// does not (the actor gate still passes, so only the "they control" recipient
/// gate suppresses it).
#[test]
fn counter_placement_subject_binds_they_control_to_the_placing_opponent() {
    let forms = [
        "Whenever an opponent puts one or more counters on another creature they control, you gain 1 life.",
        "Whenever an opponent puts one or more counters on one or more creatures they control, you gain 1 life.",
    ];
    let observed: Vec<_> = forms
        .iter()
        .map(|form| {
            (
                *form,
                watcher_response_to_opponent_counter(form, P1),
                watcher_response_to_opponent_counter(form, P0),
            )
        })
        .collect();
    let expected: Vec<_> = forms.iter().map(|form| (*form, (1, 1), (0, 0))).collect();
    assert_eq!(
        observed, expected,
        "(form, on the opponent's creature, on your creature) as (firings, P0 life change)"
    );
}

const P2: PlayerId = PlayerId(2);

const ANATHEMANCER: &str = "When this creature enters, it deals damage to target player equal to the number of nonbasic lands that player controls.\nUnearth {5}{B}{R} ({5}{B}{R}: Return this card from your graveyard to the battlefield. It gains haste. Exile it at the beginning of the next end step or if it would leave the battlefield. Unearth only as a sorcery.)";
const ALPHA_BRAWL: &str = "Target creature an opponent controls deals damage equal to its power to each other creature that player controls, then each of those creatures deals damage equal to its power to that creature.";
const IMPERIAL_EDICT: &str =
    "Target opponent chooses a creature they control. Destroy that creature.";
const EUNUCHS_INTRIGUES: &str = "Target opponent chooses a creature they control. Other creatures they control can't block this turn.";
const WEI_ASSASSINS: &str = "When this creature enters, target opponent chooses a creature they control. Destroy that creature.";
const ORACLE_EN_VEC: &str = "{T}: Target opponent chooses any number of creatures they control. During that player's next turn, the chosen creatures attack if able, and other creatures can't attack. At the beginning of that turn's end step, destroy each of the chosen creatures that didn't attack this turn. Activate only during your turn.";
const DONG_ZHOU: &str = "When Dong Zhou enters, target creature an opponent controls deals damage equal to its power to that player.";
const PRICE_OF_PROGRESS: &str = "Price of Progress deals damage to each player equal to twice the number of nonbasic lands that player controls.";
const ACIDIC_SOIL: &str =
    "Acidic Soil deals damage to each player equal to the number of lands they control.";
const SHADRIX_SILVERQUILL: &str = "Flying, double strike\nAt the beginning of combat on your turn, you may choose two. Each mode must target a different player.\n• Target player creates a 2/1 white and black Inkling creature token with flying.\n• Target player draws a card and loses 1 life.\n• Target player puts a +1/+1 counter on each creature they control.";
const BREAK_UNDER_PRESSURE: &str = "Target opponent sacrifices a creature or planeswalker with the greatest mana value among creatures and planeswalkers they control. You gain 2 life.";
/// The Consume half of Consecrate // Consume.
const CONSUME: &str = "Target player sacrifices a creature with the greatest power among creatures they control. You gain life equal to its power.";

/// A non-basic land for P-relative land counts.
const COLORLESS_LAND: &str = "{T}: Add {C}.";

pub(super) fn main_phase_scenario(players: u8) -> GameScenario {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 7)
    };
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// The legal set a target prompt currently offers: the live per-choice set when
/// published, else the slot's static set.
fn offered_targets(waiting: &WaitingFor) -> Option<(Vec<TargetRef>, bool)> {
    match waiting {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        }
        | WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let slot = target_slots.get(selection.current_slot);
            let legal = if selection.current_legal_targets.is_empty() {
                slot.map(|slot| slot.legal_targets.clone())
                    .unwrap_or_default()
            } else {
                selection.current_legal_targets.clone()
            };
            Some((legal, slot.is_some_and(|slot| slot.optional)))
        }
        _ => None,
    }
}

/// Drive every prompt until the stack is empty, recording each target
/// prompt's offer. A target prompt takes the first preference it offers (each
/// preference is used once); an optional prompt offering none is declined.
/// Modal prompts select `modes`; a "you may" prompt is accepted.
pub(super) fn settle_recording_offers(
    runner: &mut GameRunner,
    prefs: &[TargetRef],
    modes: &[usize],
) -> Vec<Vec<TargetRef>> {
    let mut prefs = prefs.to_vec();
    let mut offers = Vec::new();
    for _ in 0..120 {
        let waiting = runner.state().waiting_for.clone();
        if let Some((legal, optional)) = offered_targets(&waiting) {
            let target = match prefs.iter().position(|pref| legal.contains(pref)) {
                Some(index) => Some(prefs.remove(index)),
                None if optional => None,
                None => panic!("required target prompt offers no preference: {legal:?}"),
            };
            offers.push(legal);
            runner
                .act(GameAction::ChooseTarget { target })
                .expect("choose target");
            continue;
        }
        let action = match waiting {
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
                GameAction::SelectModes {
                    indices: modes.to_vec(),
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return offers,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).expect("answer prompt");
    }
    panic!("the stack did not settle; offers so far: {offers:?}");
}

pub(super) fn cast_free(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
}

/// The object offers among `offers` (the player declarations excluded).
fn object_offers(offers: &[Vec<TargetRef>]) -> Vec<Vec<TargetRef>> {
    offers
        .iter()
        .filter(|offer| {
            offer
                .iter()
                .any(|target| matches!(target, TargetRef::Object(_)))
        })
        .cloned()
        .collect()
}

/// The object is on the battlefield with no counters and no damage.
fn assert_untouched(runner: &GameRunner, id: ObjectId, label: &str) {
    let object = &runner.state().objects[&id];
    assert_eq!(
        object.zone,
        Zone::Battlefield,
        "{label} stays on the battlefield"
    );
    assert!(object.counters.is_empty(), "{label} gets no counters");
    assert_eq!(object.damage_marked, 0, "{label} is dealt no damage");
}

/// CR 115.1 + CR 608.2c: Anathemancer's "that player" is the target player, so
/// the damage counts the target player's nonbasic lands, never the caster's.
#[test]
fn anathemancer_counts_the_target_players_nonbasic_lands() {
    let mut scenario = main_phase_scenario(2);
    scenario.add_land_from_oracle(P0, "P0 Nonbasic", COLORLESS_LAND);
    scenario.add_land_from_oracle(P1, "P1 Nonbasic A", COLORLESS_LAND);
    scenario.add_land_from_oracle(P1, "P1 Nonbasic B", COLORLESS_LAND);
    let anathemancer = scenario
        .add_creature_to_hand_from_oracle(P0, "Anathemancer", 2, 2, ANATHEMANCER)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(anathemancer).target_player(P1).resolve();

    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "P1 controls two nonbasic lands; P0's one is not counted"
    );
    assert_eq!(outcome.life_delta(P0), 0);
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ),
        "the declared player is the only target: {:?}",
        outcome.final_waiting_for()
    );
}

/// CR 608.2c + CR 120.3a: Price of Progress's "that player" is each player in
/// turn, so each player is dealt twice their own nonbasic-land count.
#[test]
fn price_of_progress_doubles_each_players_own_nonbasic_lands() {
    let mut scenario = main_phase_scenario(2);
    scenario.add_land_from_oracle(P0, "P0 Nonbasic", COLORLESS_LAND);
    scenario.add_land_from_oracle(P1, "P1 Nonbasic A", COLORLESS_LAND);
    scenario.add_land_from_oracle(P1, "P1 Nonbasic B", COLORLESS_LAND);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Price of Progress", true, PRICE_OF_PROGRESS)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    assert_eq!(outcome.life_delta(P1), -4, "twice P1's two nonbasic lands");
    assert_eq!(outcome.life_delta(P0), -2, "twice P0's one nonbasic land");
}

/// Sibling of the Price of Progress row without the composite amount: Acidic
/// Soil deals each player damage equal to that player's own land count.
#[test]
fn acidic_soil_counts_each_players_own_lands() {
    let mut scenario = main_phase_scenario(2);
    scenario.add_land_from_oracle(P0, "P0 Nonbasic", COLORLESS_LAND);
    scenario.add_land_from_oracle(P1, "P1 Nonbasic A", COLORLESS_LAND);
    scenario.add_land_from_oracle(P1, "P1 Nonbasic B", COLORLESS_LAND);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Acidic Soil", false, ACIDIC_SOIL)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    assert_eq!(outcome.life_delta(P1), -2, "P1's two lands");
    assert_eq!(outcome.life_delta(P0), -1, "P0's one land");
}

/// CR 109.4 + CR 608.2c: Alpha Brawl's "that player" is the controller of the
/// target creature, so its other creatures are dealt damage and the caster's
/// are not. (Clause 2, "each of those creatures deals damage … to that
/// creature", is issue #9749 and is not asserted.)
#[test]
fn alpha_brawl_damages_the_target_creatures_controllers_other_creatures() {
    let mut scenario = main_phase_scenario(2);
    let a = scenario.add_creature(P1, "P1 Brawler A", 3, 3).id();
    let b = scenario.add_creature(P1, "P1 Bystander B", 2, 2).id();
    let c = scenario.add_creature(P1, "P1 Bystander C", 4, 4).id();
    let d = scenario.add_creature(P0, "P0 Bystander D", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Alpha Brawl", false, ALPHA_BRAWL)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(a).resolve();

    assert_eq!(
        outcome.zone_of(b),
        Zone::Graveyard,
        "A deals 3 damage to P1's 2/2"
    );
    assert_eq!(outcome.zone_of(c), Zone::Battlefield, "P1's 4/4 survives 3");
    assert_eq!(
        outcome.zone_of(d),
        Zone::Battlefield,
        "the caster's creature is not the target creature's controller's"
    );
}

/// CR 109.4 + CR 608.2c: Dong Zhou's "that player" is the target creature's
/// controller, who is dealt damage equal to its power.
#[test]
fn dong_zhou_damages_the_target_creatures_controller() {
    let mut scenario = main_phase_scenario(2);
    let brute = scenario.add_creature(P1, "P1 Brute", 3, 3).id();
    let dong_zhou = scenario
        .add_creature_to_hand_from_oracle(P0, "Dong Zhou, the Tyrant", 3, 3, DONG_ZHOU)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(dong_zhou).target_object(brute).resolve();

    assert_eq!(outcome.life_delta(P1), -3, "P1 controls the 3-power target");
    assert_eq!(outcome.life_delta(P0), 0, "the trigger's controller is not");
}

/// CR 608.2c: Imperial Edict's "they" is the target opponent, so the creature
/// it destroys is that opponent's. This pins the binding only: who makes the
/// choice, and when, is issue #9915.
#[test]
fn imperial_edict_destroys_a_creature_the_target_opponent_controls() {
    let mut scenario = main_phase_scenario(2);
    let theirs = scenario.add_creature(P1, "P1 Beast", 2, 2).id();
    let mine = scenario.add_creature(P0, "P0 Beast", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Imperial Edict", false, IMPERIAL_EDICT)
        .id();
    let mut runner = scenario.build();

    // The caster's creature is listed first: a creature choice that admits it
    // takes it.
    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .target_objects(&[mine, theirs])
        .resolve();

    assert_eq!(outcome.zone_of(theirs), Zone::Graveyard);
    assert_eq!(outcome.zone_of(mine), Zone::Battlefield);
}

/// The three-player X/Y/Z board: P1 controls X, P2 controls Y, P0 controls Z.
struct TargetOpponentBoard {
    x: ObjectId,
    y: ObjectId,
    z: ObjectId,
}

fn target_opponent_board(scenario: &mut GameScenario) -> TargetOpponentBoard {
    TargetOpponentBoard {
        x: scenario.add_creature(P1, "P1 X", 2, 2).id(),
        y: scenario.add_creature(P2, "P2 Y", 2, 2).id(),
        z: scenario.add_creature(P0, "P0 Z", 2, 2).id(),
    }
}

/// CR 608.2c: in the Imperial Edict family, "a creature they control" is a
/// creature the target opponent controls: with P1 declared, every creature
/// offer is exactly P1's X, never P2's Y or the caster's Z. The binding level
/// only (#9915 tracks who makes the choice).
#[test]
fn target_opponent_family_offers_only_the_target_opponents_creatures() {
    // (name, Oracle text, X's zone after resolution)
    for (name, text, x_zone) in [
        ("Imperial Edict", IMPERIAL_EDICT, Zone::Graveyard),
        ("Eunuchs' Intrigues", EUNUCHS_INTRIGUES, Zone::Battlefield),
    ] {
        let mut scenario = main_phase_scenario(3);
        let board = target_opponent_board(&mut scenario);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, name, false, text)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        cast_free(&mut runner, spell);
        let offers = settle_recording_offers(
            &mut runner,
            &[
                TargetRef::Player(P1),
                TargetRef::Object(board.z),
                TargetRef::Object(board.y),
                TargetRef::Object(board.x),
            ],
            &[],
        );
        assert_eq!(
            object_offers(&offers),
            vec![vec![TargetRef::Object(board.x)]],
            "{name}: offers {offers:?}"
        );
        assert_eq!(runner.state().objects[&board.x].zone, x_zone, "{name}");
        assert_untouched(&runner, board.y, &format!("{name}: P2's Y"));
        assert_untouched(&runner, board.z, &format!("{name}: P0's Z"));
    }

    // Triggered: Wei Assassins' enters trigger.
    let mut scenario = main_phase_scenario(3);
    let board = target_opponent_board(&mut scenario);
    let assassins = scenario
        .add_creature_to_hand_from_oracle(P0, "Wei Assassins", 3, 2, WEI_ASSASSINS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    cast_free(&mut runner, assassins);
    let offers = settle_recording_offers(
        &mut runner,
        &[
            TargetRef::Player(P1),
            TargetRef::Object(board.z),
            TargetRef::Object(assassins),
            TargetRef::Object(board.y),
            TargetRef::Object(board.x),
        ],
        &[],
    );
    assert_eq!(
        object_offers(&offers),
        vec![vec![TargetRef::Object(board.x)]],
        "Wei Assassins: offers {offers:?}"
    );
    assert_eq!(runner.state().objects[&board.x].zone, Zone::Graveyard);
    assert_untouched(&runner, board.y, "Wei Assassins: P2's Y");
    assert_untouched(&runner, board.z, "Wei Assassins: P0's Z");

    // Activated: Oracle en-Vec.
    let mut scenario = main_phase_scenario(3);
    let board = target_opponent_board(&mut scenario);
    let en_vec = scenario
        .add_creature_from_oracle(P0, "Oracle en-Vec", 1, 1, ORACLE_EN_VEC)
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: en_vec,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Oracle en-Vec");
    let offers = settle_recording_offers(
        &mut runner,
        &[
            TargetRef::Player(P1),
            TargetRef::Object(board.z),
            TargetRef::Object(en_vec),
            TargetRef::Object(board.y),
            TargetRef::Object(board.x),
        ],
        &[],
    );
    assert_eq!(
        object_offers(&offers),
        vec![vec![TargetRef::Object(board.x)]],
        "Oracle en-Vec: offers {offers:?}"
    );
    assert_untouched(&runner, board.y, "Oracle en-Vec: P2's Y");
    assert_untouched(&runner, board.z, "Oracle en-Vec: P0's Z");
}

/// CR 608.2c + CR 115.1: in a modal trigger whose modes must target different
/// players, "each creature they control" is the player that mode targets.
#[test]
fn shadrix_silverquill_counter_mode_reads_its_own_target_player() {
    // (mode 2's player, mode 3's player)
    for (draws, counters) in [(P0, P1), (P1, P0)] {
        let mut scenario = main_phase_scenario(2);
        let shadrix = scenario
            .add_creature_from_oracle(P0, "Shadrix Silverquill", 2, 5, SHADRIX_SILVERQUILL)
            .id();
        let p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
        let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
        scenario.with_library_top(P0, &["P0 Card A", "P0 Card B"]);
        scenario.with_library_top(P1, &["P1 Card A", "P1 Card B"]);
        let mut runner = scenario.build();
        let hand_before = |runner: &GameRunner, player: PlayerId| {
            runner.state().players[player.0 as usize].hand.len()
        };
        let hands = (hand_before(&runner, P0), hand_before(&runner, P1));

        // Advance once: re-calling `advance_to_phase` mid-step re-fires the
        // beginning-of-combat trigger.
        runner.advance_to_phase(Phase::BeginCombat);
        settle_recording_offers(
            &mut runner,
            &[TargetRef::Player(draws), TargetRef::Player(counters)],
            &[1, 2],
        );

        let state = runner.state();
        let plus_one = |id: ObjectId| {
            state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        };
        // Reach guard: mode 2 resolved for its player.
        let drawer_hand_before = if draws == P0 { hands.0 } else { hands.1 };
        assert_eq!(
            state.players[draws.0 as usize].hand.len(),
            drawer_hand_before + 1,
            "mode 2's player draws a card"
        );
        assert_eq!(state.players[draws.0 as usize].life, 19);

        let (counted, uncounted): (Vec<ObjectId>, Vec<ObjectId>) = if counters == P1 {
            (vec![p1_bear], vec![p0_bear, shadrix])
        } else {
            (vec![p0_bear, shadrix], vec![p1_bear])
        };
        for id in counted {
            assert_eq!(plus_one(id), 1, "mode 3's player's creature gets a counter");
        }
        for id in uncounted {
            assert_eq!(plus_one(id), 0, "mode 2's player's creature gets none");
        }
    }
}

/// The mana-value board: P1 controls X1 (creature, MV 2, 2/2), X2 (creature,
/// MV `x2_mana_value`, 3/3) and XP (planeswalker, MV 4); P2 controls Y
/// (creature, MV 5); P0 controls Z (creature, MV 6), Z2 (creature, MV 1) and ZP
/// (planeswalker, MV 3). Shared with `strategic_betrayal_6505.rs`.
pub(super) struct ManaValueBoard {
    pub(super) x1: ObjectId,
    pub(super) x2: ObjectId,
    pub(super) xp: ObjectId,
    pub(super) y: ObjectId,
    pub(super) z: ObjectId,
    pub(super) z2: ObjectId,
    pub(super) zp: ObjectId,
}

pub(super) fn mana_value_board(scenario: &mut GameScenario, x2_mana_value: u32) -> ManaValueBoard {
    ManaValueBoard {
        x1: scenario
            .add_creature(P1, "P1 X1", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id(),
        x2: scenario
            .add_creature(P1, "P1 X2", 3, 3)
            .with_mana_cost(ManaCost::generic(x2_mana_value))
            .id(),
        xp: scenario
            .add_creature(P1, "P1 XP", 0, 0)
            .as_planeswalker_with_loyalty("Jace", 3)
            .with_mana_cost(ManaCost::generic(4))
            .id(),
        y: scenario
            .add_creature(P2, "P2 Y", 2, 2)
            .with_mana_cost(ManaCost::generic(5))
            .id(),
        z: scenario
            .add_creature(P0, "P0 Z", 2, 2)
            .with_mana_cost(ManaCost::generic(6))
            .id(),
        z2: scenario
            .add_creature(P0, "P0 Z2", 1, 1)
            .with_mana_cost(ManaCost::generic(1))
            .id(),
        zp: scenario
            .add_creature(P0, "P0 ZP", 0, 0)
            .as_planeswalker_with_loyalty("Chandra", 3)
            .with_mana_cost(ManaCost::generic(3))
            .id(),
    }
}

/// CR 608.2c + CR 202.3: Break Under Pressure's greatest mana value is taken
/// over the target opponent's creatures and planeswalkers as one group, and
/// that opponent chooses among the tied ones as the spell resolves.
#[test]
fn break_under_pressure_sacrifices_the_target_opponents_greatest_mana_value() {
    // Tie: X2 and XP are P1's greatest (MV 4); P2's Y (MV 5) and P0's Z (MV 6)
    // are higher but not P1's.
    let mut scenario = main_phase_scenario(3);
    let board = mana_value_board(&mut scenario, 4);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Break Under Pressure", true, BREAK_UNDER_PRESSURE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "the target opponent sacrifices");
            let mut offered = cards.clone();
            offered.sort();
            let mut expected = vec![board.x2, board.xp];
            expected.sort();
            assert_eq!(offered, expected, "P1's tied greatest mana values");
        }
        other => panic!("expected P1's sacrifice choice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![board.x2],
        })
        .expect("P1 sacrifices X2");
    let state = runner.state();
    assert_eq!(state.objects[&board.x2].zone, Zone::Graveyard);
    for (id, label) in [
        (board.x1, "X1"),
        (board.xp, "XP"),
        (board.y, "Y"),
        (board.z, "Z"),
        (board.z2, "Z2"),
        (board.zp, "ZP"),
    ] {
        assert_eq!(state.objects[&id].zone, Zone::Battlefield, "{label}");
    }

    // XP alone is P1's greatest: it is sacrificed with no choice.
    let mut scenario = main_phase_scenario(3);
    let board = mana_value_board(&mut scenario, 1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Break Under Pressure", true, BREAK_UNDER_PRESSURE)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert_eq!(outcome.zone_of(board.xp), Zone::Graveyard);
    for id in [board.x1, board.x2, board.y, board.z, board.z2, board.zp] {
        assert_eq!(outcome.zone_of(id), Zone::Battlefield);
    }
}

/// CR 608.2c: Consume's greatest power is taken over the target player's
/// creatures.
#[test]
fn consume_sacrifices_the_target_players_greatest_power() {
    let mut scenario = main_phase_scenario(3);
    let board = mana_value_board(&mut scenario, 4);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume", false, CONSUME)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    assert_eq!(outcome.zone_of(board.x2), Zone::Graveyard, "P1's 3/3");
    assert_eq!(outcome.zone_of(board.x1), Zone::Battlefield, "P1's 2/2");
    for id in [board.y, board.z, board.z2] {
        assert_eq!(outcome.zone_of(id), Zone::Battlefield);
    }
}

// ---- Phase-4 frames: the punisher document antecedent, the accepting player ----

const SIRENS_CALL: &str = "Cast this spell only during an opponent's turn, before attackers are declared.\nCreatures the active player controls attack this turn if able.\nAt the beginning of the next end step, destroy all non-Wall creatures that player controls that didn't attack this turn. Ignore this effect for each creature the player didn't control continuously since the beginning of the turn.";

const BROWBEAT: &str = "Any player may have Browbeat deal 5 damage to them. If no one does, target player draws three cards.";

const DESECRATION_DEMON: &str = "Flying\nAt the beginning of each combat, any opponent may sacrifice a creature of their choice. If a player does, tap this creature and put a +1/+1 counter on it.";

/// Drive priority until the stack is empty, answering no other prompt.
fn pass_until_stack_empty(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
}

/// CR 113.3a + CR 608.2c + CR 302.6: Siren's Call's third paragraph is part of
/// the same spell ability as its coerce paragraph, so "that player" is the
/// active player. At the end step the active player's non-attacking creatures
/// are destroyed; an attacker, another opponent's creature and the caster's
/// creature survive.
#[test]
fn sirens_call_destroys_only_the_active_players_creatures_that_did_not_attack() {
    let mut scenario = main_phase_scenario(3);
    let attacker = scenario.add_creature(P1, "P1 Attacker A", 2, 2).id();
    let tapped = scenario.add_creature(P1, "P1 Tapped B", 2, 2).id();
    let bystander = scenario.add_creature(P2, "P2 Bystander C", 2, 2).id();
    let casters = scenario.add_creature(P0, "P0 Bystander D", 2, 2).id();
    let sirens_call = scenario
        .add_spell_to_hand_from_oracle(P0, "Siren's Call", true, SIRENS_CALL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.objects.get_mut(&tapped).expect("B").tapped = true;
    }
    cast_free(&mut runner, sirens_call);
    pass_until_stack_empty(&mut runner);
    assert_eq!(
        runner.state().objects[&sirens_call].zone,
        Zone::Graveyard,
        "reach-guard: Siren's Call resolved"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 declares its untapped creature, which must attack");
    declare_no_blockers(&mut runner);
    runner.combat_damage();
    runner.advance_to_end_step();
    pass_until_stack_empty(&mut runner);

    let zone = |id: ObjectId| runner.state().objects[&id].zone;
    assert_eq!(
        zone(tapped),
        Zone::Graveyard,
        "the active player's creature that didn't attack is destroyed"
    );
    assert_eq!(zone(attacker), Zone::Battlefield, "the attacker survives");
    assert_eq!(
        zone(bystander),
        Zone::Battlefield,
        "a non-active opponent's creature is not the active player's"
    );
    assert_eq!(
        zone(casters),
        Zone::Battlefield,
        "the caster's creature is not the active player's"
    );
}

/// Reservoir Kraken with one 2/2 per player; P1's may be tapped or hexproof.
struct KrakenBoard {
    kraken: ObjectId,
    p0_bear: ObjectId,
    p1_bear: ObjectId,
    p2_bear: Option<ObjectId>,
}

/// What the accepting-player frame asked during one beginning-of-combat
/// trigger: the players offered, in order, and each resolution-time creature
/// choice with its selector and legal set.
struct KrakenDrive {
    offered: Vec<PlayerId>,
    choices: Vec<(PlayerId, Vec<ObjectId>)>,
}

fn kraken_board(
    players: u8,
    active: PlayerId,
    p1_bear_tapped: bool,
    p1_bear_hexproof: bool,
) -> (GameRunner, KrakenBoard) {
    let mut scenario = GameScenario::new_n_player(players, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let kraken = scenario
        .add_creature_from_oracle(P0, "Reservoir Kraken", 6, 6, RESERVOIR_KRAKEN)
        .id();
    let p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_bear = if p1_bear_hexproof {
        scenario.add_creature(P1, "P1 Bear", 2, 2).hexproof().id()
    } else {
        scenario.add_creature(P1, "P1 Bear", 2, 2).id()
    };
    let p2_bear = (players > 2).then(|| scenario.add_creature(P2, "P2 Bear", 2, 2).id());
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = active;
        state.priority_player = active;
        state.waiting_for = WaitingFor::Priority { player: active };
        state.objects.get_mut(&p1_bear).expect("P1 Bear").tapped = p1_bear_tapped;
    }
    (
        runner,
        KrakenBoard {
            kraken,
            p0_bear,
            p1_bear,
            p2_bear,
        },
    )
}

/// Advance once to the beginning of combat and drive Reservoir Kraken's
/// trigger: every offer is accepted and each choice takes its first legal
/// creature.
fn drive_kraken_trigger(runner: &mut GameRunner) -> KrakenDrive {
    // Advance once: re-calling `advance_to_phase` mid-step re-fires the
    // beginning-of-combat trigger.
    runner.advance_to_phase(Phase::BeginCombat);
    let mut drive = KrakenDrive {
        offered: Vec::new(),
        choices: Vec::new(),
    };
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                drive.offered.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::MultiTargetSelection {
                player,
                legal_targets,
                ..
            } => {
                let cards = legal_targets.first().copied().into_iter().collect();
                drive.choices.push((player, legal_targets));
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("choose a creature");
            }
            // CR 115.10a + CR 608.2d: the accepting body calls its creature no
            // "target", so the accepting player chooses it on resolution.
            WaitingFor::TriggerTargetSelection { .. } => panic!(
                "the accepting body declares no target, so no stack-time target prompt: {:?}",
                runner.state().waiting_for
            ),
            WaitingFor::Priority { .. } if runner.state().phase == Phase::BeginCombat => {
                if runner.state().stack.is_empty() {
                    return drive;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return drive,
        }
    }
    panic!("the beginning-of-combat trigger did not settle");
}

fn fish_tokens(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name.contains("Fish"))
        .count()
}

/// CR 608.2c + CR 608.2d: on the opponent's own combat, the opponent who
/// accepts still chooses among their own creatures, on resolution.
#[test]
fn reservoir_kraken_on_the_opponents_combat_offers_the_opponents_creatures() {
    let (mut runner, board) = kraken_board(2, P1, false, false);
    let drive = drive_kraken_trigger(&mut runner);

    assert_eq!(drive.offered, vec![P1], "reach-guard: P1 is offered");
    assert_eq!(drive.choices, vec![(P1, vec![board.p1_bear])]);
    let state = runner.state();
    assert!(state.objects[&board.p1_bear].tapped);
    assert!(state.objects[&board.kraken].tapped);
    assert!(!state.objects[&board.p0_bear].tapped);
    assert_eq!(fish_tokens(&runner), 1);
}

/// CR 608.2c + CR 608.2d + CR 101.4, hostile multi-authority board: three
/// players, P1's combat, P1's only creature already tapped. The offers run in
/// APNAP order; the accepting player each time chooses among their own
/// untapped creatures, so P2, who can, taps P2's creature.
#[test]
fn reservoir_kraken_three_players_each_acceptor_chooses_among_their_own_creatures() {
    let (mut runner, board) = kraken_board(3, P1, true, false);
    let p2_bear = board.p2_bear.expect("P2 Bear");
    let drive = drive_kraken_trigger(&mut runner);

    assert_eq!(drive.offered, vec![P1, P2], "reach-guard: APNAP offers");
    assert!(
        drive.choices.contains(&(P2, vec![p2_bear])),
        "P2 chooses among P2's untapped creatures; choices={:?}",
        drive.choices
    );
    assert!(
        drive
            .choices
            .iter()
            .all(|(_, legal)| !legal.contains(&board.p0_bear)),
        "the Kraken's controller's creature is never an acceptor's choice; choices={:?}",
        drive.choices
    );
    let state = runner.state();
    assert!(state.objects[&p2_bear].tapped);
    assert!(state.objects[&board.kraken].tapped);
    assert!(!state.objects[&board.p0_bear].tapped);
    assert_eq!(fish_tokens(&runner), 1);
}

/// CR 608.2c + CR 608.2d: three players on the Kraken controller's own combat,
/// so the accepting player is not the active player.
#[test]
fn reservoir_kraken_three_players_on_its_controllers_combat() {
    let (mut runner, board) = kraken_board(3, P0, true, false);
    let p2_bear = board.p2_bear.expect("P2 Bear");
    let drive = drive_kraken_trigger(&mut runner);

    assert!(
        drive.offered.contains(&P2),
        "reach-guard: P2 is offered; offered={:?}",
        drive.offered
    );
    assert!(
        drive.choices.contains(&(P2, vec![p2_bear])),
        "P2 chooses among P2's untapped creatures; choices={:?}",
        drive.choices
    );
    let state = runner.state();
    assert!(state.objects[&p2_bear].tapped);
    assert!(state.objects[&board.kraken].tapped);
    assert!(!state.objects[&board.p0_bear].tapped);
    assert_eq!(fish_tokens(&runner), 1);
}

/// CR 115.10a + CR 702.11b: the accepting player's creature is chosen, not
/// targeted, so hexproof does not stop its own controller from tapping it.
#[test]
fn reservoir_kraken_acceptor_may_choose_their_hexproof_creature() {
    let (mut runner, board) = kraken_board(2, P1, false, true);
    let drive = drive_kraken_trigger(&mut runner);

    assert_eq!(drive.offered, vec![P1], "reach-guard: P1 is offered");
    assert_eq!(drive.choices, vec![(P1, vec![board.p1_bear])]);
    let state = runner.state();
    assert!(state.objects[&board.p1_bear].tapped);
    assert!(state.objects[&board.kraken].tapped);
    assert_eq!(fish_tokens(&runner), 1);
}

/// The trigger body whose head carries an "any opponent / any player may"
/// scope.
fn accepting_trigger_body(name: &str, text: &str) -> AbilityDefinition {
    let parsed = parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]);
    parsed
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref())
        .find(|execute| execute.optional_for.is_some())
        .cloned()
        .unwrap_or_else(|| panic!("{name}: no accepting-player trigger body in {parsed:?}"))
}

/// Every `ScopedPlayer` player reference anywhere in a definition (filters,
/// amounts, conditions, nested chains and branches), read from its serialized
/// form.
fn scoped_player_refs(definition: &AbilityDefinition) -> usize {
    serde_json::to_string(definition)
        .expect("a definition serializes")
        .matches("\"ScopedPlayer\"")
        .count()
}

fn typed_controller(filter: &TargetFilter) -> Option<ControllerRef> {
    match filter {
        TargetFilter::Typed(typed) => typed.controller.clone(),
        other => panic!("expected a typed filter, got {other:?}"),
    }
}

/// The accepting head and its sub node, each with its operand's controller and
/// its timing.
fn head_and_sub(
    body: &AbilityDefinition,
) -> (
    (Option<ControllerRef>, TargetChoiceTiming),
    (Option<ControllerRef>, TargetChoiceTiming),
) {
    let sub = body.sub_ability.as_deref().expect("a sub node");
    let operand = |def: &AbilityDefinition| {
        typed_controller(def.effect.target_filter().expect("a direct operand"))
    };
    (
        (operand(body), body.target_choice_timing),
        (operand(sub), sub.target_choice_timing),
    )
}

/// CR 608.2c + CR 608.2d + CR 115.10a: Reservoir Kraken's accepting body binds
/// "they" to the accepting player and chooses its creature on resolution. A
/// player recipient (Browbeat) is neither re-timed nor seeded; a body with no
/// anaphor (Desecration Demon) is re-timed only.
#[test]
fn accepting_player_body_binds_they_and_resolves_its_described_object() {
    let kraken = accepting_trigger_body("Reservoir Kraken", RESERVOIR_KRAKEN);
    assert_eq!(kraken.optional_for, Some(OpponentMayScope::AnyOpponent));
    let Effect::SetTapState { target, .. } = kraken.effect.as_ref() else {
        panic!("expected the SetTapState head, got {:?}", kraken.effect);
    };
    assert_eq!(typed_controller(target), Some(ControllerRef::ScopedPlayer));
    assert_eq!(kraken.target_choice_timing, TargetChoiceTiming::Resolution);

    let browbeat = parse_oracle_text(BROWBEAT, "Browbeat", &[], &["Sorcery".to_string()], &[]);
    let head = &browbeat.abilities[0];
    assert_eq!(head.optional_for, Some(OpponentMayScope::AnyPlayer));
    assert!(
        matches!(
            head.effect.as_ref(),
            Effect::DealDamage {
                target: TargetFilter::Player,
                ..
            }
        ),
        "{:?}",
        head.effect
    );
    assert_eq!(head.target_choice_timing, TargetChoiceTiming::Stack);
    assert_eq!(scoped_player_refs(head), 0);

    let demon = accepting_trigger_body("Desecration Demon", DESECRATION_DEMON);
    let Effect::Sacrifice { target, .. } = demon.effect.as_ref() else {
        panic!("expected the Sacrifice head, got {:?}", demon.effect);
    };
    assert_ne!(
        typed_controller(target),
        Some(ControllerRef::ScopedPlayer),
        "Desecration Demon is stamp-only: the accepting-player seed binds no anaphor in its body"
    );
    assert_eq!(demon.target_choice_timing, TargetChoiceTiming::Resolution);
}

/// CR 608.2c + CR 608.2d + CR 115.10a: a `try_split_targeted_compound`
/// continuation of an accepting body binds "they" and resolves its described
/// object, like the head.
#[test]
fn accepting_body_continuation_binds_and_resolves_with_its_head() {
    let body = accepting_trigger_body(
        "Probe",
        "At the beginning of each combat, any opponent may tap an untapped creature they control and goad an untapped creature they control.",
    );
    assert_eq!(body.optional_for, Some(OpponentMayScope::AnyOpponent));
    assert!(matches!(body.effect.as_ref(), Effect::SetTapState { .. }));
    assert!(matches!(
        body.sub_ability.as_deref().map(|sub| sub.effect.as_ref()),
        Some(Effect::Goad { .. })
    ));
    let resolved = (
        Some(ControllerRef::ScopedPlayer),
        TargetChoiceTiming::Resolution,
    );
    assert_eq!(head_and_sub(&body), (resolved.clone(), resolved));
}

/// CR 608.2c + CR 608.2d + CR 115.10a: "tap and goad <one object>" shares one
/// described object between the head and its `Goad` sub node; both bind
/// "they" and resolve.
#[test]
fn accepting_body_tap_and_goad_binds_and_resolves_both_nodes() {
    let body = accepting_trigger_body(
        "Probe",
        "At the beginning of each combat, any opponent may tap and goad an untapped creature they control.",
    );
    assert_eq!(body.optional_for, Some(OpponentMayScope::AnyOpponent));
    assert!(matches!(body.effect.as_ref(), Effect::SetTapState { .. }));
    assert!(matches!(
        body.sub_ability.as_deref().map(|sub| sub.effect.as_ref()),
        Some(Effect::Goad { .. })
    ));
    let resolved = (
        Some(ControllerRef::ScopedPlayer),
        TargetChoiceTiming::Resolution,
    );
    assert_eq!(head_and_sub(&body), (resolved.clone(), resolved));
}

/// CR 115.10a: an accepting body that prints "target" keeps its stack-time
/// targets, so the accepting-player binding is withheld from every node.
#[test]
fn accepting_body_that_prints_target_is_not_seeded() {
    let body = accepting_trigger_body(
        "Probe",
        "At the beginning of each combat, any opponent may tap target creature and goad an untapped creature they control.",
    );
    assert_eq!(body.optional_for, Some(OpponentMayScope::AnyOpponent));
    assert!(matches!(body.effect.as_ref(), Effect::SetTapState { .. }));
    assert!(matches!(
        body.sub_ability.as_deref().map(|sub| sub.effect.as_ref()),
        Some(Effect::Goad {
            target: TargetFilter::Typed(_)
        })
    ));
    assert_eq!(scoped_player_refs(&body), 0, "{body:?}");
}

/// CR 608.2c: a relative scope the trigger supplies ("that player", the damaged
/// player) is the continuation's antecedent; the accepting-player seed does not
/// replace it.
#[test]
fn accepting_body_keeps_a_trigger_supplied_that_player() {
    let body = accepting_trigger_body(
        "Probe",
        "Whenever this creature deals combat damage to a player, any opponent may tap target creature and goad a creature that player controls.",
    );
    assert_eq!(body.optional_for, Some(OpponentMayScope::AnyOpponent));
    assert!(matches!(body.effect.as_ref(), Effect::SetTapState { .. }));
    let Some(Effect::Goad { target }) = body.sub_ability.as_deref().map(|sub| sub.effect.as_ref())
    else {
        panic!("expected the Goad continuation, got {:?}", body.sub_ability);
    };
    assert_eq!(
        typed_controller(target),
        Some(ControllerRef::TriggeringPlayer)
    );
    assert_eq!(scoped_player_refs(&body), 0, "{body:?}");
}

/// The accepting-player seed reaches the emitted definition only in a direct
/// operand of a resolution-timed node of the accepting body's own chain. Each
/// of these bodies has its anaphor elsewhere (a delayed payload, a
/// `ChooseOneOf` branch, an amount), so none carries `ScopedPlayer`.
#[test]
fn accepting_body_anaphors_outside_its_own_operands_are_not_seeded() {
    // A delayed payload: the accepting body is wrapped by its trailing delayed
    // condition, so the accepting frame sits inside the payload, not on the
    // chain the seed was probed for.
    let parsed = parse_oracle_text(
        "At the beginning of each combat, any opponent may tap an untapped creature they control at the beginning of the next end step.",
        "Probe",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let execute = parsed
        .triggers
        .iter()
        .find_map(|trigger| trigger.execute.as_deref())
        .expect("reach-guard: the combat trigger has a body");
    let Effect::CreateDelayedTrigger { effect, .. } = execute.effect.as_ref() else {
        panic!(
            "reach-guard: expected a delayed trigger, got {:?}",
            execute.effect
        );
    };
    assert_eq!(effect.optional_for, Some(OpponentMayScope::AnyOpponent));
    assert!(
        matches!(
            effect.effect.as_ref(),
            Effect::SetTapState {
                target: TargetFilter::Typed(_),
                ..
            }
        ),
        "reach-guard: {:?}",
        effect.effect
    );
    assert_eq!(scoped_player_refs(execute), 0, "{execute:?}");

    // `ChooseOneOf` branches: a put-counter choice, an inline "A or B" choice
    // and "have <subject> <keyword choice>".
    for text in [
        "At the beginning of each combat, any opponent may put your choice of a +1/+1 counter or a -1/-1 counter on a creature they control.",
        "At the beginning of each combat, any opponent may sacrifice a creature they control or discard a card.",
        "At the beginning of each combat, any opponent may have creatures they control gain your choice of flying or haste until end of turn.",
    ] {
        let body = accepting_trigger_body("Probe", text);
        assert_eq!(body.optional_for, Some(OpponentMayScope::AnyOpponent), "{text}");
        let Effect::ChooseOneOf { branches, .. } = body.effect.as_ref() else {
            panic!("reach-guard: expected a ChooseOneOf head for {text:?}, got {:?}", body.effect);
        };
        assert_eq!(branches.len(), 2, "reach-guard: {text}");
        assert_eq!(scoped_player_refs(&body), 0, "{text}: {body:?}");
    }

    // The head's amount.
    let body = accepting_trigger_body(
        "Probe",
        "At the beginning of each combat, any opponent may lose life equal to the number of creatures they control.",
    );
    assert_eq!(body.optional_for, Some(OpponentMayScope::AnyOpponent));
    assert!(
        matches!(body.effect.as_ref(), Effect::LoseLife { .. }),
        "reach-guard: {:?}",
        body.effect
    );
    assert_eq!(scoped_player_refs(&body), 0, "{body:?}");
}
