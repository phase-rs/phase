//! CR 608.2h + CR 109.4 + CR 113.8: who "that permanent's controller" / "that
//! spell or ability's controller" is when an event's object has changed control
//! or left its zone before the trigger resolves.
//!
//! Oracle text is verbatim from Scryfall, except fixtures labelled synthetic.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{Effect, PtValue, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, Targeter};
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GREMLIN_INFESTATION: &str = "Enchant artifact\nAt the beginning of your end step, this Aura deals 2 damage to enchanted artifact's controller.\nWhen enchanted artifact is put into a graveyard, create a 2/2 red Gremlin creature token.";
/// Synthetic: a one-line control change at instant speed.
const STEAL_ARTIFACT: &str = "Gain control of target artifact.";
/// Synthetic: an instant-speed bounce for an artifact.
const BOUNCE_ARTIFACT: &str = "Return target artifact to its owner's hand.";

fn free_spell(scenario: &mut GameScenario, owner: PlayerId, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(owner, name, true, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Pass priority (resolving nothing) until `player` holds it.
fn priority_to(runner: &mut GameRunner, player: PlayerId) {
    let depth = runner.state().stack.len();
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { player: p } if p == player => {
                assert_eq!(runner.state().stack.len(), depth, "nothing resolved");
                return;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("priority not reachable: {other:?}"),
        }
    }
    panic!("priority not reached");
}

/// Resolve the top stack object only.
fn resolve_one(runner: &mut GameRunner) {
    let depth = runner.state().stack.len();
    assert!(depth > 0, "something to resolve");
    for _ in 0..8 {
        if runner.state().stack.len() < depth {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    panic!("the top object did not resolve");
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.life(player)
}

/// Whether the enchanted artifact leaves before the end-step trigger resolves.
#[derive(Clone, Copy)]
enum HostDeparture {
    Bounced,
    Stays,
}

/// P1's Gremlin Infestation enchants P1's artifact, which P0 stole. At P1's end
/// step the Aura triggers; P0 optionally bounces the artifact in response.
/// Returns (P0, P1) life after the trigger resolves.
fn gremlin_board(departure: HostDeparture) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario.add_artifact_from_oracle(P1, "Ornament", "").id();
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Gremlin Infestation", "")
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(GREMLIN_INFESTATION)
        .id();
    let steal = free_spell(&mut scenario, P0, "Steal Artifact", STEAL_ARTIFACT);
    let bounce = free_spell(&mut scenario, P0, "Bounce Artifact", BOUNCE_ARTIFACT);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&aura).unwrap().attached_to = Some(artifact.into());
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .attachments
            .push(aura);
        state.layers_dirty.mark_full();
    }
    runner.cast(steal).target_object(artifact).resolve();
    assert_eq!(
        runner.state().objects[&artifact].controller,
        P0,
        "reach guard: P0 stole the artifact"
    );

    // P1's turn, its end step: the Aura's controller is P1 ("your end step").
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.phase = Phase::PostCombatMain;
    }
    give_priority(&mut runner, P1);
    runner.advance_to_phase(Phase::End);
    for _ in 0..4 {
        if !runner.state().stack.is_empty() {
            break;
        }
        if let WaitingFor::OrderTriggers { .. } = runner.state().waiting_for {
            drain_order_triggers_with_identity(runner.state_mut());
        } else {
            break;
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the end-step trigger is waiting"
    );

    if let HostDeparture::Bounced = departure {
        priority_to(&mut runner, P0);
        runner.cast(bounce).target_object(artifact).commit();
        resolve_one(&mut runner);
        assert_eq!(
            runner.state().objects[&artifact].zone,
            Zone::Hand,
            "reach guard: bounced to its owner"
        );
    }
    let before = (life(&runner, P0), life(&runner, P1));
    resolve_one(&mut runner);
    (life(&runner, P0) - before.0, life(&runner, P1) - before.1)
}

/// CR 608.2h + CR 303.4: "enchanted artifact's controller" when the artifact
/// left before the trigger resolved is its controller as it last existed on
/// the battlefield: P0, who stole it, not P1, who owns it.
#[test]
fn gremlin_infestation_damages_the_last_controller_of_a_departed_host() {
    assert_eq!(gremlin_board(HostDeparture::Bounced), (-2, 0));
}

/// Control: the stolen artifact stays, so its current controller (P0) is hit.
#[test]
fn gremlin_infestation_damages_the_controller_of_a_stolen_host_that_stays() {
    assert_eq!(gremlin_board(HostDeparture::Stays), (-2, 0));
}

// ---------------------------------------------------------------------------
// CR 115.1 + CR 113.8 + CR 601.2c: the targeter is recorded at announcement.
// ---------------------------------------------------------------------------

const FORSAKEN_WASTES: &str = "Players can't gain life.\nAt the beginning of each player's upkeep, that player loses 1 life.\nWhenever this enchantment becomes the target of a spell, that spell's controller loses 5 life.";
const CONFISCATE: &str = "Enchant permanent\nYou control enchanted permanent.";
const COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent enters under your control.)";
const COUNTERSPELL: &str = "Counter target spell.";
const LAVA_RUNNER: &str = "Haste\nWhenever this creature becomes the target of a spell or ability, that spell or ability's controller sacrifices a land of their choice.";
const PRODIGAL_PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";
const ELDER_DEEP_FIEND: &str = "Flash\nEmerge {5}{U}{U} (You may cast this spell by sacrificing a creature and paying the emerge cost reduced by that creature's mana value.)\nWhen you cast this spell, tap up to four target permanents.";
const AETHERSNATCH: &str = "Gain control of target spell. You may choose new targets for it. (If that spell becomes a permanent, it enters under your control.)";
const ENTS_FURY: &str = "Put a +1/+1 counter on target creature you control if its power is 4 or greater. Then that creature gets +1/+1 until end of turn and fights target creature you don't control.";
const ROYAL_DECREE: &str = "Cumulative upkeep {W}\nWhenever a Swamp, Mountain, black permanent, or red permanent becomes tapped, this enchantment deals 1 damage to that permanent's controller.";
const BOOMERANG: &str = "Return target permanent to its owner's hand.";
/// Synthetic: an instant-speed creature theft.
const STEAL_CREATURE: &str = "Gain control of target creature.";
/// Synthetic: an instant-speed creature bounce.
const BOUNCE_CREATURE: &str = "Return target creature to its owner's hand.";

fn stage_turn(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.phase = Phase::PreCombatMain;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn drain_ordering(runner: &mut GameRunner) {
    for _ in 0..8 {
        if let WaitingFor::OrderTriggers { .. } = runner.state().waiting_for {
            drain_order_triggers_with_identity(runner.state_mut());
        } else {
            return;
        }
    }
}

/// Resolve the top stack object, declining "you may" choices (new targets).
fn resolve_one_declining(runner: &mut GameRunner) {
    let depth = runner.state().stack.len();
    assert!(depth > 0, "something to resolve");
    for _ in 0..16 {
        if runner.state().stack.len() < depth
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    panic!(
        "the top object did not resolve: {:?}",
        runner.state().waiting_for
    );
}

fn lands(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Land)
        })
        .count()
}

#[derive(Clone, Copy, Debug)]
enum SpellFate {
    StaysOnStack,
    Countered,
}

fn confiscate_in_hand(scenario: &mut GameScenario, cost: ManaCost) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P1, "Confiscate", false, CONFISCATE)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], CONFISCATE)
        .with_mana_cost(cost)
        .id()
}

fn one_colorless(scenario: &mut GameScenario, player: PlayerId) {
    scenario.with_mana_pool(
        player,
        vec![engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
}

/// P1 casts Confiscate targeting P0's Forsaken Wastes; P0 Commandeers the
/// Confiscate (declining new targets) when `commandeer`, then optionally
/// counters it. Returns (P0, P1) life deltas from the Wastes trigger only.
fn wastes_commandeer_board(commandeer: bool, fate: SpellFate) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wastes = scenario
        .add_enchantment_from_oracle(P0, "Forsaken Wastes", FORSAKEN_WASTES)
        .id();
    let confiscate = confiscate_in_hand(&mut scenario, ManaCost::zero());
    let commandeer_id = free_spell(&mut scenario, P0, "Commandeer", COMMANDEER);
    let counterspell = free_spell(&mut scenario, P0, "Counterspell", COUNTERSPELL);
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    runner.cast(confiscate).target_object(wastes).commit();
    drain_ordering(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: Confiscate and one Wastes trigger"
    );

    if commandeer {
        priority_to(&mut runner, P0);
        runner
            .cast(commandeer_id)
            .target_object(confiscate)
            .commit();
        resolve_one_declining(&mut runner);
        assert_eq!(
            runner.state().objects[&confiscate].controller,
            P0,
            "reach guard: Commandeer took the spell"
        );
    }
    if let SpellFate::Countered = fate {
        priority_to(&mut runner, P0);
        runner.cast(counterspell).target_object(confiscate).commit();
        resolve_one_declining(&mut runner);
        assert_ne!(
            runner.state().objects[&confiscate].zone,
            Zone::Stack,
            "reach guard: countered"
        );
    }
    let expected_depth = match fate {
        SpellFate::StaysOnStack => 2,
        SpellFate::Countered => 1,
    };
    assert_eq!(
        runner.state().stack.len(),
        expected_depth,
        "reach guard: the Wastes trigger is on top"
    );
    let before = (life(&runner, P0), life(&runner, P1));
    resolve_one_declining(&mut runner);
    (life(&runner, P0) - before.0, life(&runner, P1) - before.1)
}

/// CR 109.4 + CR 113.8: "that spell's controller" for a spell whose control
/// changed on the stack is its current controller (P0, who Commandeered it).
#[test]
fn forsaken_wastes_hits_the_commandeering_player() {
    assert_eq!(
        wastes_commandeer_board(true, SpellFate::StaysOnStack),
        (-5, 0)
    );
}

/// CR 608.2h: the Commandeered spell is countered before the trigger
/// resolves; its controller as it last existed on the stack is P0.
#[test]
fn forsaken_wastes_hits_the_last_controller_of_a_countered_spell() {
    assert_eq!(wastes_commandeer_board(true, SpellFate::Countered), (-5, 0));
}

/// Control: no control change, so the caster (P1) is hit, live or countered.
#[test]
fn forsaken_wastes_hits_the_caster_without_commandeer() {
    assert_eq!(
        wastes_commandeer_board(false, SpellFate::StaysOnStack),
        (0, -5)
    );
    assert_eq!(
        wastes_commandeer_board(false, SpellFate::Countered),
        (0, -5)
    );
}

/// CR 601.2a + CR 601.2c: the announcement recorded on the targeting event is
/// the one the finalized spell still carries after an interactive payment
/// pause and the move to the stack.
///
/// Scope: this proves the identity hand-off ONLY. It does not prove that a
/// "becomes the target of a spell" trigger fires under interactive payment:
/// it doesn't today, a pre-existing casting-payment defect disclosed on the
/// PR and tracked for its own follow-up PR.
#[test]
fn interactive_payment_keeps_the_announcement_its_targeting_event_recorded() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wastes = scenario
        .add_enchantment_from_oracle(P0, "Forsaken Wastes", FORSAKEN_WASTES)
        .id();
    let confiscate = confiscate_in_hand(&mut scenario, ManaCost::generic(1));
    one_colorless(&mut scenario, P1);
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    let card_id = runner.state().objects[&confiscate].card_id;
    let mut recorded = Vec::new();
    let result = runner
        .act(GameAction::CastSpell {
            object_id: confiscate,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Manual,
        })
        .expect("begin the cast");
    recorded.extend(result.events);
    let mut paused = false;
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                let r = runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(wastes)],
                    })
                    .expect("target Wastes");
                recorded.extend(r.events);
            }
            WaitingFor::ManaPayment { .. } => {
                paused = true;
                runner.act(GameAction::PassPriority).expect("pay");
            }
            _ => break,
        }
    }
    assert!(
        paused,
        "reach guard: the cast paused at interactive payment"
    );
    let targeter = recorded.iter().find_map(|event| match event {
        GameEvent::BecomesTarget {
            source_id,
            targeter,
            ..
        } if *source_id == confiscate => Some(*targeter),
        _ => None,
    });
    let obj = &runner.state().objects[&confiscate];
    assert_eq!(obj.zone, Zone::Stack, "reach guard: finalized");
    let announcement = obj.spell_announcement.expect("the spell keeps it");
    assert_eq!(
        targeter,
        Some(Some(Targeter::Spell(announcement))),
        "the targeting event names the finalized spell"
    );
}

/// CR 733.1: a cast backed out at payment is undone — its announcement is
/// cleared, and a later cast gets a fresh one.
///
/// Scope: the absent Wastes trigger is NOT an independent rollback witness.
/// Manual payment never collects the cast's becomes-target trigger anyway (the
/// pre-existing interactive-payment defect disclosed on the PR), so this row
/// proves the announcement lifecycle only.
#[test]
fn a_cancelled_cast_triggers_nothing_and_its_announcement_is_never_reused() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wastes = scenario
        .add_enchantment_from_oracle(P0, "Forsaken Wastes", FORSAKEN_WASTES)
        .id();
    let confiscate = scenario
        .add_spell_to_hand_from_oracle(P1, "Confiscate", false, CONFISCATE)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], CONFISCATE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P1,
        vec![engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    let card_id = runner.state().objects[&confiscate].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: confiscate,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Manual,
        })
        .expect("begin the cast");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(wastes)],
            })
            .expect("target Wastes");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: paused at payment, {:?}",
        runner.state().waiting_for
    );
    let first = runner.state().objects[&confiscate]
        .spell_announcement
        .expect("reach guard: announced");
    runner.act(GameAction::CancelCast).expect("cancel");
    drain_ordering(&mut runner);
    assert!(runner.state().stack.is_empty(), "nothing went on the stack");
    assert_eq!((life(&runner, P0), life(&runner, P1)), (20, 20));
    assert_eq!(
        runner.state().objects[&confiscate].spell_announcement,
        None,
        "the undone announcement is cleared"
    );
    stage_turn(&mut runner, P1);
    let card_id = runner.state().objects[&confiscate].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: confiscate,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Manual,
        })
        .expect("cast again");
    let second = runner.state().objects[&confiscate]
        .spell_announcement
        .expect("announced again");
    assert!(second > first, "a fresh, never-reused announcement");
}

/// Matt's board: P1 controls a Mountain P0 owns and taps it for mana; Royal
/// Decree triggers; P0 returns the Mountain to its owner's hand in response.
/// CR 608.2h: "that permanent's controller" is P1, its last controller.
#[test]
fn royal_decree_damages_the_tapper_of_a_departed_borrowed_mountain() {
    for bounce in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(P0, "Royal Decree", ROYAL_DECREE);
        let mountain = scenario
            .add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.")
            .with_subtypes(vec!["Mountain"])
            .id();
        let take = free_spell(
            &mut scenario,
            P1,
            "Take Land",
            "Gain control of target land.",
        );
        let boomerang = free_spell(&mut scenario, P0, "Boomerang", BOOMERANG);
        let mut runner = scenario.build();
        stage_turn(&mut runner, P1);
        runner.cast(take).target_object(mountain).resolve();
        assert_eq!(
            runner.state().objects[&mountain].controller,
            P1,
            "reach guard: P1 controls P0's Mountain"
        );
        stage_turn(&mut runner, P1);
        runner
            .act(GameAction::ActivateAbility {
                source_id: mountain,
                ability_index: 0,
            })
            .expect("tap the Mountain for mana");
        drain_ordering(&mut runner);
        assert!(
            runner.state().objects[&mountain].tapped,
            "reach guard: tapped"
        );
        assert_eq!(
            runner.state().stack.len(),
            1,
            "reach guard: one Decree trigger"
        );
        if bounce {
            priority_to(&mut runner, P0);
            runner.cast(boomerang).target_object(mountain).commit();
            resolve_one_declining(&mut runner);
            assert_eq!(
                runner.state().objects[&mountain].zone,
                Zone::Hand,
                "reach guard: back in P0's hand"
            );
        }
        let before = (life(&runner, P0), life(&runner, P1));
        resolve_one_declining(&mut runner);
        assert_eq!(
            (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
            (0, -1),
            "the tapper (P1) takes 1, bounced={bounce}"
        );
    }
}

/// Elder Deep-Fiend's cast trigger (P1's ability) targets P0's Lava Runner;
/// P0 Aethersnatches the Deep-Fiend spell in response. CR 113.8 + CR 113.7a:
/// the targeter is the triggered ability, still P1's, so P1 sacrifices.
#[test]
fn lava_runner_punishes_the_cast_trigger_controller_not_the_new_spell_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runner_id = scenario
        .add_creature_from_oracle(P0, "Lava Runner", 2, 2, LAVA_RUNNER)
        .id();
    scenario.add_land_from_oracle(P0, "P0 Land", "{T}: Add {R}.");
    scenario.add_land_from_oracle(P1, "P1 Land", "{T}: Add {U}.");
    let fiend = scenario
        .add_creature_to_hand_from_oracle(P1, "Elder Deep-Fiend", 5, 6, ELDER_DEEP_FIEND)
        .with_mana_cost(ManaCost::zero())
        .id();
    let snatch = free_spell(&mut scenario, P0, "Aethersnatch", AETHERSNATCH);
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    let card_id = runner.state().objects[&fiend].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fiend,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Elder Deep-Fiend");
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(runner_id)],
                    })
                    .expect("target Lava Runner");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        3,
        "reach guard: Deep-Fiend, its cast trigger, Lava Runner's trigger"
    );
    priority_to(&mut runner, P0);
    runner.cast(snatch).target_object(fiend).commit();
    resolve_one_declining(&mut runner);
    assert_eq!(
        runner.state().objects[&fiend].controller,
        P0,
        "reach guard: Aethersnatch took the Deep-Fiend spell"
    );
    let before = (lands(&runner, P0), lands(&runner, P1));
    resolve_one_declining(&mut runner);
    assert_eq!(
        (lands(&runner, P0), lands(&runner, P1)),
        (before.0, before.1 - 1),
        "P1, the trigger's controller, sacrifices"
    );
}

#[derive(Clone, Copy, Debug)]
enum Theft {
    Stolen,
    Own,
}

#[derive(Clone, Copy, Debug)]
enum SourceFate {
    Bounced,
    Stays,
}

/// PG7: P1's Prodigal Pyromancer activates at P0's Lava Runner; in response P0
/// optionally steals and/or bounces the Pyromancer. Returns which player lost
/// a land when Runner's trigger resolved: (P0 lost, P1 lost).
fn pyromancer_runner_board(theft: Theft, fate: SourceFate) -> (usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runner_id = scenario
        .add_creature_from_oracle(P0, "Lava Runner", 2, 2, LAVA_RUNNER)
        .id();
    scenario.add_land_from_oracle(P0, "P0 Land", "{T}: Add {R}.");
    scenario.add_land_from_oracle(P1, "P1 Land", "{T}: Add {R}.");
    let pyromancer = scenario
        .add_creature_from_oracle(P1, "Prodigal Pyromancer", 1, 1, PRODIGAL_PYROMANCER)
        .id();
    let steal = free_spell(&mut scenario, P0, "Steal Creature", STEAL_CREATURE);
    let bounce = free_spell(&mut scenario, P0, "Bounce Creature", BOUNCE_CREATURE);
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: pyromancer,
            ability_index: 0,
        })
        .expect("activate");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(runner_id)],
            })
            .expect("target Lava Runner");
    }
    drain_ordering(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: Pyromancer's ability and Runner's trigger"
    );
    // Responses go above Runner's trigger and resolve first.
    if let Theft::Stolen = theft {
        priority_to(&mut runner, P0);
        runner.cast(steal).target_object(pyromancer).commit();
        resolve_one_declining(&mut runner);
        assert_eq!(runner.state().objects[&pyromancer].controller, P0);
    }
    if let SourceFate::Bounced = fate {
        priority_to(&mut runner, P0);
        runner.cast(bounce).target_object(pyromancer).commit();
        resolve_one_declining(&mut runner);
        assert_eq!(runner.state().objects[&pyromancer].zone, Zone::Hand);
    }
    let before = (lands(&runner, P0), lands(&runner, P1));
    resolve_one_declining(&mut runner);
    (before.0 - lands(&runner, P0), before.1 - lands(&runner, P1))
}

/// CR 113.8: an activated ability's controller is the player who activated it
/// (P1) whatever then happens to its source. 4a normal, 4b theft (live source),
/// 4c bounce, 4d theft and bounce.
#[test]
fn lava_runner_punishes_the_activator_whatever_happens_to_the_source() {
    for (theft, fate, label) in [
        (Theft::Own, SourceFate::Stays, "4a"),
        (Theft::Stolen, SourceFate::Stays, "4b"),
        (Theft::Own, SourceFate::Bounced, "4c"),
        (Theft::Stolen, SourceFate::Bounced, "4d"),
    ] {
        assert_eq!(
            pyromancer_runner_board(theft, fate),
            (0, 1),
            "{label}: P1, the activator, sacrifices"
        );
    }
}

/// 4e: a triggered targeter. P1's labelled synthetic creature's enters
/// trigger targets Lava Runner; P0 steals the creature in response. CR 113.8:
/// the trigger stays P1's.
#[test]
fn lava_runner_punishes_the_trigger_controller_when_the_source_is_stolen() {
    for theft in [Theft::Stolen, Theft::Own] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let runner_id = scenario
            .add_creature_from_oracle(P0, "Lava Runner", 2, 2, LAVA_RUNNER)
            .id();
        scenario.add_land_from_oracle(P0, "P0 Land", "{T}: Add {R}.");
        scenario.add_land_from_oracle(P1, "P1 Land", "{T}: Add {R}.");
        let pinger = scenario
            .add_creature_to_hand_from_oracle(
                P1,
                "Pinger",
                1,
                1,
                "When this creature enters, it deals 1 damage to any target.",
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let steal = free_spell(&mut scenario, P0, "Steal Creature", STEAL_CREATURE);
        let mut runner = scenario.build();
        stage_turn(&mut runner, P1);
        runner.cast(pinger).commit();
        for _ in 0..12 {
            if runner.state().objects[&pinger].zone == Zone::Battlefield
                && runner.state().stack.len() >= 2
            {
                break;
            }
            match runner.state().waiting_for.clone() {
                WaitingFor::TriggerTargetSelection { .. } => {
                    runner
                        .act(GameAction::SelectTargets {
                            targets: vec![TargetRef::Object(runner_id)],
                        })
                        .expect("target Lava Runner");
                }
                WaitingFor::OrderTriggers { .. } => {
                    drain_order_triggers_with_identity(runner.state_mut());
                }
                _ => {
                    runner.act(GameAction::PassPriority).expect("pass");
                }
            }
        }
        assert_eq!(
            runner.state().stack.len(),
            2,
            "reach guard: the enters trigger and Runner's trigger"
        );
        if let Theft::Stolen = theft {
            priority_to(&mut runner, P0);
            runner.cast(steal).target_object(pinger).commit();
            resolve_one_declining(&mut runner);
            assert_eq!(runner.state().objects[&pinger].controller, P0);
        }
        let before = (lands(&runner, P0), lands(&runner, P1));
        resolve_one_declining(&mut runner);
        assert_eq!(
            (before.0 - lands(&runner, P0), before.1 - lands(&runner, P1)),
            (0, 1),
            "{theft:?}: P1, the trigger's controller, sacrifices"
        );
    }
}

/// 4f: a printed spell targeter. P1 casts Ent's Fury with both targets legal
/// (its own creature and P0's Lava Runner): P1 sacrifices.
#[test]
fn lava_runner_punishes_the_caster_of_ents_fury() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runner_id = scenario
        .add_creature_from_oracle(P0, "Lava Runner", 2, 2, LAVA_RUNNER)
        .id();
    scenario.add_land_from_oracle(P0, "P0 Land", "{T}: Add {R}.");
    scenario.add_land_from_oracle(P1, "P1 Land", "{T}: Add {G}.");
    let ent = scenario.add_creature(P1, "Treefolk", 5, 5).id();
    let fury = scenario
        .add_spell_to_hand_from_oracle(P1, "Ent's Fury", false, ENTS_FURY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    runner.cast(fury).target_objects(&[ent, runner_id]).commit();
    drain_ordering(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: Ent's Fury and Runner's trigger"
    );
    let before = (lands(&runner, P0), lands(&runner, P1));
    resolve_one_declining(&mut runner);
    assert_eq!(
        (before.0 - lands(&runner, P0), before.1 - lands(&runner, P1)),
        (0, 1)
    );
}

const REALITY_SMASHER: &str = "({C} represents colorless mana.)\nTrample, haste\nWhenever this creature becomes the target of a spell an opponent controls, counter that spell unless its controller discards a card.";
const PERPLEXING_CHIMERA: &str = "Whenever an opponent casts a spell, you may exchange control of this creature and that spell. If you do, you may choose new targets for the spell. (If the spell becomes a permanent, you control that permanent.)";
const STRIONIC_RESONATOR: &str = "{2}, {T}: Copy target triggered ability you control. You may choose new targets for the copy. (A triggered ability uses the words \"when,\" \"whenever,\" or \"at.\")";

/// Bonecrusher Giant (`TriggeringSpellController`, the other spelling of the
/// same referent): P1's spell targets it and P0 Commandeers the spell before
/// the trigger resolves. CR 109.4: "that spell's controller" is P0 now.
#[test]
fn bonecrusher_giant_damages_the_targeting_spells_current_controller() {
    const BONECRUSHER: &str = "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.";
    for commandeer in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = scenario
            .add_creature_from_oracle(P0, "Bonecrusher Giant", 4, 3, BONECRUSHER)
            .id();
        let ping = free_spell(
            &mut scenario,
            P1,
            "Ping",
            "Ping deals 1 damage to target creature.",
        );
        let commandeer_id = free_spell(&mut scenario, P0, "Commandeer", COMMANDEER);
        let mut runner = scenario.build();
        stage_turn(&mut runner, P1);
        runner.cast(ping).target_object(giant).commit();
        drain_ordering(&mut runner);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "reach guard: Ping and the Giant's trigger"
        );
        if commandeer {
            priority_to(&mut runner, P0);
            runner.cast(commandeer_id).target_object(ping).commit();
            resolve_one_declining(&mut runner);
            assert_eq!(runner.state().objects[&ping].controller, P0);
        }
        let before = (life(&runner, P0), life(&runner, P1));
        resolve_one_declining(&mut runner);
        let expected = if commandeer { (-2, 0) } else { (0, -2) };
        assert_eq!(
            (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
            expected,
            "commandeer={commandeer}"
        );
    }
}

/// Reality Smasher's payer: P1's spell targets P0's Smasher; P0 optionally
/// Commandeers the spell first. CR 109.4: "its controller" is the spell's
/// current controller, who is asked to discard.
#[test]
fn reality_smasher_asks_the_targeting_spells_current_controller() {
    for commandeer in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let smasher = scenario
            .add_creature_from_oracle(P0, "Reality Smasher", 5, 5, REALITY_SMASHER)
            .id();
        let ping = free_spell(
            &mut scenario,
            P1,
            "Ping",
            "Ping deals 1 damage to target creature.",
        );
        let commandeer_id = free_spell(&mut scenario, P0, "Commandeer", COMMANDEER);
        scenario.add_card_to_hand(P0, "P0 Discard Fodder");
        scenario.add_card_to_hand(P1, "P1 Discard Fodder");
        let mut runner = scenario.build();
        stage_turn(&mut runner, P1);
        runner.cast(ping).target_object(smasher).commit();
        drain_ordering(&mut runner);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "reach guard: Ping and Smasher's trigger"
        );
        if commandeer {
            priority_to(&mut runner, P0);
            runner.cast(commandeer_id).target_object(ping).commit();
            resolve_one_declining(&mut runner);
            assert_eq!(runner.state().objects[&ping].controller, P0);
        }
        for _ in 0..6 {
            match runner.state().waiting_for.clone() {
                WaitingFor::UnlessPayment { .. } => break,
                WaitingFor::OrderTriggers { .. } => {
                    drain_order_triggers_with_identity(runner.state_mut());
                }
                _ => {
                    runner.act(GameAction::PassPriority).expect("pass");
                }
            }
        }
        let expected = if commandeer { P0 } else { P1 };
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::UnlessPayment { player, .. } if player == expected
            ),
            "commandeer={commandeer}: {expected:?} is asked to discard, got {:?}",
            runner.state().waiting_for
        );
    }
}

/// The printed Chimera discriminator: P1 casts Elder Deep-Fiend, whose cast
/// trigger targets P0's Lava Runner; P0's Perplexing Chimera also triggers. P0
/// copies Chimera's trigger with Strionic Resonator, and the copy exchanges
/// control of Chimera and the Deep-Fiend spell before Runner's trigger
/// resolves. CR 113.8: the targeter is P1's cast trigger, so P1 sacrifices.
#[test]
fn lava_runner_after_a_copied_chimera_exchange_punishes_the_trigger_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runner_id = scenario
        .add_creature_from_oracle(P0, "Lava Runner", 2, 2, LAVA_RUNNER)
        .id();
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA)
        .as_enchantment()
        .id();
    let resonator = scenario
        .add_artifact_from_oracle(P0, "Strionic Resonator", STRIONIC_RESONATOR)
        .id();
    scenario.add_land_from_oracle(P0, "P0 Land", "{T}: Add {R}.");
    scenario.add_land_from_oracle(P1, "P1 Land", "{T}: Add {U}.");
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let fiend = scenario
        .add_creature_to_hand_from_oracle(P1, "Elder Deep-Fiend", 5, 6, ELDER_DEEP_FIEND)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    let card_id = runner.state().objects[&fiend].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fiend,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Elder Deep-Fiend");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(runner_id)],
                    })
                    .expect("target Lava Runner");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => break,
        }
    }
    let chimera_trigger = runner
        .state()
        .stack
        .iter()
        .find(|entry| {
            entry.source_id == chimera
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|entry| entry.id)
        .expect("reach guard: Chimera's trigger is on the stack");
    let top_is_runner = runner
        .state()
        .stack
        .back()
        .is_some_and(|entry| entry.source_id == runner_id);
    assert!(top_is_runner, "reach guard: Runner's trigger is on top");
    assert_eq!(runner.state().stack.len(), 4, "reach guard: four objects");

    // P0 copies Chimera's trigger; the copy resolves above Runner's trigger.
    priority_to(&mut runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: resonator,
            ability_index: 0,
        })
        .expect("activate Strionic Resonator");
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(chimera_trigger)],
                    })
                    .expect("target Chimera's trigger");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay");
            }
            _ => break,
        }
    }
    // Resolve the Resonator ability, then the copy (accept the exchange,
    // decline new targets).
    let depth_before = runner.state().stack.len();
    assert_eq!(depth_before, 5, "reach guard: Resonator's ability on top");
    let mut accepted = false;
    for _ in 0..24 {
        if runner.state().objects[&fiend].controller == P0
            && runner.state().stack.len() == 4
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: !accepted })
                    .expect("optional");
                accepted = true;
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }
    assert_eq!(
        runner.state().objects[&fiend].controller,
        P0,
        "reach guard: the copied exchange gave P0 the Deep-Fiend spell"
    );
    assert!(
        runner
            .state()
            .stack
            .back()
            .is_some_and(|entry| entry.source_id == runner_id),
        "reach guard: Runner's trigger is next"
    );
    let before = (lands(&runner, P0), lands(&runner, P1));
    resolve_one_declining(&mut runner);
    assert_eq!(
        (before.0 - lands(&runner, P0), before.1 - lands(&runner, P1)),
        (0, 1),
        "P1, the cast trigger's controller, sacrifices"
    );
}

const FRACTURED_LOYALTY: &str = "Enchant creature\nWhenever enchanted creature becomes the target of a spell or ability, that spell or ability's controller gains control of that creature. (This effect lasts indefinitely.)";

/// Fractured Loyalty's recipient: P1's Pyromancer ability targets P0's
/// enchanted creature; P0 optionally steals the Pyromancer in response.
/// CR 113.8: "that spell or ability's controller" is the activator, P1, who
/// gains control of the creature either way.
#[test]
fn fractured_loyalty_gives_the_creature_to_the_ability_controller() {
    for theft in [Theft::Own, Theft::Stolen] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
        let loyalty = scenario
            .add_enchantment_from_oracle(P0, "Fractured Loyalty", "")
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(&["Enchant"], FRACTURED_LOYALTY)
            .id();
        let pyromancer = scenario
            .add_creature_from_oracle(P1, "Prodigal Pyromancer", 1, 1, PRODIGAL_PYROMANCER)
            .id();
        let steal = free_spell(&mut scenario, P0, "Steal Creature", STEAL_CREATURE);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.objects.get_mut(&loyalty).unwrap().attached_to = Some(bear.into());
            state
                .objects
                .get_mut(&bear)
                .unwrap()
                .attachments
                .push(loyalty);
            state.layers_dirty.mark_full();
        }
        stage_turn(&mut runner, P1);
        runner
            .act(GameAction::ActivateAbility {
                source_id: pyromancer,
                ability_index: 0,
            })
            .expect("activate");
        if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(bear)],
                })
                .expect("target the Bear");
        }
        drain_ordering(&mut runner);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "reach guard: the ability and Loyalty's trigger"
        );
        if let Theft::Stolen = theft {
            priority_to(&mut runner, P0);
            runner.cast(steal).target_object(pyromancer).commit();
            resolve_one_declining(&mut runner);
            assert_eq!(runner.state().objects[&pyromancer].controller, P0);
        }
        resolve_one_declining(&mut runner);
        assert_eq!(
            runner.state().objects[&bear].controller,
            P1,
            "{theft:?}: the activator (P1) gains control"
        );
    }
}

/// CR 608.2c: a becomes-target trigger that chooses a fresh object target
/// ("tap target creature. Its controller draws a card.") reads that choice's
/// controller, not the targeter's. P1's Pyromancer targets P0's sentinel;
/// P0's trigger taps P0's own Victim, so P0 draws. Synthetic sentinel text.
#[test]
fn a_fresh_target_choice_names_its_own_controller_not_the_targeter() {
    const SENTINEL: &str = "Whenever this creature becomes the target of a spell or ability, tap target creature. Its controller draws a card.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Card A", "P0 Card B"]);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B"]);
    let sentinel = scenario
        .add_creature_from_oracle(P0, "Synthetic Sentinel", 3, 3, SENTINEL)
        .id();
    let victim = scenario.add_creature(P0, "Victim", 2, 2).id();
    let pyromancer = scenario
        .add_creature_from_oracle(P1, "Prodigal Pyromancer", 1, 1, PRODIGAL_PYROMANCER)
        .id();
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: pyromancer,
            ability_index: 0,
        })
        .expect("activate");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(sentinel)],
            })
            .expect("target the sentinel");
    }
    for _ in 0..4 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(victim)],
                    })
                    .expect("P0 taps its own Victim");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: ability + trigger"
    );
    let hand = |r: &GameRunner, p: PlayerId| r.state().players[p.0 as usize].hand.len();
    let (p0, p1) = (hand(&runner, P0), hand(&runner, P1));
    resolve_one_declining(&mut runner);
    assert!(
        runner.state().objects[&victim].tapped,
        "reach guard: Victim tapped"
    );
    assert_eq!(hand(&runner, P0), p0 + 1, "the Victim's controller draws");
    assert_eq!(hand(&runner, P1), p1, "not the targeter");
}

const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";

/// Maintainer round 3, finding 2. P1 controls P0's red Prodigal Pyromancer and
/// taps it; Royal Decree triggers; P1 Ephemerates the Pyromancer in response,
/// which returns it under its owner P0 as a new object (CR 400.7). CR 608.2h:
/// "that permanent's controller" is the tapped permanent's last controller,
/// P1, not the returned permanent's controller P0. Control: no blink, P1.
#[test]
fn royal_decree_damages_the_tapped_incarnations_controller_after_a_blink() {
    for blink in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(P0, "Royal Decree", ROYAL_DECREE);
        let pyromancer = scenario
            .add_creature_from_oracle(P0, "Prodigal Pyromancer", 1, 1, PRODIGAL_PYROMANCER)
            .with_color(vec![ManaColor::Red])
            .id();
        let steal = free_spell(&mut scenario, P1, "Steal Creature", STEAL_CREATURE);
        let ephemerate = free_spell(&mut scenario, P1, "Ephemerate", EPHEMERATE);
        let mut runner = scenario.build();
        stage_turn(&mut runner, P1);
        runner.cast(steal).target_object(pyromancer).resolve();
        assert_eq!(
            runner.state().objects[&pyromancer].controller,
            P1,
            "reach guard: P1 controls P0's Pyromancer"
        );
        // It came under P1's control this turn; let it be tapped for a cost.
        runner
            .state_mut()
            .objects
            .get_mut(&pyromancer)
            .unwrap()
            .summoning_sick = false;
        stage_turn(&mut runner, P1);
        runner
            .act(GameAction::ActivateAbility {
                source_id: pyromancer,
                ability_index: 0,
            })
            .expect("tap the Pyromancer");
        if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P0)],
                })
                .expect("target P0");
        }
        drain_ordering(&mut runner);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "reach guard: the Pyromancer ability and Decree's trigger"
        );
        if blink {
            priority_to(&mut runner, P1);
            runner.cast(ephemerate).target_object(pyromancer).commit();
            resolve_one_declining(&mut runner);
            let returned = &runner.state().objects[&pyromancer];
            assert_eq!(returned.zone, Zone::Battlefield, "reach guard: returned");
            assert_eq!(returned.controller, P0, "reach guard: under its owner");
            assert!(!returned.tapped, "reach guard: a new, untapped object");
        }
        let before = (life(&runner, P0), life(&runner, P1));
        resolve_one_declining(&mut runner);
        assert_eq!(
            (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
            (0, -1),
            "P1, the tapped permanent's controller, takes 1 (blink={blink})"
        );
    }
}

/// Maintainer round 3, finding 3. Elder Deep-Fiend's cast trigger is an
/// ABILITY, even though its source is a spell on the stack. CR 115.1 + CR
/// 113.8: Forsaken Wastes ("becomes the target of a spell") does not trigger
/// when that ability targets it, so P1 loses no life. Lava Runner's "spell or
/// ability" row above keeps the positive for abilities;
/// `forsaken_wastes_hits_the_caster_without_commandeer` keeps the spell
/// positive.
#[test]
fn forsaken_wastes_ignores_a_cast_trigger_targeting_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wastes = scenario
        .add_enchantment_from_oracle(P0, "Forsaken Wastes", FORSAKEN_WASTES)
        .id();
    let fiend = scenario
        .add_creature_to_hand_from_oracle(P1, "Elder Deep-Fiend", 5, 6, ELDER_DEEP_FIEND)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    stage_turn(&mut runner, P1);
    let card_id = runner.state().objects[&fiend].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fiend,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Elder Deep-Fiend");
    let mut targeted = false;
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                targeted = true;
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(wastes)],
                    })
                    .expect("target Forsaken Wastes");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => break,
        }
    }
    assert!(
        targeted,
        "reach guard: the cast trigger targeted the Wastes"
    );
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Deep-Fiend and its cast trigger only; no Wastes trigger"
    );
    let p1 = life(&runner, P1);
    while !runner.state().stack.is_empty() {
        resolve_one_declining(&mut runner);
    }
    assert!(
        runner.state().objects[&wastes].tapped,
        "reach guard: the cast trigger resolved and tapped the Wastes"
    );
    assert_eq!(life(&runner, P1), p1, "P1 loses no life");
}

/// Maintainer round 3, finding 7. CR 301.5c + CR 301.5f + CR 704.5n: when an
/// equipped creature is blinked, the Equipment becomes unattached and stays on
/// the battlefield; "equipped creature" is whatever it is attached to now,
/// which is nothing, so its "equipped creature gets +2/+2" ability does nothing
/// to the returned creature (a new object, CR 400.7). Control: no blink, the
/// creature gets +2/+2. Synthetic Equipment text.
#[test]
fn an_unattached_equipment_does_not_pump_its_former_host() {
    for blink in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
        let blade = scenario
            .add_artifact_from_oracle(P0, "Synthetic Rally Blade", RALLY_BLADE)
            .with_subtypes(vec!["Equipment"])
            .id();
        let ephemerate = free_spell(&mut scenario, P0, "Ephemerate", EPHEMERATE);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.objects.get_mut(&blade).unwrap().attached_to = Some(bear.into());
            state
                .objects
                .get_mut(&bear)
                .unwrap()
                .attachments
                .push(blade);
            state.layers_dirty.mark_full();
        }
        // The accepted typed shape the finding names: a Pump whose target is
        // `TargetFilter::AttachedTo` (no Oracle wording lowers to it for an
        // Equipment; the parsed "{0}" ability's effect is replaced with it).
        inject_attached_to_pump(&mut runner, blade);
        if blink {
            runner.cast(ephemerate).target_object(bear).resolve();
            let blade_now = &runner.state().objects[&blade];
            assert_eq!(blade_now.zone, Zone::Battlefield, "the Equipment stays");
            assert_eq!(blade_now.attached_to, None, "and is unattached");
            assert_eq!(
                runner.state().objects[&bear].zone,
                Zone::Battlefield,
                "reach guard: the Bear returned"
            );
        }
        runner
            .act(GameAction::ActivateAbility {
                source_id: blade,
                ability_index: 0,
            })
            .expect("activate the Equipment's ability");
        while !runner.state().stack.is_empty() {
            resolve_one_declining(&mut runner);
        }
        runner.state_mut().layers_dirty.mark_full();
        engine::game::layers::evaluate_layers(runner.state_mut());
        let bear_now = &runner.state().objects[&bear];
        let expected = if blink { (2, 2) } else { (4, 4) };
        assert_eq!(
            (bear_now.power.unwrap_or(0), bear_now.toughness.unwrap_or(0)),
            expected,
            "blink={blink}"
        );
    }
}

const RALLY_BLADE: &str = "{0}: Equipped creature gets +2/+2 until end of turn.\nEquip {0}";
const FLICKER: &str =
    "Exile target nontoken permanent, then return it to the battlefield under its owner's control.";

/// Replace the Equipment's first ability's effect with the accepted typed
/// `Pump { target: AttachedTo }`, in both the printed base and the derived
/// list so layer evaluation keeps it.
fn inject_attached_to_pump(runner: &mut GameRunner, blade: ObjectId) {
    let pump = Effect::Pump {
        power: PtValue::Fixed(2),
        toughness: PtValue::Fixed(2),
        target: TargetFilter::AttachedTo,
    };
    let blade_obj = runner.state_mut().objects.get_mut(&blade).unwrap();
    *std::sync::Arc::make_mut(&mut blade_obj.base_abilities)[0].effect = pump.clone();
    *std::sync::Arc::make_mut(&mut blade_obj.abilities)[0].effect = pump;
}

/// Pre-push review R1. CR 400.7 + CR 301.5c + CR 301.5f: Flicker on an attached
/// Equipment returns it as a new object, unattached; "equipped creature" is
/// whatever it is attached to now — nothing — so its own departure record
/// (which kept the Bear) must not answer, and the Bear stays 2/2. Control: no
/// blink, 4/4. Synthetic Equipment text; Flicker is verbatim.
#[test]
fn a_blinked_equipment_does_not_pump_its_former_host() {
    for blink in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
        let blade = scenario
            .add_artifact_from_oracle(P0, "Synthetic Rally Blade", RALLY_BLADE)
            .with_subtypes(vec!["Equipment"])
            .id();
        let flicker = scenario
            .add_spell_to_hand_from_oracle(P0, "Flicker", false, FLICKER)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.objects.get_mut(&blade).unwrap().attached_to = Some(bear.into());
            state
                .objects
                .get_mut(&bear)
                .unwrap()
                .attachments
                .push(blade);
            state.layers_dirty.mark_full();
        }
        let incarnation_before = runner.state().objects[&blade].incarnation;
        if blink {
            runner.cast(flicker).target_object(blade).resolve();
            let blade_now = &runner.state().objects[&blade];
            assert_eq!(
                blade_now.zone,
                Zone::Battlefield,
                "reach guard: it returned"
            );
            assert_ne!(
                blade_now.incarnation, incarnation_before,
                "reach guard: a new object"
            );
            assert_eq!(blade_now.attached_to, None, "it returns unattached");
            assert!(
                runner.state().zone_changes_this_turn.iter().any(|r| {
                    r.object_id == blade
                        && r.from_zone == Some(Zone::Battlefield)
                        && r.attached_to == Some(bear.into())
                }),
                "reach guard: its departure record kept the former attachment"
            );
        }
        inject_attached_to_pump(&mut runner, blade);
        runner
            .act(GameAction::ActivateAbility {
                source_id: blade,
                ability_index: 0,
            })
            .expect("activate the Equipment's ability");
        while !runner.state().stack.is_empty() {
            resolve_one_declining(&mut runner);
        }
        runner.state_mut().layers_dirty.mark_full();
        engine::game::layers::evaluate_layers(runner.state_mut());
        let bear_now = &runner.state().objects[&bear];
        let expected = if blink { (2, 2) } else { (4, 4) };
        assert_eq!(
            (bear_now.power.unwrap_or(0), bear_now.toughness.unwrap_or(0)),
            expected,
            "blink={blink}"
        );
    }
}
