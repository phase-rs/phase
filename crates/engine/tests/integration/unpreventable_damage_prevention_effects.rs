//! Prevention effects that have additional effects. The module holds the Outfitted
//! Jouster population rows, then the printed "while" guards on prevention lines and the one
//! reading a guard gives a simultaneous damage batch. Their admission under damage that can't be
//! prevented (CR 615.12) is tracked as Command Zone #172.
//!
//! Outfitted Jouster: "If damage would be dealt to Outfitted Jouster while it's equipped, prevent
//! that damage. When damage is prevented this way, sacrifice an Equipment attached to Outfitted
//! Jouster." The rider names a population (CR 301.5a, CR 701.21a), so the player picks from what
//! is attached (CR 608.2d), and the Jouster itself is never the sacrifice.
//!
//! A printed "while <state>" gates the shield itself (CR 604.2, CR 611.3a). Undergrowth Champion:
//! "If damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that
//! damage and remove a +1/+1 counter from this creature." Its ruling: "If Undergrowth Champion is
//! dealt damage from multiple sources at the same time (for example, if it's blocked by multiple
//! creatures), its prevention effect will apply once to all that damage." So the guard is read once
//! when a simultaneous batch opens (CR 510.2, CR 608.2f), and every portion of the batch meets that
//! reading. The reading survives an order prompt inside combat damage, and a save and load at it.
//!
//! A rider's pause inside a combat damage batch is settled before the batch moves on, at the
//! per-portion drain and at the aggregate drain after it (CR 510.2). Combat damage can't pause for
//! a player's answer yet (Command Zone #140), so the engine answers in the player's place: an
//! order the rider's event meets takes its first candidate (CR 616.1, CR 616.1f), a "may" on it is
//! declined, and the rider's own choice is cleared. None of these is the player's answer, so those
//! rows are strict failures, not readings of the cards. A card whose own replacement asks such a
//! choice reports unsupported (`ResolverFeature:structural:damage_replacement_choice_unasked_in_combat`).
//! The last section holds those rows; the Ironscale Hydra rows are in
//! `ironscale_hydra_combat_prevention`.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DamageGuardReading, Effect, EffectKind, TargetFilter,
    TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, LossReason};
use engine::types::game_state::{
    CastPaymentMode, CombatDamageAssignmentMode, GameState, ReplacementActionScope, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::ResolutionFrame;
use engine::types::zones::Zone;

use super::rules::run_combat;

const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const JOUSTER: &str = "When Outfitted Jouster enters, conjure a card named Steelclaw Lance and a card named Brawler's Plate onto the battlefield and attach them to Outfitted Jouster.\nIf damage would be dealt to Outfitted Jouster while it's equipped, prevent that damage. When damage is prevented this way, sacrifice an Equipment attached to Outfitted Jouster.";
const STEELCLAW_LANCE: &str = "Equipped creature gets +2/+2.\nEquip Knight {1}\nEquip {3}";
const BRAWLERS_PLATE: &str = "Equipped creature gets +2/+2 and has trample. (It can deal excess combat damage to the player or planeswalker it's attacking.)\nEquip {4} ({4}: Attach to target creature you control. Equip only as a sorcery.)";
const BONESPLITTER: &str = "Equipped creature gets +2/+0.\nEquip {1}";

fn equipment(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    let mut builder = scenario.add_artifact_from_oracle(P1, name, text);
    builder.with_subtypes(vec!["Equipment"]);
    builder.id()
}

fn jouster_scenario() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jouster = scenario
        .add_creature_from_oracle(P1, "Outfitted Jouster", 2, 2, JOUSTER)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    (scenario, jouster, bolt)
}

/// Drive to the next prompt, or to `Priority` with an empty stack.
fn settle(runner: &mut GameRunner, target: &mut Option<TargetRef>, events: &mut Vec<GameEvent>) {
    for _ in 0..40 {
        let action = match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: target.take(),
            },
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            _ => return,
        };
        events.extend(runner.act(action).expect("settle action").events);
    }
    panic!("the game did not settle");
}

pub(super) fn bolt_at(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Lightning Bolt")
        .events;
    settle(runner, &mut Some(TargetRef::Object(target)), &mut events);
    events
}

/// Two Equipment attached and one not: the prompt offers exactly the attached two, the Jouster is
/// not sacrificed, and the unattached Bonesplitter is out of the population.
#[test]
fn outfitted_jouster_sacrifices_the_attached_equipment_its_controller_picks() {
    let (mut scenario, jouster, bolt) = jouster_scenario();
    let lance = equipment(&mut scenario, "Steelclaw Lance", STEELCLAW_LANCE);
    let plate = equipment(&mut scenario, "Brawler's Plate", BRAWLERS_PLATE);
    let bonesplitter = equipment(&mut scenario, "Bonesplitter", BONESPLITTER);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), lance, jouster);
    attach_to(runner.state_mut(), plate, jouster);

    let mut events = bolt_at(&mut runner, bolt, jouster);

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            count,
            cards,
            effect_kind,
            ..
        } => {
            assert_eq!(*effect_kind, EffectKind::Sacrifice);
            assert_eq!(*count, 1);
            let mut offered = cards.clone();
            offered.sort();
            let mut attached = vec![lance, plate];
            attached.sort();
            assert_eq!(offered, attached);
        }
        other => panic!("expected EffectZoneChoice, got {other:?}"),
    }

    events.extend(
        runner
            .act(GameAction::SelectCards { cards: vec![plate] })
            .expect("choose Brawler's Plate")
            .events,
    );
    settle(&mut runner, &mut None, &mut events);

    assert_eq!(runner.state().objects[&plate].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&lance].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&bonesplitter].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().objects[&jouster].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&jouster].damage_marked, 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}

/// One Equipment attached: the attached relation resolves the count with no prompt, whatever
/// else its controller has on the battlefield.
#[test]
fn outfitted_jouster_needs_no_prompt_for_a_single_attached_equipment() {
    let (mut scenario, jouster, bolt) = jouster_scenario();
    let lance = equipment(&mut scenario, "Steelclaw Lance", STEELCLAW_LANCE);
    let bonesplitter = equipment(&mut scenario, "Bonesplitter", BONESPLITTER);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), lance, jouster);

    bolt_at(&mut runner, bolt, jouster);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&lance].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&bonesplitter].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().objects[&jouster].zone, Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// Printed "while" guards and the one reading of a simultaneous batch
// ---------------------------------------------------------------------------

const UNDERGROWTH_CHAMPION: &str = "If damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that damage and remove a +1/+1 counter from this creature.\nLandfall — Whenever a land you control enters, put a +1/+1 counter on this creature.";
const OATHSWORN_KNIGHT: &str = "This creature enters with four +1/+1 counters on it.\nThis creature attacks each combat if able.\nIf damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that damage and remove a +1/+1 counter from it.";
const POLUKRANOS_UNCHAINED: &str = "Polukranos enters with six +1/+1 counters on it. It escapes with twelve +1/+1 counters on it instead.\nIf damage would be dealt to Polukranos while it has a +1/+1 counter on it, prevent that damage and remove that many +1/+1 counters from it.\n{1}{B}{G}: Polukranos fights another target creature.\nEscape—{4}{B}{G}, Exile six other cards from your graveyard.";
const UGINS_CONJURANT: &str = "This creature enters with X +1/+1 counters on it.\nIf damage would be dealt to this creature while it has a +1/+1 counter on it, prevent that damage and remove that many +1/+1 counters from this creature.";
const JARED_CARTHALION: &str = "When Jared Carthalion enters, target opponent becomes the monarch. You can't become the monarch this turn.\nIf damage would be dealt to Jared Carthalion while you're the monarch, prevent that damage and put that many +1/+1 counters on it.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const BAND_TOGETHER: &str = "Up to two target creatures you control each deal damage equal to their power to another target creature.";
const MOONLIGHT_HUNT: &str = "Choose target creature you don't control. Each creature you control that's a Wolf or a Werewolf deals damage equal to its power to that creature.";
const COORDINATED_CLOBBERING: &str = "Tap one or two target untapped creatures you control. They each deal damage equal to their power to target creature an opponent controls.";
const FURNACE_OF_RATH: &str = "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";
const VIGOR: &str = "Trample\nIf damage would be dealt to another creature you control, prevent that damage. Put a +1/+1 counter on that creature for each 1 damage prevented this way.\nWhen Vigor is put into a graveyard from anywhere, shuffle it into its owner's library.";
const PANTHER_HABIT: &str = "If equipped creature would be dealt damage, prevent that damage and put that many +1/+1 counters on it.\nEquip {2}";
const ACOLYTES_REWARD: &str = "Prevent the next X damage that would be dealt to target creature this turn, where X is your devotion to white. If damage is prevented this way, Acolyte's Reward deals that much damage to any target. (Each {W} in the mana costs of permanents you control counts toward your devotion to white.)";
const PARIAH: &str = "Enchant creature\nAll damage that would be dealt to you is dealt to enchanted creature instead.";
const STEAM_BLAST: &str = "Steam Blast deals 2 damage to each creature and each player.";
const MISTHIOS_FURY: &str = "Misthios's Fury deals 3 damage to target creature. If you control an Equipment, Misthios's Fury also deals 2 damage to that creature's controller.";
const CHANDRAS_OUTRAGE: &str = "Chandra's Outrage deals 4 damage to target creature and 2 damage to that creature's controller.";
const HAIL_STORM: &str = "Hail Storm deals 2 damage to each attacking creature and 1 damage to you and each creature you control.";
const ALPHA_BRAWL: &str = "Target creature an opponent controls deals damage equal to its power to each other creature that player controls, then each of those creatures deals damage equal to its power to that creature.";
const KAMAHLS_SLEDGE: &str = "Kamahl's Sledge deals 4 damage to target creature.\nThreshold — If there are seven or more cards in your graveyard, instead Kamahl's Sledge deals 4 damage to that creature and 4 damage to that creature's controller.";
const CUOMBAJJ_WITCHES: &str = "{T}: This creature deals 1 damage to any target and 1 damage to any target of an opponent's choice.";
const SEARING_BLAZE: &str = "Searing Blaze deals 1 damage to target player or planeswalker and 1 damage to target creature that player or that planeswalker's controller controls.\nLandfall — If you had a land enter the battlefield under your control this turn, Searing Blaze deals 3 damage to that player or planeswalker and 3 damage to that creature instead.";
const NOMADS_EN_KOR: &str = "{0}: The next 1 damage that would be dealt to this creature this turn is dealt to target creature you control instead.";
const PIT_FIGHT: &str = "Target creature you control fights another target creature. (Each deals damage equal to its power to the other.)";

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn undergrowth_champion(
    scenario: &mut GameScenario,
    controller: PlayerId,
    counters: u32,
) -> ObjectId {
    let mut builder = scenario.add_creature_from_oracle(
        controller,
        "Undergrowth Champion",
        2,
        2,
        UNDERGROWTH_CHAMPION,
    );
    if counters > 0 {
        builder.with_plus_counters(counters);
    }
    builder.id()
}

fn spell(scenario: &mut GameScenario, name: &str, text: &str, instant: bool) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, text)
        .id()
}

fn furnace_of_rath(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE_OF_RATH)
        .id()
}

/// Pariah, controlled by `owner`; the caller attaches it after the build.
fn pariah(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(owner, "Pariah", PARIAH)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
        .id()
}

fn equipment_of(scenario: &mut GameScenario, owner: PlayerId, name: &str, text: &str) -> ObjectId {
    let mut builder = scenario.add_artifact_from_oracle(owner, name, text);
    builder.with_subtypes(vec!["Equipment"]);
    builder.id()
}

/// A textless permanent whose printed {W} pips set its controller's devotion to white.
fn devotion_to_white(scenario: &mut GameScenario, pips: usize) {
    scenario
        .add_creature(P0, "Devotion Filler", 0, 4)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White; pips],
            generic: 0,
        });
}

/// Drive to the next prompt, or to `Priority` with an empty stack, answering target slots in
/// order from `targets`.
fn drive(runner: &mut GameRunner, targets: &mut Vec<TargetRef>, events: &mut Vec<GameEvent>) {
    for _ in 0..80 {
        let action = match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                assert!(
                    !targets.is_empty(),
                    "a target slot with no target left: {:?}",
                    runner.state().waiting_for
                );
                GameAction::ChooseTarget {
                    target: Some(targets.remove(0)),
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            _ => return,
        };
        events.extend(runner.act(action).expect("drive action").events);
    }
    panic!("the game did not settle");
}

/// Cast `spell`, answering its target slots in order. A slot with one legal target is chosen by
/// the engine, so it takes none from `targets`.
fn cast(runner: &mut GameRunner, spell: ObjectId, targets: Vec<TargetRef>) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast")
        .events;
    let mut targets = targets;
    drive(runner, &mut targets, &mut events);
    events
}

/// Which candidate a CR 616.1 prompt is answered with: by its source, or by its description
/// where one source offers two effects (a resolved shield is summarized under the creature it
/// protects).
#[derive(Clone, Copy, Debug)]
enum Pick {
    From(ObjectId),
    NotFrom(ObjectId),
    Described(&'static str),
}

/// What a prompt showed when it was answered.
#[derive(Debug)]
struct Prompt {
    sources: Vec<ObjectId>,
    watched_counters: u32,
}

/// Answer the current CR 616.1 prompt with `pick`, then drive to the next stop.
fn answer_one(
    runner: &mut GameRunner,
    pick: Pick,
    watched: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Prompt {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a replacement prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    let index = candidates
        .iter()
        .position(|c| match pick {
            Pick::From(source) => c.source_id == source,
            Pick::NotFrom(source) => c.source_id != source,
            Pick::Described(description) => c.description == description,
        })
        .unwrap_or_else(|| panic!("{pick:?} is not offered: {candidates:?}"));
    let prompt = Prompt {
        sources: candidates.iter().map(|c| c.source_id).collect(),
        watched_counters: plus_counters(runner, watched),
    };
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("answer the replacement prompt")
            .events,
    );
    drive(runner, &mut Vec::new(), events);
    prompt
}

/// Answer every CR 616.1 prompt in turn; prompt `i` uses `picks[i]`, or the last pick.
fn answer(
    runner: &mut GameRunner,
    picks: &[Pick],
    watched: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        assert!(prompts.len() < 8, "too many prompts");
        let pick = picks
            .get(prompts.len())
            .or(picks.last())
            .copied()
            .expect("a pick");
        prompts.push(answer_one(runner, pick, watched, events));
    }
    prompts
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn prevented(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::DamagePrevented { .. }))
        .count()
}

fn damage_dealt_to(events: &[GameEvent], id: ObjectId) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamageDealt {
                target: TargetRef::Object(target),
                amount,
                ..
            } if *target == id => Some(*amount),
            _ => None,
        })
        .collect()
}

fn assert_settled(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

/// The battlefield host is alive with no damage and no +1/+1 counter left: its one counter
/// paid for the whole batch.
fn assert_shielded_once(runner: &GameRunner, champion: ObjectId) {
    let object = &runner.state().objects[&champion];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.damage_marked, 0);
    assert_eq!(plus_counters(runner, champion), 0);
}

/// CR 604.2 + CR 725.1: Jared Carthalion, True Heir: "If damage would be dealt to Jared Carthalion
/// while you're the monarch, prevent that damage and put that many +1/+1 counters on it." With no
/// monarch, or another player the monarch, Shock deals its 2 and Jared gains nothing.
#[test]
fn jared_carthalion_is_shielded_only_while_its_controller_is_the_monarch() {
    for (monarch, shielded) in [(None, false), (Some(P0), false), (Some(P1), true)] {
        let mut scenario = main_phase();
        let jared = scenario
            .add_creature_from_oracle(P1, "Jared Carthalion, True Heir", 3, 3, JARED_CARTHALION)
            .id();
        let shock = spell(&mut scenario, "Shock", SHOCK, true);
        let mut runner = scenario.build();
        runner.state_mut().monarch = monarch;

        let events = cast(&mut runner, shock, vec![TargetRef::Object(jared)]);

        assert_settled(&runner);
        let object = &runner.state().objects[&jared];
        if shielded {
            assert_eq!(object.damage_marked, 0, "monarch {monarch:?}");
            assert_eq!(plus_counters(&runner, jared), 2, "monarch {monarch:?}");
            assert_eq!(prevented(&events), 1, "monarch {monarch:?}");
        } else {
            assert_eq!(object.damage_marked, 2, "monarch {monarch:?}");
            assert_eq!(plus_counters(&runner, jared), 0, "monarch {monarch:?}");
            assert_eq!(prevented(&events), 0, "monarch {monarch:?}");
        }
    }
}

/// Jared's ruling: "If you're the monarch and damage would be dealt to Jared Carthalion at the same
/// time combat damage is dealt to you, Jared Carthalion's last ability will prevent damage and put
/// counters on it before the triggered ability that makes another player the monarch resolves."
#[test]
fn jared_carthalion_reads_the_monarchy_before_combat_damage_moves_it() {
    let mut scenario = main_phase();
    let jared = scenario
        .add_creature_from_oracle(P1, "Jared Carthalion, True Heir", 3, 3, JARED_CARTHALION)
        .id();
    let blocked = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let unblocked = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().monarch = Some(P1);

    run_combat(
        &mut runner,
        vec![blocked, unblocked],
        vec![(jared, blocked)],
    );

    assert_eq!(plus_counters(&runner, jared), 2);
    assert_eq!(runner.state().objects[&jared].damage_marked, 0);
    assert_eq!(runner.state().players[1].life, 18);
    // Reach guard: the monarchy changes only after the batch, through the trigger.
    assert_eq!(runner.state().monarch, Some(P1));
    drive(&mut runner, &mut Vec::new(), &mut Vec::new());
    assert_eq!(runner.state().monarch, Some(P0));
    assert_eq!(plus_counters(&runner, jared), 2);
}

/// CR 604.2 + CR 611.3a: "while it has a +1/+1 counter on it". With no counter there is no shield:
/// Lightning Bolt kills a counterless Undergrowth Champion and marks 3 on a counterless Ugin's
/// Conjurant. With counters the shield holds: Champion and Oathsworn Knight lose one ("remove a
/// +1/+1 counter"), Polukranos and the Conjurant lose three ("remove that many").
#[test]
fn counter_guards_shield_only_while_a_counter_is_on_the_host() {
    for (name, text, toughness, counters, remaining) in [
        ("Undergrowth Champion", UNDERGROWTH_CHAMPION, 2, 2, 1),
        ("Oathsworn Knight", OATHSWORN_KNIGHT, 0, 2, 1),
        ("Polukranos, Unchained", POLUKRANOS_UNCHAINED, 0, 6, 3),
        ("Ugin's Conjurant", UGINS_CONJURANT, 0, 4, 1),
    ] {
        let mut scenario = main_phase();
        let host = scenario
            .add_creature_from_oracle(P1, name, toughness, toughness, text)
            .with_plus_counters(counters)
            .id();
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();

        let events = cast(&mut runner, bolt, vec![TargetRef::Object(host)]);

        assert_settled(&runner);
        assert_eq!(runner.state().objects[&host].damage_marked, 0, "{name}");
        assert_eq!(plus_counters(&runner, host), remaining, "{name}");
        assert_eq!(prevented(&events), 1, "{name}");
    }

    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 0);
    let conjurant = scenario
        .add_creature_from_oracle(P1, "Ugin's Conjurant", 0, 5, UGINS_CONJURANT)
        .id();
    let bolt_champion = spell(&mut scenario, "Lightning Bolt", BOLT, true);
    let bolt_conjurant = spell(&mut scenario, "Lightning Bolt", BOLT, true);
    let mut runner = scenario.build();

    let mut events = cast(
        &mut runner,
        bolt_champion,
        vec![TargetRef::Object(champion)],
    );
    events.extend(cast(
        &mut runner,
        bolt_conjurant,
        vec![TargetRef::Object(conjurant)],
    ));

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&champion].zone, Zone::Graveyard);
    assert_eq!(damage_dealt_to(&events, champion), vec![3]);
    assert_eq!(runner.state().objects[&conjurant].damage_marked, 3);
    assert_eq!(prevented(&events), 0);
}

/// CR 510.2 + Undergrowth Champion's ruling ("its prevention effect will apply once to all that
/// damage"): double-blocked with one counter, both blockers' damage is prevented.
#[test]
fn undergrowth_champion_double_blocked_reads_its_guard_once() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P0, 1);
    let first = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P1, "Bear B", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![champion],
        vec![(first, champion), (second, champion)],
    );

    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
}

const ANGRATHS_MARAUDERS: &str = "If a source you control would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";
/// Fencing Ace (1/1): "Double strike (This creature deals both first-strike and regular combat
/// damage.)" The bare keyword line is the harness's keyword-hint idiom.
const FENCING_ACE: &str = "Double strike";

/// A save and load of the whole game at a prompt the combat damage batch parked on.
fn reload_at_the_combat_park(runner: &mut GameRunner) {
    assert!(
        runner
            .state()
            .pending_combat_lifelink
            .as_ref()
            .is_some_and(|record| record.replacement.is_some()),
        "the prompt is the parked combat batch's"
    );
    let json = serde_json::to_string(runner.state()).expect("serialize at the pause");
    *runner.state_mut() = serde_json::from_str::<GameState>(&json).expect("deserialize");
}

/// CR 510.2 + CR 608.2f + CR 700.1: a combat batch parked on an order prompt keeps the guard
/// reading it opened with, in memory and through a save and load between the answers. Undergrowth
/// Champion ("If damage would be dealt to this creature while it has a +1/+1 counter on it,
/// prevent that damage and remove a +1/+1 counter from this creature.") attacks with one counter,
/// and P1's Angrath's Marauders ("If a source you control would deal damage to a permanent or
/// player, it deals double that damage to that permanent or player instead.") and Grizzly Bears
/// block it. Each blocker's damage meets the Champion's shield and the Marauders' doubling, an
/// order P0 chooses (CR 616.1). Its ruling: "its prevention effect will apply once to all that
/// damage", so both portions are prevented whichever applies first (CR 616.1f: the doubled damage
/// still meets the shield), one counter is removed, and the Champion survives.
#[test]
fn undergrowth_champion_keeps_its_reading_across_both_orders_of_a_double_block() {
    for round_trip in [false, true] {
        for champion_first in [true, false] {
            let mut scenario = main_phase();
            let champion = undergrowth_champion(&mut scenario, P0, 1);
            let marauders = scenario
                .add_creature_from_oracle(P1, "Angrath's Marauders", 4, 4, ANGRATHS_MARAUDERS)
                .id();
            let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
            let mut runner = scenario.build();
            let mut answer = preferring(vec![if champion_first { champion } else { marauders }]);
            let mut asked = 0;

            let combat = fight_driving(
                &mut runner,
                &[champion],
                &[(marauders, champion), (bears, champion)],
                |runner| {
                    asked += 1;
                    if round_trip && asked == 2 {
                        reload_at_the_combat_park(runner);
                    }
                    answer(runner.state())
                },
            );

            let label = format!("round trip {round_trip}, champion first {champion_first}");
            assert_batch_settled(&runner);
            let object = &runner.state().objects[&champion];
            assert_eq!(object.zone, Zone::Battlefield, "{label}");
            assert_eq!(object.damage_marked, 0, "{label}");
            assert!(
                damage_dealt_to(&combat.events, champion).is_empty(),
                "{label}"
            );
            // Reach guards: both portions were asked of P0, each with both effects offered, the
            // one counter paid for the batch, and the Champion's own damage was dealt.
            assert_eq!(combat.prompts.len(), 2, "{label}: {:?}", combat.prompts);
            assert_eq!(combat.choosers, vec![P0, P0], "{label}");
            for prompt in &combat.prompts {
                assert_eq!(offered(prompt), offered(&[champion, marauders]), "{label}");
            }
            assert_eq!(plus_counters(&runner, champion), 0, "{label}");
            assert!(
                combat.events.iter().any(|e| matches!(
                    e,
                    GameEvent::DamageDealt { source_id, .. } if *source_id == champion
                )),
                "{label}"
            );
        }
    }
}

/// CR 608.2f + CR 700.1 + CR 611.3a: a combat batch that parks keeps the guard reading it opened
/// with, in memory and through a save and load at the second prompt. Undergrowth Champion with no
/// counter attacks and two Grizzly Bears block it; it assigns its 2 to the first (CR 510.1c). P0
/// cast Temper at X = 1 ("Prevent the next X damage that would be dealt to target creature this
/// turn. For each 1 damage prevented this way, put a +1/+1 counter on that creature.") at the
/// Champion, and controls two Hardened Scales and Doubling Season.
///
/// The batch opens with the Champion's guard ("while it has a +1/+1 counter on it") false, so its
/// shield applies to none of the 4 damage (its ruling: "its prevention effect will apply once to
/// all that damage"). Temper prevents 1, and its counter meets the three effects: P0 orders them,
/// two prompts (CR 616.1, CR 616.1f). Hardened Scales twice gives ((1+1)+1)×2 = 6; Hardened
/// Scales then Doubling Season (1+1)×2+1 = 5. The Bears' other 3 is dealt to the Champion even
/// though it has counters by then, because the batch read the guard before they were put.
#[test]
fn undergrowth_champion_keeps_its_batch_reading_across_a_combat_park() {
    for round_trip in [false, true] {
        for (second_scales, expected) in [(true, 6), (false, 5)] {
            let mut scenario = main_phase();
            let champion = undergrowth_champion(&mut scenario, P0, 0);
            let first_bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
            let second_bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
            let scales_a = scenario
                .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
                .id();
            let scales_b = scenario
                .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
                .id();
            let season = scenario
                .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON)
                .id();
            let temper = super::damage_substitution_replacements::temper(&mut scenario);
            let mut runner = scenario.build();
            super::damage_substitution_replacements::cast_temper_at_one(
                &mut runner,
                temper,
                champion,
            );
            let mut answer = preferring(vec![
                scales_a,
                if second_scales { scales_b } else { season },
            ]);
            let mut asked = 0;

            let combat = fight_driving(
                &mut runner,
                &[champion],
                &[(first_bear, champion), (second_bear, champion)],
                |runner| {
                    asked += 1;
                    if round_trip && asked == 2 {
                        // The second prompt is answered on a reloaded game: the parked record
                        // carries the batch's reading.
                        reload_at_the_combat_park(runner);
                    }
                    answer(runner.state())
                },
            );

            let label = format!("round trip {round_trip}, second Scales {second_scales}");
            assert_batch_settled(&runner);
            assert_eq!(
                runner.state().objects[&champion].damage_marked,
                3,
                "{label}"
            );
            assert!(
                combat
                    .prompts
                    .iter()
                    .all(|prompt| !prompt.contains(&champion)),
                "{label}: the Champion's shield is never offered"
            );
            assert_eq!(combat.prompts.len(), 2, "{label}: {:?}", combat.prompts);
            assert_eq!(combat.choosers, vec![P0, P0], "{label}");
            assert_eq!(
                offered(&combat.prompts[0]),
                offered(&[scales_a, scales_b, season]),
                "{label}"
            );
            // Reach guard: Temper applied, and its counters made the live guard true.
            assert_eq!(prevented(&combat.events), 1, "{label}");
            assert_eq!(plus_counters(&runner, champion), expected, "{label}");
            assert_eq!(
                runner.state().objects[&champion].zone,
                Zone::Battlefield,
                "{label}"
            );
        }
    }
}

/// CR 510.4 + CR 510.2: each combat damage step is its own batch, so a step's reading is not
/// carried past it. Undergrowth Champion with one counter attacks and P1's Fencing Ace ("Double
/// strike") blocks it. The first-strike step's 1 is prevented and removes the counter; the
/// regular step's batch opens with no counter, so its 1 is dealt.
#[test]
fn a_later_combat_damage_step_reads_the_guard_afresh() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P0, 1);
    let ace = creature_with_keywords(
        &mut scenario,
        P1,
        "Fencing Ace",
        (1, 1),
        &["Double strike"],
        FENCING_ACE,
    );
    let mut runner = scenario.build();

    let combat = fight(&mut runner, &[champion], &[(ace, champion)], None);

    assert_batch_settled(&runner);
    assert_eq!(damage_dealt_to(&combat.events, champion), vec![1]);
    assert_eq!(runner.state().objects[&champion].damage_marked, 1);
    // Reach guards: the first-strike portion was prevented and took the counter, and the
    // Champion's regular damage killed the Ace.
    assert_eq!(prevented(&combat.events), 1);
    assert_eq!(plus_counters(&runner, champion), 0);
    assert_eq!(runner.state().objects[&ace].zone, Zone::Graveyard);
    assert!(combat.prompts.is_empty());
}

/// CR 608.2f: one spell's damage from several sources to one creature is one simultaneous batch,
/// so the guard is read once at each noncombat resolver: Band Together ("Up to two target
/// creatures you control each deal damage equal to their power to another target creature."),
/// Moonlight Hunt and Coordinated Clobbering. CR 615.13: the shield applies once to the batch's
/// simultaneous damage events, so its prevention publishes as one `DamagePrevented`.
#[test]
fn noncombat_batches_read_the_guard_once() {
    // Band Together.
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let band = spell(&mut scenario, "Band Together", BAND_TOGETHER, true);
    let mut runner = scenario.build();
    let events = cast(
        &mut runner,
        band,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    );
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 1, "Band Together");

    // Moonlight Hunt.
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    for name in ["Wolf A", "Wolf B"] {
        scenario
            .add_creature(P0, name, 2, 2)
            .with_subtypes(vec!["Wolf"]);
    }
    let hunt = spell(&mut scenario, "Moonlight Hunt", MOONLIGHT_HUNT, true);
    let mut runner = scenario.build();
    let events = cast(&mut runner, hunt, vec![TargetRef::Object(champion)]);
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 1, "Moonlight Hunt");

    // Coordinated Clobbering.
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let clobbering = spell(
        &mut scenario,
        "Coordinated Clobbering",
        COORDINATED_CLOBBERING,
        false,
    );
    let mut runner = scenario.build();
    let events = cast(
        &mut runner,
        clobbering,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    );
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 1, "Coordinated Clobbering");
}

/// CR 604.2: "while it's equipped". An unequipped Outfitted Jouster takes Lightning Bolt's 3 and
/// dies, and the Equipment its controller has elsewhere is not sacrificed. Equipped, the shield
/// prevents and the rider sacrifices.
#[test]
fn outfitted_jouster_is_shielded_only_while_equipped() {
    for equipped in [false, true] {
        let (mut scenario, jouster, bolt) = jouster_scenario();
        let bonesplitter = equipment(&mut scenario, "Bonesplitter", BONESPLITTER);
        let mut runner = scenario.build();
        if equipped {
            attach_to(runner.state_mut(), bonesplitter, jouster);
        }

        let events = bolt_at(&mut runner, bolt, jouster);

        assert_settled(&runner);
        if equipped {
            assert_eq!(runner.state().objects[&jouster].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&jouster].damage_marked, 0);
            assert_eq!(runner.state().objects[&bonesplitter].zone, Zone::Graveyard);
            assert_eq!(prevented(&events), 1);
        } else {
            assert_eq!(runner.state().objects[&jouster].zone, Zone::Graveyard);
            assert_eq!(damage_dealt_to(&events, jouster), vec![3]);
            assert_eq!(
                runner.state().objects[&bonesplitter].zone,
                Zone::Battlefield
            );
            assert_eq!(prevented(&events), 0);
        }
    }
}

/// CR 611.3a: a guard read false stays false for the whole batch, even when another shield's
/// rider puts a counter on the host between portions. Vigor: "Put a +1/+1 counter on that creature
/// for each 1 damage prevented this way." Panther Habit: "prevent that damage and put that many
/// +1/+1 counters on it." No prompt, and every portion's damage becomes counters.
#[test]
fn a_false_guard_stays_false_when_a_rider_adds_a_counter() {
    // Vigor under Band Together.
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 0);
    scenario
        .add_creature(P1, "Vigor", 6, 6)
        .from_oracle_text_with_keywords(&["Trample"], VIGOR);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let band = spell(&mut scenario, "Band Together", BAND_TOGETHER, true);
    let mut runner = scenario.build();
    let events = cast(
        &mut runner,
        band,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    );
    assert_settled(&runner);
    assert_eq!(plus_counters(&runner, champion), 4, "Vigor");
    assert_eq!(runner.state().objects[&champion].damage_marked, 0);
    assert_eq!(prevented(&events), 1);

    // Panther Habit under Band Together.
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 0);
    let habit = equipment_of(&mut scenario, P1, "Panther Habit", PANTHER_HABIT);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let band = spell(&mut scenario, "Band Together", BAND_TOGETHER, true);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), habit, champion);
    cast(
        &mut runner,
        band,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    );
    assert_settled(&runner);
    assert_eq!(plus_counters(&runner, champion), 4, "Panther Habit");
    assert_eq!(runner.state().objects[&champion].damage_marked, 0);

    // Panther Habit, double-blocked (CR 510.2).
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P0, 0);
    let habit = equipment_of(&mut scenario, P0, "Panther Habit", PANTHER_HABIT);
    let first = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P1, "Bear B", 2, 2).id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), habit, champion);
    run_combat(
        &mut runner,
        vec![champion],
        vec![(first, champion), (second, champion)],
    );
    assert_settled(&runner);
    assert_eq!(
        plus_counters(&runner, champion),
        4,
        "Panther Habit in combat"
    );
    assert_eq!(runner.state().objects[&champion].damage_marked, 0);
}

struct FurnaceBand {
    runner: GameRunner,
    champion: ObjectId,
    furnace: ObjectId,
    events: Vec<GameEvent>,
}

/// Undergrowth Champion with one counter, Furnace of Rath, and Band Together cast at it: each
/// portion meets both effects, so each prompts for an order (CR 616.1).
fn band_together_under_furnace() -> FurnaceBand {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let furnace = furnace_of_rath(&mut scenario);
    let band = spell(&mut scenario, "Band Together", BAND_TOGETHER, true);
    let mut runner = scenario.build();
    let events = cast(
        &mut runner,
        band,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    );
    FurnaceBand {
        runner,
        champion,
        furnace,
        events,
    }
}

/// CR 510.2 + CR 616.1: a batch that pauses for an order choice keeps its reading. The first
/// portion parks and the second is still offered the shield, in every order of answers. The
/// shield's counter removal is its prevention's additional effect, so it waits for the batch's
/// damage (CR 615.5): the counter is still there at the second answer, and gone after.
#[test]
fn a_paused_batch_keeps_its_guard_reading_in_every_answer_order() {
    let FurnaceBand {
        champion, furnace, ..
    } = band_together_under_furnace();
    for picks in [
        [Pick::From(champion), Pick::From(champion)],
        [Pick::From(furnace), Pick::From(champion)],
        [Pick::From(champion), Pick::From(furnace)],
        [Pick::From(furnace), Pick::From(furnace)],
    ] {
        let FurnaceBand {
            mut runner,
            champion,
            mut events,
            ..
        } = band_together_under_furnace();

        let prompts = answer(&mut runner, &picks, champion, &mut events);

        assert_settled(&runner);
        assert_shielded_once(&runner, champion);
        assert_eq!(prevented(&events), 1, "{picks:?}");
        assert_eq!(prompts.len(), 2, "{picks:?}: {prompts:?}");
        assert_eq!(prompts[1].watched_counters, 1, "{picks:?}");
        assert!(prompts[1].sources.contains(&champion), "{picks:?}");
    }
}

/// The parked portion's own re-scan, answered shield first and then Furnace of Rath first: the
/// second portion's answer re-scans it under the batch's reading, not the live counterless host.
#[test]
fn the_parked_portion_rescans_under_the_batch_reading() {
    let FurnaceBand {
        mut runner,
        champion,
        furnace,
        mut events,
    } = band_together_under_furnace();

    answer(
        &mut runner,
        &[Pick::From(champion), Pick::From(furnace)],
        champion,
        &mut events,
    );

    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 1);
}

/// CR 510.2 + CR 608.2f: three portions, two of them stashed while the first parks. Moonlight Hunt
/// with three Wolves, and Coordinated Clobbering, answered Furnace of Rath first each time.
#[test]
fn every_stashed_portion_carries_the_batch_reading() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    for name in ["Wolf A", "Wolf B", "Wolf C"] {
        scenario
            .add_creature(P0, name, 2, 2)
            .with_subtypes(vec!["Wolf"]);
    }
    let furnace = furnace_of_rath(&mut scenario);
    let hunt = spell(&mut scenario, "Moonlight Hunt", MOONLIGHT_HUNT, true);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, hunt, vec![TargetRef::Object(champion)]);
    let prompts = answer(&mut runner, &[Pick::From(furnace)], champion, &mut events);
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prompts.len(), 3, "Moonlight Hunt");
    assert_eq!(prevented(&events), 1, "Moonlight Hunt");

    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let furnace = furnace_of_rath(&mut scenario);
    let clobbering = spell(
        &mut scenario,
        "Coordinated Clobbering",
        COORDINATED_CLOBBERING,
        false,
    );
    let mut runner = scenario.build();
    let mut events = cast(
        &mut runner,
        clobbering,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    );
    let prompts = answer(&mut runner, &[Pick::From(furnace)], champion, &mut events);
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prompts.len(), 2, "Coordinated Clobbering");
    assert_eq!(prevented(&events), 1, "Coordinated Clobbering");
}

/// The Champion's printed shield and Acolyte's Reward's resolved shield, as the prompt names them.
const CHAMPION_SHIELD: &str = "If damage would be dealt to ~ while it has a +1/+1 counter on it, prevent that damage and remove a +1/+1 counter from ~.";
const ACOLYTES_REWARD_SHIELD: &str = "Prevent damage";
/// The summary Test of Faith's shield shows under the creature it protects.
const TEST_OF_FAITH_SHIELD: &str = "Prevent damage";
/// Phytohydra's substitution as its prompt shows it.
const PHYTOHYDRA_SUBSTITUTION: &str =
    "If damage would be dealt to ~, put that many +1/+1 counters on it instead.";

/// CR 700.1: a rider's damage is a new event and reads the guard live. Acolyte's Reward: "If damage
/// is prevented this way, Acolyte's Reward deals that much damage to any target." Its rider at the
/// Champion, after the Champion's own shield took the counter, kills it.
#[test]
fn a_riders_damage_reads_the_guard_live() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    devotion_to_white(&mut scenario, 2);
    let acolyte = spell(&mut scenario, "Acolyte's Reward", ACOLYTES_REWARD, true);
    let band = spell(&mut scenario, "Band Together", BAND_TOGETHER, true);
    let mut runner = scenario.build();

    let mut events = cast(
        &mut runner,
        acolyte,
        vec![TargetRef::Object(champion), TargetRef::Object(champion)],
    );
    events.extend(cast(
        &mut runner,
        band,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    ));
    let prompts = answer(
        &mut runner,
        &[
            Pick::Described(CHAMPION_SHIELD),
            Pick::Described(ACOLYTES_REWARD_SHIELD),
        ],
        champion,
        &mut events,
    );

    assert_settled(&runner);
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_eq!(prevented(&events), 2);
    assert_eq!(damage_dealt_to(&events, champion), vec![2]);
    assert_eq!(runner.state().objects[&champion].zone, Zone::Graveyard);
}

/// CR 510.2 + CR 608.2f: a portion redirected to the host meets the same reading. Pariah ("All
/// damage that would be dealt to you is dealt to enchanted creature instead.") on the Champion:
/// two unblocked attackers, and Steam Blast ("deals 2 damage to each creature and each player").
#[test]
fn redirected_portions_meet_the_batch_reading() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let aura = pariah(&mut scenario, P1);
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, champion);
    run_combat(&mut runner, vec![first, second], vec![]);
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(runner.state().players[1].life, 20);

    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let aura = pariah(&mut scenario, P1);
    let blast = spell(&mut scenario, "Steam Blast", STEAM_BLAST, false);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, champion);
    let events = cast(&mut runner, blast, vec![]);
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 2);
    assert_eq!(runner.state().players[0].life, 18);
    assert_eq!(runner.state().players[1].life, 20);
}

/// A Nomads en-Kor whose shield ("The next 1 damage that would be dealt to this creature this turn
/// is dealt to target creature you control instead.") is activated at `champion`.
fn nomads_shielding(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, name, 1, 1, NOMADS_EN_KOR)
        .id()
}

fn activate_shields_at(runner: &mut GameRunner, nomads: &[ObjectId], champion: ObjectId) {
    for nomad in nomads {
        runner.activate(*nomad, 0).target_object(champion).resolve();
    }
    assert_settled(runner);
}

/// CR 701.14a + CR 608.2f: the two directions of a fight are one simultaneous damage event, so the
/// Champion's guard is read once for both. Pit Fight between two Nomads en-Kor, each one's shield
/// aimed at the Champion: both 1-damage portions are dealt to the Champion instead, and its one
/// counter prevents them both.
#[test]
fn fight_damage_redirected_to_the_host_meets_one_reading() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P0, 1);
    let nomads = [
        nomads_shielding(&mut scenario, "Nomads en-Kor A"),
        nomads_shielding(&mut scenario, "Nomads en-Kor B"),
    ];
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();
    activate_shields_at(&mut runner, &nomads, champion);

    let events = cast(
        &mut runner,
        pit_fight,
        nomads.map(TargetRef::Object).to_vec(),
    );

    assert_settled(&runner);
    for nomad in nomads {
        let object = &runner.state().objects[&nomad];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(object.damage_marked, 0);
        assert!(damage_dealt_to(&events, nomad).is_empty());
    }
    assert_eq!(prevented(&events), 1);
    assert!(damage_dealt_to(&events, champion).is_empty());
    assert_shielded_once(&runner, champion);
}

const MENDING_HANDS: &str =
    "Prevent the next 4 damage that would be dealt to any target this turn.";

/// CR 701.14a + CR 614.9 + CR 616.1: a capped redirect's hit is a portion of the fight's batch.
/// Pit Fight between two Nomads en-Kor, each one's shield ("The next 1 damage ... is dealt to
/// target creature you control instead.") aimed at P0's Wall, under Test of Faith and Mending
/// Hands. Each redirected 1 meets both shields, so P0 chooses which applies (CR 616.1: "the
/// affected object's controller ... chooses one to apply"); nothing is dealt to the Wall or the
/// Nomads, and the fight completes once, after the choices.
#[test]
fn a_fights_capped_redirects_ask_the_recipients_shield_choice() {
    let mut scenario = main_phase();
    let wall = scenario.add_creature(P0, "Wall", 0, 4).id();
    let nomads = [
        nomads_shielding(&mut scenario, "Nomads en-Kor A"),
        nomads_shielding(&mut scenario, "Nomads en-Kor B"),
    ];
    let faith = spell(&mut scenario, "Test of Faith", TEST_OF_FAITH, true);
    let hands = spell(&mut scenario, "Mending Hands", MENDING_HANDS, true);
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();
    activate_shields_at(&mut runner, &nomads, wall);
    cast(&mut runner, faith, vec![TargetRef::Object(wall)]);
    cast(&mut runner, hands, vec![TargetRef::Object(wall)]);

    let mut events = cast(
        &mut runner,
        pit_fight,
        nomads.map(TargetRef::Object).to_vec(),
    );
    let fight_cast = events.len();
    let prompts = answer(&mut runner, &[Pick::From(wall)], wall, &mut events);

    assert_settled(&runner);
    assert!(
        prompts.iter().any(|prompt| prompt.sources.len() == 2),
        "the redirected hit offers both shields: {prompts:?}"
    );
    for nomad in nomads {
        assert_eq!(runner.state().objects[&nomad].damage_marked, 0);
        assert!(damage_dealt_to(&events, nomad).is_empty());
    }
    assert!(damage_dealt_to(&events, wall).is_empty());
    assert_eq!(runner.state().objects[&wall].damage_marked, 0);
    assert_eq!(prevented(&events), 1);
    let completions = events[fight_cast..]
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::EffectResolved { kind: EffectKind::Fight, source_id, .. }
                    if *source_id == pit_fight
            )
        })
        .count();
    assert_eq!(completions, 1);
}

const STOMP: &str = "Damage can't be prevented this turn. Stomp deals 2 damage to any target.";

/// CR 614.9 + CR 702.16e + CR 615.12: a capped redirect's part meets its new recipient's
/// protection. Pit Fight between a red Hill Giant and Nomads en-Kor, whose shield is aimed at
/// their controller's Silver Knight (protection from red). The Giant still deals the redirected
/// point, so the Knight's protection prevents it ("is prevented"); the Nomads take the other 2.
/// After Stomp ("Damage can't be prevented this turn."), protection prevents nothing (CR 615.12):
/// the Knight is dealt its 1.
#[test]
fn a_capped_redirects_part_meets_its_new_recipients_protection() {
    use engine::types::keywords::{Keyword, ProtectionTarget};
    use engine::types::mana::ManaColor;
    for stomped in [false, true] {
        let mut scenario = main_phase();
        let giant = scenario
            .add_creature(P0, "Hill Giant", 3, 3)
            .with_color(vec![ManaColor::Red])
            .id();
        let nomads = nomads_shielding(&mut scenario, "Nomads en-Kor");
        let knight = scenario
            .add_creature(P0, "Silver Knight", 2, 2)
            .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
            .id();
        let stomp = spell(&mut scenario, "Stomp", STOMP, true);
        let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
        let mut runner = scenario.build();
        activate_shields_at(&mut runner, &[nomads], knight);
        if stomped {
            cast(&mut runner, stomp, vec![TargetRef::Player(P1)]);
        }

        let events = cast(
            &mut runner,
            pit_fight,
            vec![TargetRef::Object(giant), TargetRef::Object(nomads)],
        );

        assert_settled(&runner);
        let knight_prevented = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::DamagePrevented { target: TargetRef::Object(id), .. }
                    if *id == knight
            )
        });
        if stomped {
            assert_eq!(damage_dealt_to(&events, knight), vec![1], "stomped");
            assert_eq!(runner.state().objects[&knight].damage_marked, 1);
            assert!(!knight_prevented, "stomped: nothing is prevented");
        } else {
            assert!(damage_dealt_to(&events, knight).is_empty());
            assert_eq!(runner.state().objects[&knight].damage_marked, 0);
            assert!(knight_prevented);
        }
        assert_eq!(
            damage_dealt_to(&events, nomads),
            vec![2],
            "stomped {stomped}"
        );
    }
}

/// CR 616.1 + CR 702.16e: the reviewer's board, with Test of Faith on the pro-red Silver Knight
/// too. The redirected point meets two prevention effects, so the Knight's controller chooses which
/// applies: Test of Faith (a +1/+1 counter and two points of prevention remaining) or protection
/// (all three points of prevention remaining). Protection proposes into the replacement pipeline
/// as a candidate, so neither pre-empts the other.
#[test]
fn a_capped_redirects_part_offers_protection_beside_a_competing_shield() {
    use engine::types::keywords::{Keyword, ProtectionTarget};
    use engine::types::mana::ManaColor;
    for protection_first in [true, false] {
        let mut scenario = main_phase();
        let giant = scenario
            .add_creature(P0, "Hill Giant", 3, 3)
            .with_color(vec![ManaColor::Red])
            .id();
        let nomads = nomads_shielding(&mut scenario, "Nomads en-Kor");
        let knight = scenario
            .add_creature(P0, "Silver Knight", 2, 2)
            .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
            .id();
        let faith = spell(&mut scenario, "Test of Faith", TEST_OF_FAITH, true);
        let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
        let mut runner = scenario.build();
        activate_shields_at(&mut runner, &[nomads], knight);
        cast(&mut runner, faith, vec![TargetRef::Object(knight)]);

        let mut events = cast(
            &mut runner,
            pit_fight,
            vec![TargetRef::Object(giant), TargetRef::Object(nomads)],
        );
        let pick = if protection_first {
            Pick::Described("Prevent damage with protection from Red")
        } else {
            Pick::Described("Prevent damage")
        };
        let prompts = answer(&mut runner, &[pick], knight, &mut events);

        assert_settled(&runner);
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::DamagePrevented { target: TargetRef::Object(id), amount: 1, .. }
                if *id == knight
        )));
        assert!(damage_dealt_to(&events, knight).is_empty());
        assert_eq!(
            plus_counters(&runner, knight),
            if protection_first { 0 } else { 1 },
            "protection first {protection_first}"
        );
        assert_eq!(damage_dealt_to(&events, nomads), vec![2]);
    }
}

/// CR 614.9 + CR 616.1 + CR 101.4d: a capped redirect chosen by an answer splits its 1 off to a
/// creature the active player controls, while the remainder still needs the answering player's
/// next choice. P0 active; Pit Fight between P0's Hill Giant and Nomads en-Kor, whose shield is
/// aimed at P0's Wall and which P1 has since gained control of. The Nomads are under Test of
/// Faith and Mending Hands too, so P1 orders three effects and picks the redirect first. The Wall's 1 meets
/// its own two shields, P0's choice, and the Giant's other 2 still meets P1's two: P0, earlier in
/// APNAP order, is asked before P1's next choice.
#[test]
fn an_answered_capped_redirect_asks_its_recipients_choice_in_apnap_order() {
    let mut scenario = main_phase();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let nomads = nomads_shielding(&mut scenario, "Nomads en-Kor");
    let wall = scenario.add_creature(P0, "Wall", 0, 4).id();
    let shields = [nomads, wall].map(|target| {
        [
            spell(&mut scenario, "Test of Faith", TEST_OF_FAITH, true),
            spell(&mut scenario, "Mending Hands", MENDING_HANDS, true),
        ]
        .map(|shield| (shield, target))
    });
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();
    activate_shields_at(&mut runner, &[nomads], wall);
    for (shield, target) in shields.into_iter().flatten() {
        cast(&mut runner, shield, vec![TargetRef::Object(target)]);
    }
    // P1 gains control of the Nomads: `evaluate_layers` resets `controller` from
    // `base_controller`, so both are set.
    {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&nomads)
            .expect("the Nomads");
        object.base_controller = Some(P1);
        object.controller = P1;
    }

    let mut events = cast(
        &mut runner,
        pit_fight,
        vec![TargetRef::Object(giant), TargetRef::Object(nomads)],
    );

    let mut askers = Vec::new();
    for _ in 0..8 {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = runner.state().waiting_for.clone()
        else {
            break;
        };
        // The Nomads' redirect was set up first, so it is the first candidate of P1's three.
        assert!(
            !askers.is_empty() || candidates.len() == 3,
            "{candidates:?}"
        );
        askers.push(player);
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("answer")
                .events,
        );
        drive(&mut runner, &mut Vec::new(), &mut events);
    }

    assert_settled(&runner);
    assert_eq!(
        askers,
        vec![P1, P0, P1],
        "P0's choice for the Wall before P1's next"
    );
    assert_eq!(runner.state().objects[&wall].damage_marked, 0);
    assert_eq!(runner.state().objects[&nomads].damage_marked, 0);
    // The Wall's redirected 1 and the Nomads' remaining 2, each prevented.
    assert_eq!(prevented(&events), 2);
}

/// CR 701.14a + CR 616.1: a fight whose first direction parks on an order choice resumes its second
/// direction under the fight's reading. Pit Fight with Nomads en-Kor fighting the Champion under
/// Furnace of Rath, the Nomads' shield aimed at the Champion. The Nomads' 1 damage to the Champion
/// meets Furnace of Rath and the Champion's shield and is answered shield first, which takes the
/// counter; the Champion's damage back is stashed behind that choice, and the 1 of it the Nomads'
/// shield then deals to the counterless Champion is still prevented. The Champion's damage meets
/// Furnace of Rath and the Nomads' redirect, whose order changes what each recipient is dealt
/// (CR 614.9 + CR 616.1), so it is the second prompt, answered Furnace first.
#[test]
fn a_paused_fight_keeps_its_guard_reading() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P0, 1);
    let nomads = nomads_shielding(&mut scenario, "Nomads en-Kor");
    let furnace = furnace_of_rath(&mut scenario);
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();
    activate_shields_at(&mut runner, &[nomads], champion);

    let mut events = cast(
        &mut runner,
        pit_fight,
        vec![TargetRef::Object(nomads), TargetRef::Object(champion)],
    );
    let prompts = answer(
        &mut runner,
        &[Pick::From(champion), Pick::From(furnace)],
        champion,
        &mut events,
    );

    assert_settled(&runner);
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].sources.contains(&nomads), "{prompts:?}");
    assert_eq!(prompts[0].watched_counters, 1, "{prompts:?}");
    assert!(prompts[0].sources.contains(&champion), "{prompts:?}");
    // The rest of the Champion's damage stays on the Nomads, which dies.
    assert!(!damage_dealt_to(&events, nomads).is_empty());
    assert_eq!(runner.state().objects[&nomads].zone, Zone::Graveyard);
    assert_eq!(prevented(&events), 1);
    assert!(damage_dealt_to(&events, champion).is_empty());
    assert_shielded_once(&runner, champion);
}

const TEST_OF_FAITH: &str = "Prevent the next 3 damage that would be dealt to target creature this turn. For each 1 damage prevented this way, put a +1/+1 counter on that creature.";

/// CR 701.14a + CR 608.2c: one fight, two answers, one prompt at a time. Pit Fight: "Target
/// creature you control fights another target creature." P0's Phytohydra ("If damage would be
/// dealt to this creature, put that many +1/+1 counters on it instead."), under P0's Test of Faith,
/// fights P1's Dralnu, Lich Lord ("If damage would be dealt to Dralnu, sacrifice that many
/// permanents instead."). Phytohydra's 1 to Dralnu becomes P1's sacrifice of one of its four
/// permanents, P1's choice (CR 608.2d). Dralnu's 3 back meets Phytohydra and Test of Faith, an order
/// P0 chooses (CR 616.1); either order puts 3 counters on Phytohydra. The second direction waits
/// behind P1's answer instead of opening its prompt over it, so neither answer is lost. Both
/// answers are given, in either order of P0's, and the sacrifice is a Grizzly Bears or Dralnu
/// itself: fight damage is dealt simultaneously, so Dralnu's 3 is still dealt after P1 sacrifices
/// it.
///
/// Both directions are one simultaneous damage event (CR 701.14a), so the reverse direction's
/// replacements apply before anything happens (CR 614.6), and Dralnu's ruling has the sacrifice
/// happen with the damage: "If any of the sacrificed permanents would deal or be dealt damage at
/// the same time as Dralnu, they deal or are dealt damage at the same time that they're
/// sacrificed." P0, the active player, orders the reverse direction's replacements first
/// (CR 616.1, CR 101.4); then Dralnu's sacrifice runs, held until the batch's replacements
/// finish. The counters agree here with the cards.
#[test]
fn a_fight_waits_behind_dralnus_sacrifice() {
    for sacrifice_dralnu in [false, true] {
        for test_of_faith_first in [false, true] {
            let mut scenario = main_phase();
            let hydra = scenario
                .add_creature_from_oracle(P0, "Phytohydra", 1, 1, PHYTOHYDRA)
                .id();
            let dralnu = scenario
                .add_creature_from_oracle(P1, "Dralnu, Lich Lord", 3, 3, DRALNU_LICH_LORD)
                .id();
            let bears: Vec<ObjectId> = (0..3)
                .map(|_| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
                .collect();
            let test_of_faith = spell(&mut scenario, "Test of Faith", TEST_OF_FAITH, true);
            let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
            let mut runner = scenario.build();
            cast(&mut runner, test_of_faith, vec![TargetRef::Object(hydra)]);

            let mut events = cast(
                &mut runner,
                pit_fight,
                vec![TargetRef::Object(hydra), TargetRef::Object(dralnu)],
            );

            // P0, the active player, orders the reverse direction's replacements first
            // (CR 616.1, CR 101.4), before P1's held sacrifice; nothing is dealt yet.
            assert_eq!(
                plus_counters(&runner, hydra),
                0,
                "nothing is dealt before the replacements"
            );
            let pick = if test_of_faith_first {
                Pick::Described(TEST_OF_FAITH_SHIELD)
            } else {
                Pick::Described(PHYTOHYDRA_SUBSTITUTION)
            };
            let prompts = answer(&mut runner, &[pick], hydra, &mut events);

            // Then P1's sacrifice, which runs as the forward direction's held substitution.
            let WaitingFor::EffectZoneChoice {
                player,
                count,
                cards,
                ..
            } = runner.state().waiting_for.clone()
            else {
                panic!(
                    "expected Dralnu's sacrifice, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!((player, count), (P1, 1));
            let mut offered = cards.clone();
            offered.sort_by_key(|id| id.0);
            let mut permanents = bears.clone();
            permanents.push(dralnu);
            permanents.sort_by_key(|id| id.0);
            assert_eq!(offered, permanents);
            let sacrificed = if sacrifice_dralnu { dralnu } else { bears[0] };
            events.extend(
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![sacrificed],
                    })
                    .expect("P1 sacrifices one permanent")
                    .events,
            );
            drive(&mut runner, &mut Vec::new(), &mut events);

            assert_eq!(prompts.len(), 1, "{prompts:?}");
            assert_settled(&runner);
            let state = runner.state();
            assert!(state.pending_replacement.is_none());
            assert!(state.resolution_stack.is_empty());
            assert_eq!(state.objects[&pit_fight].zone, Zone::Graveyard);
            assert_eq!(state.objects[&sacrificed].zone, Zone::Graveyard);
            for other in permanents.iter().filter(|&&id| id != sacrificed) {
                assert_eq!(state.objects[other].zone, Zone::Battlefield);
            }
            assert_eq!(state.objects[&dralnu].damage_marked, 0);
            assert_eq!(state.objects[&hydra].damage_marked, 0);
            assert_eq!(
                plus_counters(&runner, hydra),
                3,
                "sacrifice Dralnu: {sacrifice_dralnu}, Test of Faith first: {test_of_faith_first}"
            );
        }
    }
}

const GRATUITOUS_VIOLENCE: &str = "If a creature you control would deal damage to a permanent or player, it deals double that damage instead.";

/// Pit Fight: P0's Siege Mastodon (3/5) fights P1's Dralnu, Lich Lord; P1 controls
/// Gratuitous Violence ("If a creature you control would deal damage to a permanent or player, it
/// deals double that damage instead.") and two Grizzly Bears. The Mastodon's 3 becomes P1's
/// sacrifice of three permanents, and P1 sacrifices Gratuitous Violence and both Bears. The cards
/// give 6: the reverse direction is part of the same damage event (CR 701.14a), so Gratuitous
/// Violence doubles Dralnu's 3 before anything happens (CR 614.6), and Dralnu's ruling deals that
/// damage at the same time as the sacrifice ("If any of the sacrificed permanents would deal or be
/// dealt damage at the same time as Dralnu, they deal or are dealt damage at the same time that
/// they're sacrificed."), so the Mastodon dies.
#[test]
fn a_fight_applies_the_reverse_replacements_before_dralnus_sacrifice() {
    let mut scenario = main_phase();
    let mastodon = scenario.add_creature(P0, "Siege Mastodon", 3, 5).id();
    let dralnu = scenario
        .add_creature_from_oracle(P1, "Dralnu, Lich Lord", 3, 3, DRALNU_LICH_LORD)
        .id();
    let violence = scenario
        .add_enchantment_from_oracle(P1, "Gratuitous Violence", GRATUITOUS_VIOLENCE)
        .id();
    let bears: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
        .collect();
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();

    let mut events = cast(
        &mut runner,
        pit_fight,
        vec![TargetRef::Object(mastodon), TargetRef::Object(dralnu)],
    );

    let WaitingFor::EffectZoneChoice { player, count, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Dralnu's sacrifice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, count), (P1, 3));
    let mut sacrificed = bears.clone();
    sacrificed.push(violence);
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: sacrificed.clone(),
            })
            .expect("P1 sacrifices three permanents")
            .events,
    );
    drive(&mut runner, &mut Vec::new(), &mut events);

    assert_settled(&runner);
    let state = runner.state();
    // Reach guard: the sacrifice happened and the reverse direction was dealt.
    for id in &sacrificed {
        assert_eq!(state.objects[id].zone, Zone::Graveyard);
    }
    assert_eq!(state.objects[&dralnu].zone, Zone::Battlefield);
    assert_eq!(state.objects[&pit_fight].zone, Zone::Graveyard);
    assert_eq!(damage_dealt_to(&events, mastodon), vec![6]);
    assert_eq!(state.objects[&mastodon].zone, Zone::Graveyard);
}

/// The Gratuitous Violence fight above, on Dralnu's controller's turn. Both directions are one
/// simultaneous event (CR 701.14a): the reverse direction is doubled by Gratuitous Violence
/// (CR 614.6) before Dralnu's held sacrifice removes it, and Dralnu's ruling deals that damage
/// at the same time as the sacrifice. So Dralnu deals 6 and the Mastodon dies, as on P0's turn.
#[test]
fn a_fight_on_dralnus_turn_doubles_the_reverse_before_the_sacrifice() {
    let mut scenario = main_phase();
    let mastodon = scenario.add_creature(P0, "Siege Mastodon", 3, 5).id();
    let dralnu = scenario
        .add_creature_from_oracle(P1, "Dralnu, Lich Lord", 3, 3, DRALNU_LICH_LORD)
        .id();
    let violence = scenario
        .add_enchantment_from_oracle(P1, "Gratuitous Violence", GRATUITOUS_VIOLENCE)
        .id();
    let bears: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
        .collect();
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let mut events = cast(
        &mut runner,
        pit_fight,
        vec![TargetRef::Object(mastodon), TargetRef::Object(dralnu)],
    );

    let WaitingFor::EffectZoneChoice { player, count, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Dralnu's sacrifice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, count), (P1, 3));
    let mut sacrificed = bears.clone();
    sacrificed.push(violence);
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: sacrificed.clone(),
            })
            .expect("P1 sacrifices three permanents")
            .events,
    );
    drive(&mut runner, &mut Vec::new(), &mut events);

    let state = runner.state();
    for id in &sacrificed {
        assert_eq!(state.objects[id].zone, Zone::Graveyard);
    }
    assert_eq!(state.objects[&pit_fight].zone, Zone::Graveyard);
    assert_eq!(damage_dealt_to(&events, mastodon), vec![6]);
    assert_eq!(state.objects[&mastodon].zone, Zone::Graveyard);
}

/// CR 608.2c + CR 700.1: a later instruction is a new event and reads the guard live. Misthios's
/// Fury: "If you control an Equipment, Misthios's Fury also deals 2 damage to that creature's
/// controller." Pariah brings that damage to the Champion after its counter paid for the first
/// sentence, and Furnace of Rath doubles it.
#[test]
fn a_later_instruction_reads_the_guard_live() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let aura = pariah(&mut scenario, P1);
    equipment_of(&mut scenario, P0, "Bonesplitter", BONESPLITTER);
    let furnace = furnace_of_rath(&mut scenario);
    let fury = spell(&mut scenario, "Misthios's Fury", MISTHIOS_FURY, true);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, champion);

    let mut events = cast(&mut runner, fury, vec![TargetRef::Object(champion)]);
    answer(
        &mut runner,
        &[Pick::From(champion), Pick::From(furnace)],
        champion,
        &mut events,
    );

    assert_settled(&runner);
    assert_eq!(prevented(&events), 1);
    assert_eq!(damage_dealt_to(&events, champion), vec![4]);
    assert_eq!(runner.state().objects[&champion].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[1].life, 20);
}

/// The batch reading survives a save and load at each prompt (`settled_damage_guards` on the
/// stashed portion and on the parked record).
#[test]
fn the_batch_reading_survives_serialization_at_the_pause() {
    let FurnaceBand {
        mut runner,
        champion,
        furnace,
        mut events,
    } = band_together_under_furnace();
    let mut prompts = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        prompts += 1;
        assert!(prompts <= 4, "too many prompts");
        let json = serde_json::to_string(runner.state()).expect("serialize at the pause");
        assert!(json.contains("settled_damage_guards"), "prompt {prompts}");
        *runner.state_mut() = serde_json::from_str::<GameState>(&json).expect("deserialize");
        answer_one(&mut runner, Pick::From(furnace), champion, &mut events);
    }
    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 1);
}

/// CR 608.2f: one dealer's two damage clauses in one sentence are one action. Chandra's Outrage:
/// "deals 4 damage to target creature and 2 damage to that creature's controller." Pariah brings
/// the second clause's damage to the Champion, and its one counter covers both.
#[test]
fn one_dealers_clauses_in_one_sentence_read_the_guard_once() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let aura = pariah(&mut scenario, P1);
    let outrage = spell(&mut scenario, "Chandra's Outrage", CHANDRAS_OUTRAGE, true);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, champion);

    let events = cast(&mut runner, outrage, vec![TargetRef::Object(champion)]);

    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 2);
    assert_eq!(runner.state().players[1].life, 20);
}

/// CR 611.3a: the same spell with the Champion read false. Vigor's rider adds counters after the
/// first clause, and the second clause still meets the false reading: no prompt, Vigor prevents
/// both, 4 + 2 counters.
#[test]
fn one_dealers_clauses_keep_a_false_reading() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 0);
    let aura = pariah(&mut scenario, P1);
    scenario
        .add_creature(P1, "Vigor", 6, 6)
        .from_oracle_text_with_keywords(&["Trample"], VIGOR);
    let outrage = spell(&mut scenario, "Chandra's Outrage", CHANDRAS_OUTRAGE, true);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, champion);

    let events = cast(&mut runner, outrage, vec![TargetRef::Object(champion)]);

    assert_settled(&runner);
    assert_eq!(plus_counters(&runner, champion), 6);
    assert_eq!(runner.state().objects[&champion].damage_marked, 0);
    assert_eq!(prevented(&events), 2);
}

/// CR 608.2f: a joined clause after two or more stashed portions. Hail Storm: "deals 2 damage to
/// each attacking creature and 1 damage to you and each creature you control." The caster's
/// attackers are the Champion and two 1/9s, under Furnace of Rath; both answer orders.
#[test]
fn a_joined_clause_after_stashed_portions_keeps_the_reading() {
    for first_pick in [0usize, 1] {
        let mut scenario = main_phase();
        let champion = undergrowth_champion(&mut scenario, P0, 1);
        let first_ox = scenario.add_creature(P0, "Ox A", 1, 9).id();
        let second_ox = scenario.add_creature(P0, "Ox B", 1, 9).id();
        let furnace = furnace_of_rath(&mut scenario);
        let storm = spell(&mut scenario, "Hail Storm", HAIL_STORM, true);
        let mut runner = scenario.build();
        runner.pass_both_players();
        runner
            .act(GameAction::DeclareAttackers {
                attacks: [champion, first_ox, second_ox]
                    .into_iter()
                    .map(|id| (id, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            })
            .expect("declare attackers");

        let mut events = cast(&mut runner, storm, vec![]);
        // The first portion parks before the Oxen's portions resolve.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(runner.state().objects[&first_ox].damage_marked, 0);
        assert_eq!(runner.state().objects[&second_ox].damage_marked, 0);
        let pick = [Pick::From(champion), Pick::From(furnace)][first_pick];
        let prompts = answer(&mut runner, &[pick], champion, &mut events);

        assert_settled(&runner);
        assert_eq!(prompts.len(), 2, "{pick:?}");
        assert_shielded_once(&runner, champion);
        assert_eq!(prevented(&events), 2, "{pick:?}");
        assert_eq!(runner.state().objects[&first_ox].damage_marked, 6);
        assert_eq!(runner.state().objects[&second_ox].damage_marked, 6);
        assert_eq!(runner.state().players[0].life, 18);
    }
}

/// Different dealers are sequential: Alpha Brawl's ruling, "The two parts of Alpha Brawl's effect
/// happen sequentially: first the targeted creature deals damage, then the other creatures deal
/// damage." Acolyte's Reward on the Bear sends its rider at the Champion, whose one counter pays
/// for it during the first part; the second part reads the guard live and kills it.
#[test]
fn a_different_dealers_clause_reads_the_guard_live() {
    // Reach guard: with no rider the counter is still there for the second part.
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let brawl = spell(&mut scenario, "Alpha Brawl", ALPHA_BRAWL, false);
    let mut runner = scenario.build();
    let events = cast(&mut runner, brawl, vec![TargetRef::Object(champion)]);
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&champion].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert_eq!(prevented(&events), 1);

    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let other = scenario.add_creature(P1, "Other Bear", 2, 2).id();
    devotion_to_white(&mut scenario, 3);
    let acolyte = spell(&mut scenario, "Acolyte's Reward", ACOLYTES_REWARD, true);
    let brawl = spell(&mut scenario, "Alpha Brawl", ALPHA_BRAWL, false);
    let mut runner = scenario.build();
    let mut events = cast(
        &mut runner,
        acolyte,
        vec![TargetRef::Object(bear), TargetRef::Object(champion)],
    );
    events.extend(cast(&mut runner, brawl, vec![TargetRef::Object(champion)]));

    assert_settled(&runner);
    assert_eq!(damage_dealt_to(&events, other), vec![3]);
    assert_eq!(prevented(&events), 2);
    assert_eq!(damage_dealt_to(&events, champion), vec![2]);
    assert_eq!(runner.state().objects[&champion].zone, Zone::Graveyard);
}

/// CR 608.2f through the "instead" swap. Kamahl's Sledge with threshold: "instead Kamahl's Sledge
/// deals 4 damage to that creature and 4 damage to that creature's controller." The swapped-in
/// clause starts the joined run.
#[test]
fn a_swapped_in_clause_starts_a_joined_run() {
    for graveyard in [7usize, 0] {
        let mut scenario = main_phase();
        let champion = undergrowth_champion(&mut scenario, P1, 1);
        let aura = pariah(&mut scenario, P1);
        for index in 0..graveyard {
            scenario.add_spell_to_graveyard(P0, &format!("Graveyard Filler {index}"), true);
        }
        let sledge = spell(&mut scenario, "Kamahl's Sledge", KAMAHLS_SLEDGE, false);
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), aura, champion);

        let events = cast(&mut runner, sledge, vec![TargetRef::Object(champion)]);

        assert_settled(&runner);
        assert_shielded_once(&runner, champion);
        assert_eq!(runner.state().players[1].life, 20);
        // Reach guard: without threshold there is one clause and one portion.
        let expected = if graveyard >= 7 { 2 } else { 1 };
        assert_eq!(prevented(&events), expected, "graveyard {graveyard}");
    }
}

/// CR 608.2f with no redirection: Cuombajj Witches, "{T}: This creature deals 1 damage to any
/// target and 1 damage to any target of an opponent's choice", both targets the Champion.
#[test]
fn one_dealers_clauses_at_one_recipient_read_the_guard_once() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let witches = scenario
        .add_creature_from_oracle(P0, "Cuombajj Witches", 1, 3, CUOMBAJJ_WITCHES)
        .id();
    let mut runner = scenario.build();

    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: witches,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Cuombajj Witches")
        .events;
    let mut targets = vec![TargetRef::Object(champion), TargetRef::Object(champion)];
    drive(&mut runner, &mut targets, &mut events);
    assert!(targets.is_empty(), "unused targets: {targets:?}");

    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 2);
}

/// CR 604.2: a guard is read only where its static ability functions. An Undergrowth Champion and a
/// Jared Carthalion in hand are not in the batch's reading, neither in the parked record nor in
/// what a player is shown.
#[test]
fn a_hand_card_guard_is_not_read() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    scenario.add_creature_to_hand_from_oracle(
        P1,
        "Undergrowth Champion",
        2,
        2,
        UNDERGROWTH_CHAMPION,
    );
    scenario.add_creature_to_hand_from_oracle(
        P0,
        "Jared Carthalion, True Heir",
        3,
        3,
        JARED_CARTHALION,
    );
    let first = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let second = scenario.add_creature(P0, "Bear B", 2, 2).id();
    furnace_of_rath(&mut scenario);
    let band = spell(&mut scenario, "Band Together", BAND_TOGETHER, true);
    let mut runner = scenario.build();
    cast(
        &mut runner,
        band,
        vec![
            TargetRef::Object(first),
            TargetRef::Object(second),
            TargetRef::Object(champion),
        ],
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let parked = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the parked portion");
    let parked: Vec<(ObjectId, DamageGuardReading)> = parked
        .settled_damage_guards
        .iter()
        .map(|guard| (guard.source.object_id, guard.reading))
        .collect();
    assert_eq!(parked, vec![(champion, DamageGuardReading::Holds)]);

    let view = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    let mut shown = Vec::new();
    collect_guard_sources(
        &serde_json::to_value(&view).expect("view serializes"),
        &mut shown,
    );
    assert!(!shown.is_empty(), "the battlefield host's guard is shown");
    assert!(
        shown.iter().all(|id| *id == champion.0),
        "only the battlefield host: {shown:?}"
    );
}

fn collect_guard_sources(value: &serde_json::Value, out: &mut Vec<u64>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "settled_damage_guards" {
                    for guard in child.as_array().into_iter().flatten() {
                        out.push(guard["source"]["object_id"].as_u64().expect("an object id"));
                    }
                }
                collect_guard_sources(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_guard_sources(item, out);
            }
        }
        _ => {}
    }
}

/// Searing Blaze's board: Undergrowth Champion with one counter wearing Pariah, and a planeswalker
/// for the Landfall clause's own target slot. That slot parses as its own target, so the spell
/// can't be cast without a planeswalker (Command Zone #175); the planeswalker is a textless filler.
struct SearingBlazeBoard {
    runner: GameRunner,
    champion: ObjectId,
    aura: ObjectId,
    furnace: Option<ObjectId>,
    events: Vec<GameEvent>,
}

fn searing_blaze_board(with_furnace: bool) -> SearingBlazeBoard {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P1, 1);
    let aura = pariah(&mut scenario, P1);
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Planeswalker Filler", "Jace", 5, "")
        .id();
    let furnace = with_furnace.then(|| furnace_of_rath(&mut scenario));
    let blaze = spell(&mut scenario, "Searing Blaze", SEARING_BLAZE, true);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, champion);
    let events = cast(
        &mut runner,
        blaze,
        vec![
            TargetRef::Player(P1),
            TargetRef::Object(walker),
            TargetRef::Object(champion),
        ],
    );
    SearingBlazeBoard {
        runner,
        champion,
        aura,
        furnace,
        events,
    }
}

/// CR 608.2f across an `else_ability`. Searing Blaze: "deals 1 damage to target player or
/// planeswalker and 1 damage to target creature that player or that planeswalker's controller
/// controls." Without landfall the second clause is the "instead" sub's base clause, and it joins.
#[test]
fn a_clause_in_an_else_branch_joins_the_run() {
    let SearingBlazeBoard {
        runner,
        champion,
        events,
        ..
    } = searing_blaze_board(false);

    assert_settled(&runner);
    assert_shielded_once(&runner, champion);
    assert_eq!(prevented(&events), 2);
    assert_eq!(runner.state().players[1].life, 20);
}

/// The same spell under Furnace of Rath pauses in its first clause. The forwarded "instead" sub,
/// whose else clause is the run's second clause, carries the reading in the paused continuation,
/// and the parked second clause meets the recipient the unpaused one meets (CR 608.2b, CR 608.2c):
/// its damage reaches the Champion and is prevented under the run's reading, and the planeswalker
/// is untouched.
#[test]
fn a_pause_in_the_first_clause_keeps_the_runs_reading() {
    for furnace_first in [true, false] {
        let SearingBlazeBoard {
            mut runner,
            champion,
            aura,
            furnace,
            mut events,
        } = searing_blaze_board(true);
        let furnace = furnace.expect("Furnace of Rath");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        let continuation = runner
            .state()
            .active_ability_continuation()
            .expect("the paused continuation");
        let forwarded = &continuation.chain;
        assert!(
            forwarded
                .condition
                .as_ref()
                .is_some_and(|condition| condition.is_instead_override()),
            "the forwarded node is the \"instead\" sub: {:?}",
            forwarded.condition
        );
        let readings = |guards: &[engine::types::ability::SettledDamageGuard]| {
            guards
                .iter()
                .map(|guard| (guard.source.object_id, guard.reading))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            readings(forwarded.context.settled_damage_guards.as_slice()),
            vec![(champion, DamageGuardReading::Holds)]
        );
        let else_clause = forwarded.else_ability.as_deref().expect("the else clause");
        assert_eq!(
            readings(else_clause.context.settled_damage_guards.as_slice()),
            vec![(champion, DamageGuardReading::Holds)]
        );
        // strict-failure: the printed second target is "target creature that player or that
        // planeswalker's controller controls", the Champion. Searing Blaze's parse (Command Zone
        // #175, and a listed confirmed misparse) announces no target for that clause, and the
        // engine binds it to the first target, P1, as the unpaused spell does; P1's Pariah brings
        // that damage to the Champion. This row holds the parked clause to the unpaused binding.
        assert_eq!(else_clause.targets, vec![TargetRef::Player(P1)]);
        let walker = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == "Planeswalker Filler")
            .map(|object| object.id)
            .expect("the planeswalker");

        let first = if furnace_first {
            Pick::NotFrom(aura)
        } else {
            Pick::From(aura)
        };
        answer(
            &mut runner,
            &[first, Pick::From(furnace)],
            champion,
            &mut events,
        );

        assert_settled(&runner);
        assert!(runner.state().pending_replacement.is_none());
        assert!(runner.state().resolution_stack.is_empty());
        assert_shielded_once(&runner, champion);
        assert_eq!(runner.state().players[1].life, 20);
        // Both clauses' damage reached the Champion, doubled, and was prevented there.
        let at_champion = events
            .iter()
            .filter(|e| {
                matches!(e, GameEvent::DamagePrevented { target: TargetRef::Object(id), amount: 2, .. }
                    if *id == champion)
            })
            .count();
        assert_eq!(at_champion, 2, "furnace first: {furnace_first}");
        assert!(damage_dealt_to(&events, walker).is_empty());
        assert_eq!(runner.state().objects[&walker].loyalty, Some(5));
    }
}

/// Steelclaw Lance and Brawler's Plate aren't in the integration card data, so the conjure reads
/// these faces. The rows read only the type line and the attachment.
fn seed_jouster_equipment_faces(runner: &mut GameRunner) {
    let registry = std::sync::Arc::make_mut(&mut runner.state_mut().card_face_registry);
    for (name, text) in [
        ("Steelclaw Lance", STEELCLAW_LANCE),
        ("Brawler's Plate", BRAWLERS_PLATE),
    ] {
        registry.insert(
            name.to_lowercase(),
            CardFace {
                name: name.to_string(),
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Artifact],
                    subtypes: vec!["Equipment".to_string()],
                },
                oracle_text: Some(text.to_string()),
                ..Default::default()
            },
        );
    }
}

fn conjured(runner: &GameRunner, name: &str) -> ObjectId {
    runner
        .state()
        .objects
        .values()
        .find(|o| o.name == name && o.zone == Zone::Battlefield)
        .unwrap_or_else(|| panic!("{name} on the battlefield"))
        .id
}

/// CR 701.3a: "When Outfitted Jouster enters, conjure a card named Steelclaw Lance and a card named
/// Brawler's Plate onto the battlefield and attach them to Outfitted Jouster." A cast Jouster
/// enters wearing both.
#[test]
fn a_cast_outfitted_jouster_enters_wearing_both_equipment() {
    let mut scenario = main_phase();
    let jouster = scenario
        .add_creature_to_hand_from_oracle(P0, "Outfitted Jouster", 2, 2, JOUSTER)
        .id();
    let mut runner = scenario.build();
    seed_jouster_equipment_faces(&mut runner);

    let events = cast(&mut runner, jouster, vec![]);

    assert_settled(&runner);
    let lance = conjured(&runner, "Steelclaw Lance");
    let plate = conjured(&runner, "Brawler's Plate");
    for equipment in [lance, plate] {
        assert_eq!(
            runner.state().objects[&equipment].attached_to,
            Some(AttachTarget::Object(jouster))
        );
    }
    let mut attachments = runner.state().objects[&jouster].attachments.clone();
    attachments.sort();
    let mut expected = vec![lance, plate];
    expected.sort();
    assert_eq!(attachments, expected);
    // Reach guard: the attaches resolve with the Jouster as their source.
    let attaches = events
        .iter()
        .filter(|e| {
            matches!(e, GameEvent::EffectResolved { kind: EffectKind::Attach, source_id, .. }
                if *source_id == jouster)
        })
        .count();
    assert_eq!(attaches, 2);
}

/// CR 400.7j + CR 608.2c + CR 701.3a: a conjure that forwards its result to a later
/// instruction still attaches what it created to its own source. No printed card has this
/// composite shape. The nearest is Outfitted Jouster, whose parsed trigger body is extended
/// here with a `Destroy { ParentTarget }` after its attach and forwarded as the parser forwards
/// that composite: the Jouster wears both Equipment until the later instruction destroys them,
/// and the Jouster itself is never that instruction's object.
#[test]
fn a_forwarding_conjure_still_attaches_what_it_created_to_its_source() {
    fn extend(trigger: &mut TriggerDefinition) {
        let body = trigger.execute.as_mut().expect("the conjure trigger body");
        assert!(!body.forward_result);
        body.forward_result = true;
        let attach = body.sub_ability.as_mut().expect("the attach clause");
        assert!(attach.sub_ability.is_none());
        attach.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::ParentTarget,
                cant_regenerate: false,
            },
        )));
    }

    let mut scenario = main_phase();
    let jouster = scenario
        .add_creature_to_hand_from_oracle(P0, "Outfitted Jouster", 2, 2, JOUSTER)
        .id();
    let mut runner = scenario.build();
    seed_jouster_equipment_faces(&mut runner);
    let object = runner.state_mut().objects.get_mut(&jouster).unwrap();
    extend(&mut std::sync::Arc::make_mut(&mut object.base_trigger_definitions)[0]);
    extend(
        &mut object
            .trigger_definitions
            .get_mut(0)
            .expect("the printed trigger")
            .definition,
    );

    let events = cast(&mut runner, jouster, vec![]);

    assert_settled(&runner);
    let conjured: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.name == "Steelclaw Lance" || o.name == "Brawler's Plate")
        .map(|o| o.id)
        .collect();
    assert_eq!(conjured.len(), 2);
    let attached: Vec<ObjectId> = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::EffectResolved {
                kind: EffectKind::Attach,
                source_id,
                subject: Some(subject),
            } if *source_id == jouster => Some(subject.identity.object_id),
            _ => None,
        })
        .collect();
    let mut sorted = attached.clone();
    sorted.sort();
    let mut expected = conjured.clone();
    expected.sort();
    assert_eq!(sorted, expected, "both attach to the Jouster");
    for equipment in conjured {
        assert_eq!(runner.state().objects[&equipment].zone, Zone::Graveyard);
    }
    assert_eq!(runner.state().objects[&jouster].zone, Zone::Battlefield);
    assert!(runner.state().objects[&jouster].attachments.is_empty());
}

/// Then Lightning Bolt: the cast Jouster is equipped, so the shield prevents and its controller
/// picks which Equipment to sacrifice.
#[test]
fn a_cast_outfitted_jouster_is_shielded_by_its_conjured_equipment() {
    let mut scenario = main_phase();
    let jouster = scenario
        .add_creature_to_hand_from_oracle(P0, "Outfitted Jouster", 2, 2, JOUSTER)
        .id();
    let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
    let mut runner = scenario.build();
    seed_jouster_equipment_faces(&mut runner);
    cast(&mut runner, jouster, vec![]);
    let lance = conjured(&runner, "Steelclaw Lance");
    let plate = conjured(&runner, "Brawler's Plate");

    let mut events = cast(&mut runner, bolt, vec![TargetRef::Object(jouster)]);

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            let mut offered = cards.clone();
            offered.sort();
            let mut attached = vec![lance, plate];
            attached.sort();
            assert_eq!(offered, attached);
        }
        other => panic!("expected EffectZoneChoice, got {other:?}"),
    }
    assert_eq!(prevented(&events), 1);
    events.extend(
        runner
            .act(GameAction::SelectCards { cards: vec![plate] })
            .expect("choose Brawler's Plate")
            .events,
    );
    drive(&mut runner, &mut Vec::new(), &mut events);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&jouster].damage_marked, 0);
    assert_eq!(runner.state().objects[&plate].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&jouster].attachments, vec![lance]);
}

// ---------------------------------------------------------------------------
// A wither or infect leg whose -1/-1 counters meet two counter modifiers pauses inside the batch
// (CR 120.3d, CR 616.1). Its remainder (the damage event and record) is the counter stash's, the
// batch's rest is the parked batch's, and the instruction's later instructions are the tail's.
// ---------------------------------------------------------------------------

/// Sickle Ripper (2/1): "Wither (This deals damage to creatures in the form of -1/-1 counters.)"
/// The bare keyword line is the harness's keyword-hint idiom, as in `wither_counter_pause_resume`.
const SICKLE_RIPPER: &str = "Wither";
const WINDING_CONSTRICTOR: &str = "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.";
const VIZIER_OF_REMEDIES: &str = "If one or more -1/-1 counters would be put on a creature you control, that many -1/-1 counters minus one are put on it instead.";
const DARKSTEEL_COLOSSUS: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nIndestructible (Damage and effects that say \"destroy\" don't destroy this creature.)\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";
const BASILISK_COLLAR: &str = "Equipped creature has deathtouch and lifelink. (Any amount of damage it deals to a creature is enough to destroy it. Damage dealt by this creature also causes you to gain that much life.)\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";
const LIFELINK_AURA: &str = "Enchant creature\nEnchanted creature has lifelink. (Damage dealt by the creature also causes its controller to gain that much life.)";
const PRIZEFIGHT: &str = "Target creature you control fights target creature you don't control. (Each deals damage equal to its power to the other.)\nCreate a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";

/// The counter modifier that joins Winding Constrictor on a wither fight's board.
#[derive(Clone, Copy, Debug)]
enum CounterModifier {
    DoublingSeason,
    VizierOfRemedies,
}

/// A fight's board: Darksteel Colossus under `colossus_side`, Sickle Ripper under the other
/// player, and Winding Constrictor (with `second`, when given) under the Colossus's controller,
/// so the Ripper's -1/-1 counters on the Colossus meet both, an order that player chooses
/// (CR 616.1).
struct WitherFight {
    scenario: GameScenario,
    colossus: ObjectId,
    ripper: ObjectId,
    constrictor: ObjectId,
    second: Option<ObjectId>,
}

fn wither_fight(colossus_side: PlayerId, second: Option<CounterModifier>) -> WitherFight {
    let mut scenario = main_phase();
    let ripper_side = if colossus_side == P0 { P1 } else { P0 };
    let colossus = creature_with_keywords(
        &mut scenario,
        colossus_side,
        "Darksteel Colossus",
        (11, 11),
        &["Trample", "Indestructible"],
        DARKSTEEL_COLOSSUS,
    );
    let ripper = creature_with_keywords(
        &mut scenario,
        ripper_side,
        "Sickle Ripper",
        (2, 1),
        &["Wither"],
        SICKLE_RIPPER,
    );
    let constrictor = winding_constrictor(&mut scenario, colossus_side);
    let second = second.map(|modifier| match modifier {
        CounterModifier::DoublingSeason => scenario
            .add_enchantment_from_oracle(colossus_side, "Doubling Season", DOUBLING_SEASON)
            .id(),
        CounterModifier::VizierOfRemedies => vizier_of_remedies(&mut scenario, colossus_side),
    });
    WitherFight {
        scenario,
        colossus,
        ripper,
        constrictor,
        second,
    }
}

fn winding_constrictor(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR)
        .id()
}

fn vizier_of_remedies(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, "Vizier of Remedies", 2, 1, VIZIER_OF_REMEDIES)
        .id()
}

/// The Lifelink Aura, controlled by `owner`; the caller attaches it after the build.
fn lifelink_aura(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(owner, "Lifelink", LIFELINK_AURA)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], LIFELINK_AURA)
        .id()
}

fn minus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Minus1Minus1)
        .copied()
        .unwrap_or(0)
}

/// The amounts of the CR 120.1 records `source` dealt to `target` this turn.
fn damage_records(runner: &GameRunner, source: ObjectId, target: ObjectId) -> Vec<u32> {
    runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|record| record.source_id == source && record.target == TargetRef::Object(target))
        .map(|record| record.amount)
        .collect()
}

/// How many CR 120.1 records `source` has this turn, to any recipient.
fn records_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|record| record.source_id == source)
        .count()
}

/// Each `DamageDealt` from `source` to `target`: its position in `events` and its amount.
fn dealt(events: &[GameEvent], source: ObjectId, target: ObjectId) -> Vec<(usize, u32)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(index, e)| match e {
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(to),
                amount,
                ..
            } if *source_id == source && *to == target => Some((index, *amount)),
            _ => None,
        })
        .collect()
}

/// The positions of each `EffectResolved` of `kind` in `events`.
fn resolutions_of(events: &[GameEvent], kind: EffectKind) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(
            |(_, e)| matches!(e, GameEvent::EffectResolved { kind: found, .. } if *found == kind),
        )
        .map(|(index, _)| index)
        .collect()
}

/// A save and load of the whole game.
fn save_and_load(runner: &mut GameRunner) {
    let json = serde_json::to_string(runner.state()).expect("serialize at the pause");
    *runner.state_mut() = serde_json::from_str::<GameState>(&json).expect("deserialize");
}

/// The open prompt is a CR 616.1 order asked of `chooser`, offering exactly `sources`.
fn assert_order_prompt(runner: &GameRunner, chooser: PlayerId, sources: &[ObjectId], label: &str) {
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "{label}: expected a replacement prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, chooser, "{label}");
    let offered_sources: Vec<ObjectId> = candidates.iter().map(|c| c.source_id).collect();
    assert_eq!(offered(&offered_sources), offered(sources), "{label}");
}

/// CR 120.3d + CR 616.1: a noncombat batch paused on a leg's counter order. The leg's remainder
/// waits in the noncombat counter stash, and the batch's rest in its `ResumeDamageBatch` carrier,
/// which holds `replaced` portions already through replacement (the paused leg is not one).
fn assert_paused_in_the_batch(runner: &GameRunner, replaced: usize, label: &str) {
    let state = runner.state();
    assert!(
        state
            .pending_damage_post_counter
            .as_ref()
            .is_some_and(|stash| !stash.is_combat),
        "{label}: the leg's remainder is stashed"
    );
    let carried = state.resolution_stack.iter().find_map(|frame| match frame {
        ResolutionFrame::AbilityContinuation(frame) => match &frame.pending.chain.effect {
            Effect::ResumeDamageBatch { replaced, .. } => Some(replaced.len()),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(carried, Some(replaced), "{label}: the batch's carrier");
}

/// CR 120.3d + CR 702.80a + CR 616.1 + CR 701.14a + CR 608.2c: a wither leg of a fight that
/// pauses for its -1/-1 counter order completes exactly once, before the batch's later portion,
/// and the batch completes once. Pit Fight: "Target creature you control fights another target
/// creature. (Each deals damage equal to its power to the other.)" Darksteel Colossus (11/11,
/// indestructible) fights Sickle Ripper ("Wither (This deals damage to creatures in the form of
/// -1/-1 counters.)"), and the Colossus's controller controls Winding Constrictor ("If one or more
/// counters would be put on an artifact or creature you control, that many plus one of each of
/// those kinds of counters are put on that permanent instead.") and Doubling Season ("If an
/// effect would put one or more counters on a permanent you control, it puts twice that many of
/// those counters on that permanent instead."). The Ripper's 2 meets both, an order that player
/// chooses (CR 616.1; CR 614.16 admits the fight's placement): Constrictor first (2+1)×2 = 6,
/// Doubling Season first 2×2+1 = 5.
///
/// With the Colossus on P0's side the Ripper's leg is delivered first and the Colossus's 11 waits
/// in the parked batch; on P1's side the Colossus's 11 is dealt before the pause. Without Doubling
/// Season nothing pauses: 2+1 = 3.
#[test]
fn a_wither_fight_leg_paused_on_its_counters_completes_once() {
    for (colossus_side, replaced_at_the_pause) in [(P0, 1), (P1, 0)] {
        for round_trip in [false, true] {
            for (constrictor_first, expected) in [(true, 6), (false, 5)] {
                let label = format!(
                    "Colossus {colossus_side:?}, round trip {round_trip}, Constrictor first {constrictor_first}"
                );
                let WitherFight {
                    mut scenario,
                    colossus,
                    ripper,
                    constrictor,
                    second,
                } = wither_fight(colossus_side, Some(CounterModifier::DoublingSeason));
                let season = second.expect("Doubling Season");
                let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
                let mut runner = scenario.build();
                // P0's creature is the first target.
                let targets = if colossus_side == P0 {
                    vec![TargetRef::Object(colossus), TargetRef::Object(ripper)]
                } else {
                    vec![TargetRef::Object(ripper), TargetRef::Object(colossus)]
                };

                let mut events = cast(&mut runner, pit_fight, targets);
                assert_order_prompt(&runner, colossus_side, &[constrictor, season], &label);
                assert_paused_in_the_batch(&runner, replaced_at_the_pause, &label);
                assert!(dealt(&events, ripper, colossus).is_empty(), "{label}");
                if round_trip {
                    save_and_load(&mut runner);
                }
                let answered_at = events.len();
                answer_one(
                    &mut runner,
                    Pick::From(if constrictor_first {
                        constrictor
                    } else {
                        season
                    }),
                    colossus,
                    &mut events,
                );

                let ripper_leg = dealt(&events, ripper, colossus);
                let colossus_leg = dealt(&events, colossus, ripper);
                assert_eq!(
                    ripper_leg
                        .iter()
                        .map(|(_, amount)| *amount)
                        .collect::<Vec<_>>(),
                    vec![2],
                    "{label}: the paused leg's damage is dealt once"
                );
                assert_settled(&runner);
                assert!(runner.state().resolution_stack.is_empty(), "{label}");
                assert!(
                    runner.state().pending_damage_post_counter.is_none(),
                    "{label}"
                );
                assert_eq!(minus_counters(&runner, colossus), expected, "{label}");
                assert!(
                    ripper_leg[0].0 >= answered_at,
                    "{label}: dealt after the answer"
                );
                assert_eq!(
                    colossus_leg
                        .iter()
                        .map(|(_, amount)| *amount)
                        .collect::<Vec<_>>(),
                    vec![11],
                    "{label}"
                );
                if colossus_side == P0 {
                    assert!(
                        ripper_leg[0].0 < colossus_leg[0].0,
                        "{label}: the paused leg completes before the batch's later portion"
                    );
                }
                assert_eq!(
                    damage_records(&runner, ripper, colossus),
                    vec![2],
                    "{label}"
                );
                assert_eq!(
                    damage_records(&runner, colossus, ripper),
                    vec![11],
                    "{label}"
                );
                assert_eq!(records_from(&runner, ripper), 1, "{label}");
                assert_eq!(records_from(&runner, colossus), 1, "{label}");
                let fights = resolutions_of(&events, EffectKind::Fight);
                assert_eq!(fights.len(), 1, "{label}");
                assert!(
                    fights[0] > ripper_leg[0].0 && fights[0] > colossus_leg[0].0,
                    "{label}: the fight completes after both legs"
                );
                assert_eq!(
                    runner.state().objects[&ripper].zone,
                    Zone::Graveyard,
                    "{label}"
                );
                assert_eq!(
                    runner.state().objects[&pit_fight].zone,
                    Zone::Graveyard,
                    "{label}"
                );
            }
        }
    }

    // Parity: Winding Constrictor alone asks nothing, and the legs complete the same way.
    for colossus_side in [P0, P1] {
        let label = format!("Constrictor alone, Colossus {colossus_side:?}");
        let WitherFight {
            mut scenario,
            colossus,
            ripper,
            ..
        } = wither_fight(colossus_side, None);
        let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
        let mut runner = scenario.build();
        let targets = if colossus_side == P0 {
            vec![TargetRef::Object(colossus), TargetRef::Object(ripper)]
        } else {
            vec![TargetRef::Object(ripper), TargetRef::Object(colossus)]
        };

        let events = cast(&mut runner, pit_fight, targets);

        assert_settled(&runner);
        assert!(runner.state().resolution_stack.is_empty(), "{label}");
        assert_eq!(minus_counters(&runner, colossus), 3, "{label}");
        let ripper_leg = dealt(&events, ripper, colossus);
        let colossus_leg = dealt(&events, colossus, ripper);
        assert_eq!(ripper_leg.len(), 1, "{label}");
        assert_eq!(ripper_leg[0].1, 2, "{label}");
        assert_eq!(colossus_leg.len(), 1, "{label}");
        assert_eq!(colossus_leg[0].1, 11, "{label}");
        assert_eq!(
            damage_records(&runner, ripper, colossus),
            vec![2],
            "{label}"
        );
        assert_eq!(
            damage_records(&runner, colossus, ripper),
            vec![11],
            "{label}"
        );
        let fights = resolutions_of(&events, EffectKind::Fight);
        assert_eq!(fights.len(), 1, "{label}");
        assert!(
            fights[0] > ripper_leg[0].0 && fights[0] > colossus_leg[0].0,
            "{label}"
        );
    }
}

/// A wither fight whose Ripper wears P1's Basilisk Collar under P1's Nefarious Lich and Laboratory
/// Maniac, with P1's library empty. P0's Darksteel Colossus is P0's first target.
struct LichFight {
    runner: GameRunner,
    colossus: ObjectId,
    ripper: ObjectId,
    constrictor: ObjectId,
    season: Option<ObjectId>,
    lich: ObjectId,
    maniac: ObjectId,
    pit_fight: ObjectId,
}

fn lich_fight(season: bool) -> LichFight {
    let WitherFight {
        mut scenario,
        colossus,
        ripper,
        constrictor,
        second,
    } = wither_fight(P0, season.then_some(CounterModifier::DoublingSeason));
    let collar = equipment_of(&mut scenario, P1, "Basilisk Collar", BASILISK_COLLAR);
    let lich = scenario
        .add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH)
        .id();
    let maniac = scenario
        .add_creature_from_oracle(P1, "Laboratory Maniac", 2, 2, LABORATORY_MANIAC)
        .id();
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), collar, ripper);
    assert!(runner.state().players[1].library.is_empty());
    LichFight {
        runner,
        colossus,
        ripper,
        constrictor,
        season: second,
        lich,
        maniac,
        pit_fight,
    }
}

/// CR 104.2b + CR 702.15b + CR 120.3, with the batch's CR 104.4a + CR 614.6 + CR 120.4: P1 won the
/// game through the Ripper's lifelink gain, and that win settled with the fight's event, after the
/// Colossus's 11. `answered_at` is where the counter order was answered, if it was asked.
fn assert_the_lifelink_win_waited_for_the_fight(
    fight: &LichFight,
    events: &[GameEvent],
    counters: u32,
    answered_at: Option<usize>,
    label: &str,
) {
    let LichFight {
        runner,
        colossus,
        ripper,
        lich,
        maniac,
        ..
    } = fight;
    let (colossus, ripper) = (*colossus, *ripper);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P1) }
        ),
        "{label}: {:?}",
        runner.state().waiting_for
    );
    let overs: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, GameEvent::GameOver { .. }))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(overs.len(), 1, "{label}");
    let over = overs[0];
    assert!(
        matches!(events[over], GameEvent::GameOver { winner: Some(P1) }),
        "{label}"
    );
    let colossus_leg = dealt(events, colossus, ripper);
    assert_eq!(colossus_leg.len(), 1, "{label}");
    assert_eq!(colossus_leg[0].1, 11, "{label}");
    assert!(
        colossus_leg[0].0 < over,
        "{label}: the Colossus's 11 is part of the fight's event, so it is dealt before the win settles"
    );
    // CR 104.2b: P1's win makes P0 lose.
    assert_eq!(
        super::damage_substitution_replacements::player_losses(events),
        vec![(
            P0,
            std::collections::BTreeSet::from([LossReason::OpponentWon])
        )],
        "{label}"
    );
    assert_nothing_after_the_game_ended(runner, events);
    // CR 104.1: the game ended as the batch settled, so the instruction never completes.
    assert!(
        resolutions_of(events, EffectKind::Fight).is_empty(),
        "{label}"
    );
    // Reach guards: the Ripper's 2 was dealt (after the answer, if one was asked), its gain became
    // the Lich's draw and the draw Laboratory Maniac's win, and the -1/-1 counters were put once.
    let ripper_leg = dealt(events, ripper, colossus);
    assert_eq!(ripper_leg.len(), 1, "{label}");
    assert_eq!(ripper_leg[0].1, 2, "{label}");
    assert!(ripper_leg[0].0 < over, "{label}");
    if let Some(answered_at) = answered_at {
        assert!(ripper_leg[0].0 >= answered_at, "{label}");
    }
    for (source, kind) in [(*lich, "GainLife"), (*maniac, "Draw")] {
        assert!(
            events[..over].iter().any(|e| matches!(
                e,
                GameEvent::ReplacementApplied { source_id, event_type }
                    if *source_id == source && event_type == kind
            )),
            "{label}: {kind} was not replaced before the game ended"
        );
    }
    // The Colossus's counters are read from their placement: P0's loss takes its objects out of
    // the game (CR 800.4a), and counters don't survive that (CR 122.2).
    let placements: Vec<(usize, u32)> = events
        .iter()
        .enumerate()
        .filter_map(|(index, e)| match e {
            GameEvent::CounterAdded {
                object_id,
                counter_type: CounterType::Minus1Minus1,
                count,
                ..
            } if *object_id == colossus => Some((index, *count)),
            _ => None,
        })
        .collect();
    assert_eq!(placements.len(), 1, "{label}: {placements:?}");
    assert_eq!(placements[0].1, counters, "{label}");
    assert!(placements[0].0 < over, "{label}");
}

/// CR 104.2b + CR 702.15b + CR 120.3 + CR 701.14a: a win an effect states during a paused wither
/// leg's remainder is recorded with the fight's event and settles after the fight's remaining
/// damage (the batch's CR 104.4a + CR 614.6 + CR 120.4). P0's Darksteel Colossus fights P1's
/// Sickle Ripper, which wears Basilisk Collar ("Equipped creature has deathtouch and lifelink.");
/// P1 controls Nefarious Lich ("If you would gain life, draw that many cards instead.") and
/// Laboratory Maniac ("If you would draw a card while your library has no cards in it, you win
/// the game instead.") with an empty library. P0 orders the Ripper's counters on the Colossus
/// (Winding Constrictor, Doubling Season). Answering deals the Ripper's 2, whose gain of 2 becomes
/// two draws and the first draw P1's win, while the batch is parked; the Colossus's 11 to the
/// Ripper is part of the same fight event, so it is dealt before the win settles with the event.
#[test]
fn a_wither_fight_legs_lifelink_win_waits_for_the_rest_of_the_fight() {
    for round_trip in [false, true] {
        for (constrictor_first, counters) in [(true, 6), (false, 5)] {
            let label = format!("round trip {round_trip}, Constrictor first {constrictor_first}");
            let mut fight = lich_fight(true);
            let season = fight.season.expect("Doubling Season");
            let targets = vec![
                TargetRef::Object(fight.colossus),
                TargetRef::Object(fight.ripper),
            ];
            let mut events = cast(&mut fight.runner, fight.pit_fight, targets);
            assert_order_prompt(&fight.runner, P0, &[fight.constrictor, season], &label);
            assert_paused_in_the_batch(&fight.runner, 1, &label);
            if round_trip {
                save_and_load(&mut fight.runner);
            }
            let answered_at = events.len();
            answer_one(
                &mut fight.runner,
                Pick::From(if constrictor_first {
                    fight.constrictor
                } else {
                    season
                }),
                fight.colossus,
                &mut events,
            );

            assert_the_lifelink_win_waited_for_the_fight(
                &fight,
                &events,
                counters,
                Some(answered_at),
                &label,
            );
        }
    }
}

/// [`a_wither_fight_legs_lifelink_win_waits_for_the_rest_of_the_fight`] with Winding Constrictor
/// alone: nothing pauses, 2+1 = 3 counters, and the win still waits for the Colossus's 11.
#[test]
fn a_wither_fight_legs_lifelink_win_waits_for_the_rest_of_the_fight_without_a_pause() {
    let mut fight = lich_fight(false);
    let targets = vec![
        TargetRef::Object(fight.colossus),
        TargetRef::Object(fight.ripper),
    ];
    let events = cast(&mut fight.runner, fight.pit_fight, targets);
    assert_the_lifelink_win_waited_for_the_fight(&fight, &events, 3, None, "no pause");
}

/// CR 608.2c + CR 111.10a: the instruction after a fight runs once, after the paused leg's
/// remainder and the batch's completion. Prizefight: "Target creature you control fights target
/// creature you don't control. (Each deals damage equal to its power to the other.)\nCreate a
/// Treasure token." P0's Darksteel Colossus fights P1's Sickle Ripper, and P0 orders the Ripper's
/// counters on the Colossus: Winding Constrictor and Vizier of Remedies ("If one or more -1/-1
/// counters would be put on a creature you control, that many -1/-1 counters minus one are put on
/// it instead."). Constrictor first 3−1 = 2, Vizier first 1+1 = 2. One Treasure, after the fight.
#[test]
fn a_fights_later_instruction_runs_once_after_a_paused_wither_leg() {
    for round_trip in [false, true] {
        for constrictor_first in [true, false] {
            let label = format!("round trip {round_trip}, Constrictor first {constrictor_first}");
            let WitherFight {
                mut scenario,
                colossus,
                ripper,
                constrictor,
                second,
            } = wither_fight(P0, Some(CounterModifier::VizierOfRemedies));
            let vizier = second.expect("Vizier of Remedies");
            let prizefight = spell(&mut scenario, "Prizefight", PRIZEFIGHT, true);
            let mut runner = scenario.build();

            let mut events = cast(
                &mut runner,
                prizefight,
                vec![TargetRef::Object(colossus), TargetRef::Object(ripper)],
            );
            assert_order_prompt(&runner, P0, &[constrictor, vizier], &label);
            assert_paused_in_the_batch(&runner, 1, &label);
            if round_trip {
                save_and_load(&mut runner);
            }
            answer_one(
                &mut runner,
                Pick::From(if constrictor_first {
                    constrictor
                } else {
                    vizier
                }),
                colossus,
                &mut events,
            );

            assert_settled(&runner);
            assert!(runner.state().resolution_stack.is_empty(), "{label}");
            let treasures: Vec<(usize, ObjectId)> = events
                .iter()
                .enumerate()
                .filter_map(|(index, e)| match e {
                    GameEvent::TokenCreated {
                        object_id, name, ..
                    } if name == "Treasure" => Some((index, *object_id)),
                    _ => None,
                })
                .collect();
            assert_eq!(treasures.len(), 1, "{label}: {treasures:?}");
            assert_eq!(
                runner.state().objects[&treasures[0].1].controller,
                P0,
                "{label}"
            );
            let fights = resolutions_of(&events, EffectKind::Fight);
            assert_eq!(fights.len(), 1, "{label}");
            assert!(
                fights[0] < treasures[0].0,
                "{label}: the Treasure comes after the fight"
            );
            let ripper_leg = dealt(&events, ripper, colossus);
            let colossus_leg = dealt(&events, colossus, ripper);
            assert_eq!(ripper_leg.len(), 1, "{label}");
            assert_eq!(colossus_leg.len(), 1, "{label}");
            assert!(
                ripper_leg[0].0 < fights[0] && colossus_leg[0].0 < fights[0],
                "{label}"
            );
            let p0_treasures = runner
                .state()
                .objects
                .values()
                .filter(|object| {
                    object.zone == Zone::Battlefield
                        && object.controller == P0
                        && object.name == "Treasure"
                })
                .count();
            assert_eq!(p0_treasures, 1, "{label}");
            assert_eq!(minus_counters(&runner, colossus), 2, "{label}");
            assert_eq!(
                runner.state().objects[&prizefight].zone,
                Zone::Graveyard,
                "{label}"
            );
        }
    }
}

/// CR 510.2 + CR 120.3d + CR 616.1 + CR 702.15b: a combat wither leg that pauses on its counter
/// order after the batch was parked and resumed in its replacement pass completes once, and the
/// batch completes once. The board of
/// [`undergrowth_champion_keeps_its_reading_across_both_orders_of_a_double_block`], plus P0's
/// Sickle Ripper attacking, enchanted by P0's Lifelink ("Enchanted creature has lifelink."), and
/// blocked by P1's Winding Constrictor; P1 also controls Vizier of Remedies. P0 answers the two
/// Champion prompts with the Champion's shield. The Ripper's 2 then meets the Constrictor's and
/// the Vizier's effects, an order P1 chooses: Constrictor first 3−1 = 2, Vizier first 1+1 = 2.
/// The gain is the damage dealt, 2, once, after that answer: P0 at 22. The Constrictor's 2,
/// doubled by Angrath's Marauders, kills the Ripper.
#[test]
fn a_wither_leg_pauses_on_its_counters_inside_a_resumed_combat_batch() {
    for round_trip in [false, true] {
        for constrictor_first in [true, false] {
            let label = format!("round trip {round_trip}, Constrictor first {constrictor_first}");
            let mut scenario = main_phase();
            let champion = undergrowth_champion(&mut scenario, P0, 1);
            let marauders = scenario
                .add_creature_from_oracle(P1, "Angrath's Marauders", 4, 4, ANGRATHS_MARAUDERS)
                .id();
            let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
            let ripper = creature_with_keywords(
                &mut scenario,
                P0,
                "Sickle Ripper",
                (2, 1),
                &["Wither"],
                SICKLE_RIPPER,
            );
            let aura = lifelink_aura(&mut scenario, P0);
            let constrictor = winding_constrictor(&mut scenario, P1);
            let vizier = vizier_of_remedies(&mut scenario, P1);
            let mut runner = scenario.build();
            attach_to(runner.state_mut(), aura, ripper);
            let mut answer = preferring(vec![
                champion,
                if constrictor_first {
                    constrictor
                } else {
                    vizier
                },
            ]);
            let mut asked = 0;

            let combat = fight_driving(
                &mut runner,
                &[champion, ripper],
                &[
                    (marauders, champion),
                    (bears, champion),
                    (constrictor, ripper),
                ],
                |runner| {
                    asked += 1;
                    if asked == 3 {
                        // The leg's remainder waits in the combat counter stash, and the batch's
                        // rest in its Phase C remnant; nothing of the leg has happened yet.
                        let state = runner.state();
                        assert!(state
                            .pending_damage_post_counter
                            .as_ref()
                            .is_some_and(|stash| stash.is_combat));
                        assert!(state
                            .pending_combat_lifelink
                            .as_ref()
                            .is_some_and(|record| record.phase_c_remnant.is_some()));
                        assert!(damage_records(runner, ripper, constrictor).is_empty());
                        assert_eq!(runner.state().players[0].life, 20);
                    }
                    if round_trip && asked == 2 {
                        reload_at_the_combat_park(runner);
                    }
                    if round_trip && asked == 3 {
                        save_and_load(runner);
                    }
                    answer(runner.state())
                },
            );

            assert_batch_settled(&runner);
            assert_eq!(
                runner.state().objects[&constrictor].zone,
                Zone::Battlefield,
                "{label}"
            );
            assert_eq!(minus_counters(&runner, constrictor), 2, "{label}");
            // The leg's damage and its lifelink gain are both emitted by the third answer.
            let third_answer = combat.answered_at[2];
            let ripper_leg = dealt(&combat.events, ripper, constrictor);
            assert_eq!(ripper_leg.len(), 1, "{label}");
            assert_eq!(ripper_leg[0].1, 2, "{label}");
            assert!(ripper_leg[0].0 >= third_answer, "{label}");
            assert_eq!(
                damage_records(&runner, ripper, constrictor),
                vec![2],
                "{label}"
            );
            assert_eq!(records_from(&runner, ripper), 1, "{label}");
            let gains: Vec<(usize, i32)> = combat
                .events
                .iter()
                .enumerate()
                .filter_map(|(index, e)| match e {
                    GameEvent::LifeChanged {
                        player_id, amount, ..
                    } if *player_id == P0 => Some((index, *amount)),
                    _ => None,
                })
                .collect();
            assert_eq!(gains.len(), 1, "{label}");
            assert_eq!(gains[0].1, 2, "{label}");
            assert!(gains[0].0 >= third_answer, "{label}");
            assert_eq!(runner.state().players[0].life, 22, "{label}");
            assert_eq!(combat.choosers, vec![P0, P0, P1], "{label}");
            assert_eq!(
                offered(&combat.prompts[2]),
                offered(&[constrictor, vizier]),
                "{label}"
            );
            assert_eq!(
                runner.state().objects[&ripper].zone,
                Zone::Graveyard,
                "{label}"
            );
            let object = &runner.state().objects[&champion];
            assert_eq!(object.zone, Zone::Battlefield, "{label}");
            assert_eq!(object.damage_marked, 0, "{label}");
            assert!(
                damage_dealt_to(&combat.events, champion).is_empty(),
                "{label}"
            );
            assert_eq!(plus_counters(&runner, champion), 0, "{label}");
        }
    }
}

// ---------------------------------------------------------------------------
// Rider pauses inside a combat batch (CR 510.2, CR 616.1; Command Zone #140)
// ---------------------------------------------------------------------------

const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const IRONSCALE_HYDRA: &str = "If a creature would deal combat damage to this creature, prevent that damage and put a +1/+1 counter on this creature.";
const PHYTOHYDRA: &str =
    "If damage would be dealt to this creature, put that many +1/+1 counters on it instead.";
const GATTA_AND_LUZZU: &str = "Flash\nWhen Gatta and Luzzu enters, choose target creature you control. If damage would be dealt to that creature this turn, prevent that damage and put that many +1/+1 counters on it.";
const CHILD_OF_NIGHT: &str = "Lifelink";
const RHOX_FAITHMENDER: &str = "Lifelink (Damage dealt by this creature also causes you to gain that much life.)\nIf you would gain life, you gain twice that much life instead.";
const LEYLINE_OF_HOPE: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf you would gain life, you gain that much life plus 1 instead.\nAs long as you have at least 7 life more than your starting life total, creatures you control get +2/+2.";
const CHANNEL_HARM: &str = "Prevent all damage that would be dealt to you and permanents you control this turn by sources you don't control. If damage is prevented this way, you may have Channel Harm deal that much damage to target creature.";
const SWANS_OF_BRYN_ARGOLL: &str = "Flying\nIf a source would deal damage to this creature, prevent that damage. The source's controller draws cards equal to the damage prevented this way.";
const ABUNDANCE: &str = "If you would draw a card, you may instead choose land or nonland and reveal cards from the top of your library until you reveal a card of the chosen kind. Put that card into your hand and put all other cards revealed this way on the bottom of your library in any order.";
const NEW_WAY_FORWARD: &str = "The next time a source of your choice would deal damage to you this turn, prevent that damage. When damage is prevented this way, New Way Forward deals that much damage to that source's controller and you draw that many cards.";
const DEFLECTING_PALM: &str = "The next time a source of your choice would deal damage to you this turn, prevent that damage. If damage is prevented this way, Deflecting Palm deals that much damage to that source's controller.";
const AWE_STRIKE: &str = "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.";
const NEFARIOUS_LICH: &str = "If damage would be dealt to you, exile that many cards from your graveyard instead. If you can't, you lose the game.\nIf you would gain life, draw that many cards instead.\nWhen this enchantment leaves the battlefield, you lose the game.";
const SHEOLDRED: &str = "Deathtouch\nWhenever you draw a card, you gain 2 life.\nWhenever an opponent draws a card, they lose 2 life.";
const EXQUISITE_BLOOD: &str = "Whenever an opponent loses life, you gain that much life.";
const FLAME_RIFT: &str = "Flame Rift deals 4 damage to each player.";
const DRALNU_LICH_LORD: &str = "If damage would be dealt to Dralnu, sacrifice that many permanents instead.\n{T}: Target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
const BOROS_SWIFTBLADE: &str = "Double strike";
const LABORATORY_MANIAC: &str =
    "If you would draw a card while your library has no cards in it, you win the game instead.";
const VAMPIRE_OF_THE_DIRE_MOON: &str = "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)\nLifelink (Damage dealt by this creature also causes you to gain that much life.)";
const VAMPIRE_NIGHTHAWK: &str = "Flying\nDeathtouch (Any amount of damage this deals to a creature is enough to destroy it.)\nLifelink (Damage dealt by this creature also causes you to gain that much life.)";

/// A main-phase board with eight library cards for each player, so a rider's draws and a later
/// draw step find cards.
pub(super) fn rider_board() -> GameScenario {
    let mut scenario = main_phase();
    for player in [P0, P1] {
        for index in 0..8 {
            scenario.add_spell_to_library_top(player, &format!("Library Filler {index}"), true);
        }
    }
    scenario
}

/// A creature built from its printed text with its keyword lines (Double strike, First strike,
/// Lifelink, Flying), which a bare keyword line would otherwise drop.
pub(super) fn creature_with_keywords(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    (power, toughness): (i32, i32),
    keywords: &[&str],
    text: &str,
) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, name, power, toughness, text)
        .from_oracle_text_with_keywords(keywords, text)
        .id()
}

/// Hardened Scales and Doubling Season under `owner`: one +1/+1 counter event meets both, an
/// order its controller chooses (CR 616.1). Returns (Hardened Scales, Doubling Season).
pub(super) fn scales_and_season(
    scenario: &mut GameScenario,
    owner: PlayerId,
) -> (ObjectId, ObjectId) {
    let scales = scenario
        .add_enchantment_from_oracle(owner, "Hardened Scales", HARDENED_SCALES)
        .id();
    let season = scenario
        .add_enchantment_from_oracle(owner, "Doubling Season", DOUBLING_SEASON)
        .id();
    (scales, season)
}

/// A [`fight_answering`] answer: each `ReplacementChoice` takes the first source of `prefer`
/// that it offers. Any other prompt is not expected, so it fails the test.
pub(super) fn preferring(prefer: Vec<ObjectId>) -> impl FnMut(&GameState) -> GameAction {
    move |state| match &state.waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            let index = prefer
                .iter()
                .find_map(|&source| candidates.iter().position(|c| c.source_id == source))
                .unwrap_or_else(|| {
                    panic!(
                        "an unexpected replacement prompt: {:?}",
                        candidates.iter().map(|c| c.source_id).collect::<Vec<_>>()
                    )
                });
            GameAction::ChooseReplacement { index }
        }
        other => panic!("an unexpected prompt: {other:?}"),
    }
}

/// The candidate sources of one prompt, as a set.
pub(super) fn offered(prompt: &[ObjectId]) -> std::collections::BTreeSet<ObjectId> {
    prompt.iter().copied().collect()
}

/// Rhox Faithmender and Leyline of Hope under P0: a lifelink gain meets both (CR 616.1).
pub(super) fn faithmender_and_leyline(scenario: &mut GameScenario) -> ObjectId {
    let rhox = creature_with_keywords(
        scenario,
        P0,
        "Rhox Faithmender",
        (1, 5),
        &["Lifelink"],
        RHOX_FAITHMENDER,
    );
    scenario.add_enchantment_from_oracle(P0, "Leyline of Hope", LEYLINE_OF_HOPE);
    rhox
}

/// What one combat showed: its events, and the candidate sources of each CR 616.1 prompt it
/// answered, with the player each was asked of and the number of events emitted before its
/// answer.
pub(super) struct Combat {
    pub(super) events: Vec<GameEvent>,
    pub(super) prompts: Vec<Vec<ObjectId>>,
    pub(super) choosers: Vec<PlayerId>,
    pub(super) answered_at: Vec<usize>,
}

/// Drive one combat from the precombat main phase to the end of combat: P0 attacks P1 with
/// `attackers`, and P1 blocks with `blocks`. A replacement prompt is answered with `prefer`'s
/// candidate; a prompt that does not offer it is not expected, so it fails the test. The drive
/// also stops where the game ended.
pub(super) fn fight(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
    prefer: Option<ObjectId>,
) -> Combat {
    fight_answering(runner, attackers, blocks, |state| {
        match &state.waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let sources: Vec<ObjectId> = candidates.iter().map(|c| c.source_id).collect();
                let index = prefer
                    .and_then(|preferred| sources.iter().position(|&s| s == preferred))
                    .unwrap_or_else(|| panic!("an unexpected replacement prompt: {sources:?}"));
                GameAction::ChooseReplacement { index }
            }
            other => panic!("combat stopped at {other:?}"),
        }
    })
}

/// [`fight`], with every `ReplacementChoice` and `EffectZoneChoice` (or other prompt) the combat
/// raises answered by `answer`. Each `ReplacementChoice`'s candidate sources are still recorded
/// in `prompts`.
pub(super) fn fight_answering(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
    mut answer: impl FnMut(&GameState) -> GameAction,
) -> Combat {
    fight_driving(runner, attackers, blocks, |runner| answer(runner.state()))
}

/// [`fight_answering`], with `answer` given the runner itself, so a row can inspect or replace
/// the state at a prompt (a save and load) before it answers.
pub(super) fn fight_driving(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
    mut answer: impl FnMut(&mut GameRunner) -> GameAction,
) -> Combat {
    let mut combat = Combat {
        events: Vec::new(),
        prompts: Vec::new(),
        choosers: Vec::new(),
        answered_at: Vec::new(),
    };
    let (mut attacked, mut blocked) = (false, false);
    for _ in 0..400 {
        let state = runner.state();
        if matches!(state.phase, Phase::EndCombat | Phase::PostCombatMain)
            || matches!(state.waiting_for, WaitingFor::GameOver { .. })
        {
            return combat;
        }
        let turn_action = match &state.waiting_for {
            WaitingFor::Priority { .. } => Some(GameAction::PassPriority),
            WaitingFor::OrderTriggers { triggers, .. } => Some(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            }),
            WaitingFor::DeclareAttackers { .. } => {
                let attacks = if attacked {
                    Vec::new()
                } else {
                    attackers
                        .iter()
                        .map(|&attacker| (attacker, AttackTarget::Player(P1)))
                        .collect()
                };
                attacked = true;
                Some(GameAction::DeclareAttackers {
                    attacks,
                    bands: vec![],
                })
            }
            WaitingFor::DeclareBlockers { .. } => {
                let assignments = if blocked { Vec::new() } else { blocks.to_vec() };
                blocked = true;
                Some(GameAction::DeclareBlockers { assignments })
            }
            WaitingFor::AssignCombatDamage {
                blockers,
                total_damage,
                ..
            } => {
                // CR 510.1c: the attacking creature's controller divides its damage among the
                // blockers as they choose; this helper assigns lethal damage to each blocker and
                // the rest to the last one.
                let mut remaining = *total_damage;
                let mut assignments: Vec<(ObjectId, u32)> = blockers
                    .iter()
                    .map(|slot| {
                        let assign = remaining.min(slot.lethal_minimum);
                        remaining -= assign;
                        (slot.blocker_id, assign)
                    })
                    .collect();
                if let Some(last) = assignments.last_mut() {
                    last.1 += remaining;
                }
                Some(GameAction::AssignCombatDamage {
                    mode: CombatDamageAssignmentMode::Normal,
                    assignments,
                    trample_damage: 0,
                    controller_damage: 0,
                })
            }
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                combat
                    .prompts
                    .push(candidates.iter().map(|c| c.source_id).collect());
                combat.choosers.push(*player);
                combat.answered_at.push(combat.events.len());
                None
            }
            // Any other prompt the combat raises (a zone choice, a named choice) is `answer`'s.
            _ => None,
        };
        let action = turn_action.unwrap_or_else(|| answer(runner));
        combat
            .events
            .extend(runner.act(action).expect("combat action").events);
    }
    panic!("combat did not end");
}

/// The batch left nothing behind: no park, no resolution frame, and a priority window at the end
/// of combat with an empty stack.
pub(super) fn assert_batch_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        state.pending_replacement.is_none(),
        "a park outlived the batch"
    );
    assert!(
        state.resolution_stack.is_empty(),
        "{} resolution frame(s) outlived the batch",
        state.resolution_stack.len()
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty());
    assert_eq!(state.phase, Phase::EndCombat);
}

/// CR 104.1: the game ended inside the batch. P0 won, P1 lost, the end was announced once, and
/// nothing is parked or left on the resolution stack.
fn assert_p1_lost_inside_the_batch(runner: &GameRunner, events: &[GameEvent]) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::GameOver { winner: Some(winner) } if winner == P0),
        "got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.game_end.as_ref().map(|end| end.winner),
        Some(Some(P0))
    );
    assert!(state.players[1].is_eliminated);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::GameOver { .. }))
            .count(),
        1
    );
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[usize::from(player.0)].hand.len()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[usize::from(player.0)].life
}

/// Gatta and Luzzu in P0's hand, with its Flash line.
fn gatta_and_luzzu(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Gatta and Luzzu", 1, 1, GATTA_AND_LUZZU)
        .from_oracle_text_with_keywords(&["Flash"], GATTA_AND_LUZZU)
        .id()
}

fn swans_of_bryn_argoll(scenario: &mut GameScenario) -> ObjectId {
    creature_with_keywords(
        scenario,
        P1,
        "Swans of Bryn Argoll",
        (4, 3),
        &["Flying"],
        SWANS_OF_BRYN_ARGOLL,
    )
}

/// CR 616.1g at the per-leg drain, on a substitution's action. Phytohydra: "If damage would be
/// dealt to this creature, put that many +1/+1 counters on it instead." Blocking a 2/2 under its
/// controller's Hardened Scales and Doubling Season, its 2 counters meet both, and the combat
/// batch asks P1 the order (CR 616.1): Hardened Scales first (2+1)×2 = 6, Doubling Season first
/// 2×2+1 = 5. Without Hardened Scales it gets 2 and nothing is asked. Nothing outlives the batch,
/// and the state-based actions after it run (Lightning Bolt kills a 3/3).
#[test]
fn phytohydras_counter_order_is_asked_inside_the_batch() {
    for (with_scales, scales_first, expected) in
        [(true, true, 6), (true, false, 5), (false, true, 2)]
    {
        let mut scenario = rider_board();
        let hydra = scenario
            .add_creature_from_oracle(P1, "Phytohydra", 1, 1, PHYTOHYDRA)
            .id();
        let counter_sources = with_scales.then(|| scales_and_season(&mut scenario, P1));
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        let prefer = counter_sources
            .map(|(scales, season)| vec![if scales_first { scales } else { season }])
            .unwrap_or_default();

        let combat = fight_answering(&mut runner, &[bears], &[(hydra, bears)], preferring(prefer));

        assert_batch_settled(&runner);
        match counter_sources {
            Some((scales, season)) => {
                assert_eq!(combat.prompts.len(), 1, "prompts: {:?}", combat.prompts);
                assert_eq!(offered(&combat.prompts[0]), offered(&[scales, season]));
            }
            None => assert!(combat.prompts.is_empty()),
        }
        assert_eq!(plus_counters(&runner, hydra), expected);
        assert_eq!(runner.state().objects[&hydra].damage_marked, 0);
        assert_eq!(runner.state().objects[&bears].damage_marked, 1);

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// CR 616.1 at the aggregate drain. Gatta and Luzzu: "If damage would be dealt to that creature
/// this turn, prevent that damage and put that many +1/+1 counters on it." Its chosen Grizzly
/// Bears, blocked by a 2/2 under its controller's Hardened Scales and Doubling Season.
#[test]
fn gatta_and_luzzus_counter_order_is_settled_after_the_batch() {
    for scales in [true, false] {
        let mut scenario = rider_board();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let gatta = gatta_and_luzzu(&mut scenario);
        if scales {
            scales_and_season(&mut scenario, P0);
        }
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        cast(&mut runner, gatta, vec![TargetRef::Object(bears)]);

        let combat = fight(&mut runner, &[bears], &[(blocker, bears)], None);

        assert_batch_settled(&runner);
        assert!(combat.prompts.is_empty());
        assert_eq!(prevented(&combat.events), 1);
        assert_eq!(runner.state().objects[&bears].damage_marked, 0);
        assert_eq!(runner.state().objects[&blocker].zone, Zone::Graveyard);
        let counters = plus_counters(&runner, bears);
        if scales {
            // strict-failure: the card reads one event of 2 counters, 5 or 6 by its controller's
            // order (CR 616.1, CR 616.1f; Hardened Scales' ruling: "you choose the order to apply
            // those effects"). "Put that many +1/+1 counters on it" parses as one counter repeated
            // per damage (Command Zone #174, with Brace for Impact, Temper and Test of Faith), so
            // each counter meets its own order, 3 or 4, and combat can't ask it (Command Zone
            // #140): the engine takes the first candidate each time, not P0's pick.
            assert!([6, 7, 8].contains(&counters), "got {counters} counters");
        } else {
            assert_eq!(counters, 2);
        }

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// A lifelink gain answered in combat, then Gatta and Luzzu's rider at the aggregate drain. Child
/// of Night is the chosen creature; P0's Rhox Faithmender and Leyline of Hope meet the lifelink
/// gain (CR 702.15b), and P0 answers Rhox Faithmender first: 2 doubled, plus 1. That is the one
/// prompt: the rider's counter order is settled inside the batch.
#[test]
fn a_lifelink_answer_then_gatta_and_luzzus_rider() {
    for scales in [true, false] {
        let mut scenario = rider_board();
        let child = creature_with_keywords(
            &mut scenario,
            P0,
            "Child of Night",
            (2, 1),
            &["Lifelink"],
            CHILD_OF_NIGHT,
        );
        let rhox = faithmender_and_leyline(&mut scenario);
        if scales {
            scales_and_season(&mut scenario, P0);
        }
        let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let gatta = gatta_and_luzzu(&mut scenario);
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        cast(&mut runner, gatta, vec![TargetRef::Object(child)]);
        let life_before = life(&runner, P0);

        let combat = fight(&mut runner, &[child], &[(blocker, child)], Some(rhox));

        assert_batch_settled(&runner);
        assert_eq!(combat.prompts.len(), 1, "prompts: {:?}", combat.prompts);
        assert!(combat.prompts[0].contains(&rhox));
        assert_eq!(life(&runner, P0) - life_before, 5);
        assert_eq!(prevented(&combat.events), 1);
        assert_eq!(runner.state().objects[&child].damage_marked, 0);
        assert_eq!(runner.state().objects[&blocker].zone, Zone::Graveyard);
        let counters = plus_counters(&runner, child);
        if scales {
            // strict-failure: the card reads one event of 2 counters, 5 or 6 by its controller's
            // order (CR 616.1, CR 616.1f; Hardened Scales' ruling); the per-counter parse (Command
            // Zone #174) gives each counter its own order, 3 or 4, which the engine answers with the
            // first candidate, not P0's pick (Command Zone #140).
            assert!([6, 7, 8].contains(&counters), "got {counters} counters");
        } else {
            assert_eq!(counters, 2);
        }

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// The rider's own prompt at the aggregate drain. Channel Harm: "If damage is prevented this way,
/// you may have Channel Harm deal that much damage to target creature." Blocked, the Hill Giant's
/// 3 to the Bears is prevented and P0 may have Channel Harm deal 3 to the Giant. Unblocked, nothing
/// is prevented and P1 takes 2. The card reports unsupported for the unasked "may".
#[test]
fn channel_harms_may_is_settled_after_the_batch() {
    for block in [true, false] {
        let mut scenario = rider_board();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
        let p1_bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let channel_harm = spell(&mut scenario, "Channel Harm", CHANNEL_HARM, true);
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        cast(&mut runner, channel_harm, vec![TargetRef::Object(giant)]);
        let blocks = if block { vec![(giant, bears)] } else { vec![] };

        let combat = fight(&mut runner, &[bears], &blocks, None);

        // strict-failure: CR 608.2d: whether Channel Harm deals the 3 is P0's choice, and combat
        // can't ask it yet (Command Zone #140). The engine clears the "may" unasked, so the Hill
        // Giant is never dealt the 3; that is not P0's answer.
        assert_batch_settled(&runner);
        assert!(combat.prompts.is_empty());
        assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&bears].damage_marked, 0);
        if block {
            assert_eq!(prevented(&combat.events), 1);
            assert_eq!(runner.state().objects[&giant].damage_marked, 2);
            assert_eq!(life(&runner, P1), 20);
        } else {
            assert_eq!(prevented(&combat.events), 0);
            assert_eq!(runner.state().objects[&giant].damage_marked, 0);
            assert_eq!(life(&runner, P1), 18);
        }

        bolt_at(&mut runner, bolt, p1_bears);
        assert_eq!(runner.state().objects[&p1_bears].zone, Zone::Graveyard);
    }
}

/// Answers Abundance ("If you would draw a card, you may instead choose land or nonland and
/// reveal cards from the top of your library until you reveal a card of the chosen kind. Put that
/// card into your hand …"): accept it and choose nonland, or decline it and draw.
fn answering_abundance(accept: bool) -> impl FnMut(&GameState) -> GameAction {
    move |state| match &state.waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            let wanted = if accept { "Accept" } else { "Decline" };
            let index = candidates
                .iter()
                .position(|c| c.description == wanted)
                .unwrap_or_else(|| panic!("Abundance's {wanted} is offered: {candidates:?}"));
            GameAction::ChooseReplacement { index }
        }
        WaitingFor::NamedChoice { .. } => GameAction::ChooseOption {
            choice: "Nonland".to_string(),
        },
        WaitingFor::RevealUntilBottomOrder { cards, .. } => GameAction::SelectCards {
            cards: cards.clone(),
        },
        other => panic!("an unexpected prompt: {other:?}"),
    }
}

fn cards_drawn(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == player))
        .count()
}

/// Both drains pause in one batch. Swans of Bryn Argoll blocks one of P0's Grizzly Bears: "The
/// source's controller draws cards equal to the damage prevented this way", each draw meeting
/// P0's Abundance, whose "may" the combat batch asks P0 (CR 616.1): two draws, two prompts.
/// Declining both draws 2 cards; accepting both and choosing nonland puts the two nonland cards
/// revealed into P0's hand instead, with no draw. P0's Channel Harm, cast at the Hill Giant
/// blocking the other Bears, prevents the blockers' damage and asks its "may" after the batch.
#[test]
fn a_may_park_and_a_riders_own_prompt_asks_abundance_and_settles_channel_harm() {
    for (abundance, accept) in [(true, false), (true, true), (false, false)] {
        let mut scenario = rider_board();
        let first = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let second = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
        let swans = swans_of_bryn_argoll(&mut scenario);
        if abundance {
            scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE);
        }
        let p1_bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let channel_harm = spell(&mut scenario, "Channel Harm", CHANNEL_HARM, true);
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        cast(&mut runner, channel_harm, vec![TargetRef::Object(giant)]);
        let hand_before = hand_size(&runner, P0);

        let combat = fight_answering(
            &mut runner,
            &[first, second],
            &[(giant, first), (swans, second)],
            answering_abundance(accept),
        );

        assert_batch_settled(&runner);
        assert_eq!(combat.prompts.len(), if abundance { 2 } else { 0 });
        // CR 615.13 + CR 120.3: Channel Harm's one application publishes once per recipient (the
        // two Bears), and the Swans' once.
        assert_eq!(prevented(&combat.events), 3);
        // CR 121.2: the Swans' 2 is two draws, so 2 cards either way.
        assert_eq!(hand_size(&runner, P0) - hand_before, 2);
        assert_eq!(
            cards_drawn(&combat.events, P0),
            if abundance && accept { 0 } else { 2 }
        );
        // strict-failure: CR 510.2 + CR 616.1: Channel Harm's "you may have Channel Harm deal
        // that much damage" is P0's to answer, and the aggregate drain can't ask it yet (Command
        // Zone #140); it is cleared unasked, so the Giant takes only the Bears' 2.
        assert_eq!(runner.state().objects[&giant].damage_marked, 2);
        for bears in [first, second] {
            assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&bears].damage_marked, 0);
        }
        assert_eq!(runner.state().objects[&swans].damage_marked, 0);

        bolt_at(&mut runner, bolt, p1_bears);
        assert_eq!(runner.state().objects[&p1_bears].zone, Zone::Graveyard);
    }
}

/// A "may" at the per-leg drain. Swans of Bryn Argoll blocks P0's Grizzly Bears; the Swans' two
/// draws each meet P0's Abundance ("If you would draw a card, you may instead …"), and the combat
/// batch asks each (CR 616.1). Declining both draws 2 cards; accepting both and choosing nonland
/// puts 2 nonland cards into P0's hand with no draw. Without Abundance, 2 draws and no prompt.
#[test]
fn swans_of_bryn_argolls_draws_ask_abundance() {
    for (abundance, accept) in [(true, false), (true, true), (false, false)] {
        let mut scenario = rider_board();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let swans = swans_of_bryn_argoll(&mut scenario);
        if abundance {
            scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE);
        }
        let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        let hand_before = hand_size(&runner, P0);

        let combat = fight_answering(
            &mut runner,
            &[bears],
            &[(swans, bears)],
            answering_abundance(accept),
        );

        assert_batch_settled(&runner);
        assert_eq!(combat.prompts.len(), if abundance { 2 } else { 0 });
        assert_eq!(prevented(&combat.events), 1);
        assert_eq!(hand_size(&runner, P0) - hand_before, 2);
        assert_eq!(
            cards_drawn(&combat.events, P0),
            if abundance && accept { 0 } else { 2 }
        );
        assert_eq!(runner.state().objects[&swans].damage_marked, 0);
        assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// The "may" in each combat damage step (CR 510.4, CR 702.4b): Boros Swiftblade (double strike)
/// blocked by Swans of Bryn Argoll has its 1 prevented in each step, so P0 draws one card in each
/// and is asked Abundance's "may" once per step. Declining draws a card in each step; accepting
/// and choosing nonland puts a nonland card into P0's hand in each, with no draw. The Swans kill
/// the Swiftblade.
#[test]
fn double_strike_asks_abundance_for_the_swans_draw_in_each_damage_step() {
    for (abundance, accept) in [(true, false), (true, true), (false, false)] {
        let mut scenario = rider_board();
        let blade = creature_with_keywords(
            &mut scenario,
            P0,
            "Boros Swiftblade",
            (1, 2),
            &["Double strike"],
            BOROS_SWIFTBLADE,
        );
        let swans = swans_of_bryn_argoll(&mut scenario);
        if abundance {
            scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE);
        }
        let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        let hand_before = hand_size(&runner, P0);

        let combat = fight_answering(
            &mut runner,
            &[blade],
            &[(swans, blade)],
            answering_abundance(accept),
        );

        assert_batch_settled(&runner);
        assert_eq!(combat.prompts.len(), if abundance { 2 } else { 0 });
        assert_eq!(prevented(&combat.events), 2);
        assert_eq!(hand_size(&runner, P0) - hand_before, 2);
        assert_eq!(
            cards_drawn(&combat.events, P0),
            if abundance && accept { 0 } else { 2 }
        );
        assert_eq!(runner.state().objects[&swans].damage_marked, 0);
        assert_eq!(runner.state().objects[&blade].zone, Zone::Graveyard);

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// A "may" park at the aggregate drain. P1's New Way Forward: "When damage is prevented this way,
/// New Way Forward deals that much damage to that source's controller and you draw that many
/// cards." Its chosen source is P0's unblocked Grizzly Bears, and P1's draws meet P1's Abundance.
#[test]
fn new_way_forwards_draws_are_kept_under_abundance() {
    for abundance in [true, false] {
        let mut scenario = rider_board();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        if abundance {
            scenario.add_enchantment_from_oracle(P1, "Abundance", ABUNDANCE);
        }
        let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let new_way_forward = scenario
            .add_spell_to_hand_from_oracle(P1, "New Way Forward", true, NEW_WAY_FORWARD)
            .id();
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();
        runner
            .act(GameAction::PassPriority)
            .expect("P0 passes priority to P1");
        let card_id = runner.state().objects[&new_way_forward].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: new_way_forward,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("P1 casts New Way Forward")
            .events;
        let mut source_choices = 0;
        loop {
            drive(&mut runner, &mut Vec::new(), &mut events);
            if !matches!(
                runner.state().waiting_for,
                WaitingFor::DamageSourceChoice { .. }
            ) {
                break;
            }
            source_choices += 1;
            events.extend(
                runner
                    .act(GameAction::ChooseDamageSource { source: bears })
                    .expect("P1 chooses P0's Grizzly Bears")
                    .events,
            );
        }
        assert_eq!(source_choices, 1);
        assert_settled(&runner);
        let (p0_before, p1_before, hand_before) =
            (life(&runner, P0), life(&runner, P1), hand_size(&runner, P1));

        let combat = fight(&mut runner, &[bears], &[], None);

        // strict-failure: CR 510.2 + CR 616.1: Abundance's "may" on each draw is P1's to answer,
        // and combat can't ask it yet (Command Zone #140); each is declined, which keeps the
        // draw. CR 121.2: "you draw that many cards" is 2 draws, so 2 cards.
        assert_batch_settled(&runner);
        assert!(combat.prompts.is_empty());
        assert_eq!(life(&runner, P0) - p0_before, -2);
        assert_eq!(life(&runner, P1), p1_before);
        assert_eq!(hand_size(&runner, P1) - hand_before, 2);

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// Nefarious Lich: "If damage would be dealt to you, exile that many cards from your graveyard
/// instead. If you can't, you lose the game." An unblocked 3/3 with 4 cards in its controller's
/// graveyard: the exile is the substitution's own choice of 3 (CR 608.2d, CR 614.6), and the
/// combat batch asks it. The 3 P1 picks are exiled, P1 doesn't lose, and takes no damage.
#[test]
fn nefarious_lichs_exile_choice_is_asked_inside_the_batch() {
    let mut scenario = rider_board();
    let fillers: Vec<ObjectId> = (0..4)
        .map(|index| {
            scenario
                .add_creature_to_graveyard(P1, &format!("Graveyard Filler {index}"), 1, 1)
                .id()
        })
        .collect();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
    let mut runner = scenario.build();
    let picked = vec![fillers[3], fillers[0], fillers[2]];
    let mut asked = Vec::new();

    let combat = fight_answering(&mut runner, &[giant], &[], |state| {
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                asked.push((*player, *count, cards.len()));
                GameAction::SelectCards {
                    cards: picked.clone(),
                }
            }
            other => panic!("an unexpected prompt: {other:?}"),
        }
    });

    assert_batch_settled(&runner);
    assert!(combat.prompts.is_empty());
    assert_eq!(asked, vec![(P1, 3, 4)]);
    assert!(!runner.state().players[1].is_eliminated);
    // Reach guard: the substitution applied, so no damage reached P1.
    assert_eq!(life(&runner, P1), 20);
    for &card in &picked {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    }
    assert_eq!(
        runner.state().objects[&fillers[1]].zone,
        Zone::Graveyard,
        "the card P1 kept"
    );
    assert_eq!(runner.state().players[1].graveyard.len(), 1);

    bolt_at(&mut runner, bolt, courser);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
}

/// "If you can't, you lose the game": with fewer than 3 cards in its controller's graveyard, an
/// unblocked 3/3 ends the game inside the batch (CR 104.1), and the batch leaves nothing behind.
#[test]
fn nefarious_lich_ends_the_game_inside_the_batch() {
    for graveyard in [2usize, 0] {
        let mut scenario = rider_board();
        for index in 0..graveyard {
            scenario.add_creature_to_graveyard(P1, &format!("Graveyard Filler {index}"), 1, 1);
        }
        scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let mut runner = scenario.build();

        let combat = fight(&mut runner, &[giant], &[], None);

        assert_p1_lost_inside_the_batch(&runner, &combat.events);
        assert!(combat.prompts.is_empty());
        assert_eq!(life(&runner, P1), 20, "graveyard {graveyard}");
        assert!(runner.state().players[1].graveyard.is_empty());
    }
}

/// Nothing happens after the game ends (CR 104.1: "A game ends immediately when a player wins").
/// Nefarious Lich ends it at P0's unblocked Hill Giant's portion, which deals its 3 to P1, whose
/// graveyard is empty. The batch's replacement choices follow APNAP order of the affected player
/// (CR 101.4), so each rider-bearing portion below affects P1 and follows the Hill Giant's in
/// assignment order; it is never dealt: P1's Outfitted Jouster wearing Steelclaw Lance and
/// Brawler's Plate, blocking, would prevent and sacrifice; P1's Ironscale Hydra under P1's
/// Hardened Scales and Doubling Season, blocking, would prevent and gain a counter. The blocked
/// attacker is dealt nothing either. P1's permanents leave the game with P1 (CR 800.4a), so the
/// Equipment's zone shows only that it was never sacrificed.
#[test]
fn nothing_happens_in_the_batch_after_the_game_ends() {
    // The Jouster's board.
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let jouster = scenario
        .add_creature_from_oracle(P1, "Outfitted Jouster", 2, 2, JOUSTER)
        .id();
    let lance = equipment_of(&mut scenario, P1, "Steelclaw Lance", STEELCLAW_LANCE);
    let plate = equipment_of(&mut scenario, P1, "Brawler's Plate", BRAWLERS_PLATE);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), lance, jouster);
    attach_to(runner.state_mut(), plate, jouster);

    let combat = fight(&mut runner, &[giant, bears], &[(jouster, bears)], None);

    assert_p1_lost_inside_the_batch(&runner, &combat.events);
    assert_nothing_after_the_game_ended(&runner, &combat.events);
    assert!(combat.prompts.is_empty());
    assert_eq!(prevented(&combat.events), 0);
    // Nothing was sacrificed: the Equipment left the game with its owner (CR 800.4a), never by
    // the Jouster's rider.
    assert!(!combat.events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: EffectKind::Sacrifice,
            ..
        }
    )));
    for equipment in [lance, plate] {
        assert_ne!(runner.state().objects[&equipment].zone, Zone::Graveyard);
    }
    assert_eq!(runner.state().objects[&bears].damage_marked, 0);

    // The Hydra's board.
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let courser = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
    let hydra = scenario
        .add_creature_from_oracle(P1, "Ironscale Hydra", 5, 5, IRONSCALE_HYDRA)
        .id();
    scales_and_season(&mut scenario, P1);
    let mut runner = scenario.build();

    let combat = fight(&mut runner, &[giant, courser], &[(hydra, courser)], None);

    assert_p1_lost_inside_the_batch(&runner, &combat.events);
    assert_nothing_after_the_game_ended(&runner, &combat.events);
    assert!(combat.prompts.is_empty());
    assert_eq!(prevented(&combat.events), 0);
    assert_eq!(plus_counters(&runner, hydra), 0);
    assert_eq!(runner.state().objects[&courser].damage_marked, 0);

    // The Hydra declared first: its 5 to the Courser is replaced before the Giant's portion
    // ends the game, and still isn't dealt.
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE_HYDRA)
        .id();
    let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    let mut runner = scenario.build();

    let combat = fight(&mut runner, &[hydra, giant], &[(courser, hydra)], None);

    assert_p1_lost_inside_the_batch(&runner, &combat.events);
    assert_nothing_after_the_game_ended(&runner, &combat.events);
    assert_eq!(runner.state().objects[&courser].damage_marked, 0);

    // A combat damage trigger: P0's Grizzly Bears, declared first, is blocked by P1's Swans of
    // Bryn Argoll, and P0 controls Sheoldred, the Apocalypse ("Whenever you draw a card, you gain
    // 2 life."). Nothing triggered by the batch is put on the stack after the game ends.
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    scenario.add_creature_from_oracle(P0, "Sheoldred, the Apocalypse", 4, 5, SHEOLDRED);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let swans = swans_of_bryn_argoll(&mut scenario);
    let mut runner = scenario.build();
    let hand_before = hand_size(&runner, P0);

    let combat = fight(&mut runner, &[bears, giant], &[(swans, bears)], None);

    assert_p1_lost_inside_the_batch(&runner, &combat.events);
    assert_nothing_after_the_game_ended(&runner, &combat.events);
    assert_eq!(life(&runner, P0), 20);
    // strict-failure: CR 615.5 runs the Swans' rider "immediately afterward", after the whole
    // damage event, which the game's end comes first in; the engine runs it in its own portion,
    // before the Giant's portion ends the game, so P0 draws 2 (riders aren't held to the end of
    // the batch, Command Zone #172).
    assert_eq!(hand_size(&runner, P0) - hand_before, 2);
}

/// CR 104.1 inside the batch's lifelink gains (CR 702.15e: each source's gain is its own event):
/// P0 controls Nefarious Lich, Laboratory Maniac and a Vampire of the Dire Moon with an empty
/// library, and P1's Vampire Nighthawk blocks the Vampire. P0's gain of 1 becomes a draw (Nefarious
/// Lich), which Laboratory Maniac replaces with P0 winning the game. Nothing in the batch happens
/// after that: P1's gain of 2 is never applied, and when P0 has cast Channel Harm at the Nighthawk,
/// the shield that prevented the Nighthawk's 2 runs no rider (CR 615.5).
#[test]
fn nothing_happens_in_the_batch_after_a_lifelink_gain_ends_the_game() {
    for channel_harm in [false, true] {
        let mut scenario = main_phase();
        let lich = scenario
            .add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH)
            .id();
        let maniac = scenario
            .add_creature_from_oracle(P0, "Laboratory Maniac", 2, 2, LABORATORY_MANIAC)
            .id();
        let attacker = creature_with_keywords(
            &mut scenario,
            P0,
            "Vampire of the Dire Moon",
            (1, 1),
            &["Deathtouch", "Lifelink"],
            VAMPIRE_OF_THE_DIRE_MOON,
        );
        let blocker = creature_with_keywords(
            &mut scenario,
            P1,
            "Vampire Nighthawk",
            (2, 3),
            &["Flying", "Deathtouch", "Lifelink"],
            VAMPIRE_NIGHTHAWK,
        );
        let harm = channel_harm.then(|| spell(&mut scenario, "Channel Harm", CHANNEL_HARM, true));
        let mut runner = scenario.build();
        assert!(runner.state().players[0].library.is_empty());
        if let Some(harm) = harm {
            cast(&mut runner, harm, vec![TargetRef::Object(blocker)]);
        }

        let combat = fight(&mut runner, &[attacker], &[(blocker, attacker)], None);

        assert_p1_lost_inside_the_batch(&runner, &combat.events);
        assert_nothing_after_the_game_ended(&runner, &combat.events);
        assert!(combat.prompts.is_empty());
        // Reach guard: the Vampire dealt its 1, and the game ended through P0's gain: Nefarious
        // Lich replaced it and Laboratory Maniac replaced the draw. The Nighthawk's 2 was dealt,
        // queuing P1's gain, or prevented by Channel Harm, leaving its rider owed.
        assert_eq!(damage_dealt_to(&combat.events, blocker), vec![1]);
        let nighthawk_damage = if channel_harm { vec![] } else { vec![2] };
        assert_eq!(
            damage_dealt_to(&combat.events, attacker),
            nighthawk_damage,
            "channel harm {channel_harm}"
        );
        let over = combat
            .events
            .iter()
            .position(|e| matches!(e, GameEvent::GameOver { .. }))
            .expect("the game ended");
        for (source, kind) in [(lich, "GainLife"), (maniac, "Draw")] {
            assert!(
                combat.events[..over].iter().any(|e| matches!(
                    e,
                    GameEvent::ReplacementApplied { source_id, event_type }
                        if *source_id == source && event_type == kind
                )),
                "{kind} was not replaced before the game ended (channel harm {channel_harm})"
            );
        }
        assert_eq!(prevented(&combat.events), 0, "channel harm {channel_harm}");
        assert_eq!(life(&runner, P0), 20);
        assert_eq!(life(&runner, P1), 20, "channel harm {channel_harm}");
    }
}

/// CR 104.1 between two of the batch's aggregate prevention riders (CR 615.5). P1 has cast
/// Deflecting Palm ("If damage is prevented this way, Deflecting Palm deals that much damage to
/// that source's controller.") choosing P0's Grizzly Bears, then Awe Strike ("The next time
/// target creature would deal damage this turn, prevent that damage. You gain life equal to the
/// damage prevented this way.") at P0's Hill Giant. Both attack unblocked and both portions are
/// prevented. Deflecting Palm's rider deals the Bears' 2 to P0, whose Nefarious Lich can't exile 2
/// cards from an empty graveyard, so P0 loses. Awe Strike's gain of 3 never happens.
#[test]
fn a_prevention_rider_that_ends_the_game_ends_the_batchs_riders() {
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let deflecting_palm = scenario
        .add_spell_to_hand_from_oracle(P1, "Deflecting Palm", true, DEFLECTING_PALM)
        .id();
    let awe_strike = scenario
        .add_spell_to_hand_from_oracle(P1, "Awe Strike", true, AWE_STRIKE)
        .id();
    let mut runner = scenario.build();

    let mut events = cast_by_p1(&mut runner, deflecting_palm);
    loop {
        drive(&mut runner, &mut Vec::new(), &mut events);
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::ChooseDamageSource { source: bears })
                .expect("P1 chooses P0's Grizzly Bears")
                .events,
        );
    }
    let mut events = cast_by_p1(&mut runner, awe_strike);
    drive(
        &mut runner,
        &mut vec![TargetRef::Object(giant)],
        &mut events,
    );
    assert_settled(&runner);

    let combat = fight(&mut runner, &[bears, giant], &[], None);

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::GameOver { winner: Some(winner) } if winner == P1),
        "got {:?}",
        state.waiting_for
    );
    assert!(state.players[0].is_eliminated);
    // Reach guard: both portions were prevented, and the game ended through Deflecting Palm's 2
    // to P0, which Nefarious Lich replaced (P0's loss above).
    assert!(damage_dealt_to_player(&combat.events, P1).is_empty());
    assert_eq!(life(&runner, P0), 20);
    // Awe Strike's rider never ran.
    assert_eq!(life(&runner, P1), 20);
    let over = combat
        .events
        .iter()
        .position(|e| matches!(e, GameEvent::GameOver { .. }))
        .expect("the game ended");
    assert!(
        !combat.events[over + 1..]
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { .. })),
        "{:?}",
        &combat.events[over + 1..]
    );
}

// ---------------------------------------------------------------------------
// A rider's own "may" keeps the prevented amount (CR 614.6 + CR 615.5 + CR 608.2d)
// ---------------------------------------------------------------------------

const SHOCKMAW_DRAGON: &str = "Flying\nWhenever this creature deals combat damage to a player, it deals 1 damage to each creature that player controls.";
const URZAS_ARMOR: &str = "If a source would deal damage to you, prevent 1 of that damage.";
const FATEFUL_SHOWDOWN: &str = "Fateful Showdown deals damage to any target equal to the number of cards in your hand. Discard all the cards in your hand, then draw that many cards.";

/// The damage `channel_harm` dealt: (recipient, amount, combat damage).
fn channel_harm_damage(
    events: &[GameEvent],
    channel_harm: ObjectId,
) -> Vec<(TargetRef, u32, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                is_combat,
                ..
            } if *source_id == channel_harm => Some((target.clone(), *amount, *is_combat)),
            _ => None,
        })
        .collect()
}

/// The positions of `player`'s draws in `events`.
fn draw_positions(events: &[GameEvent], player: PlayerId) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(
            |(_, e)| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == player),
        )
        .map(|(index, _)| index)
        .collect()
}

/// The replacement action the live optional prompt was offered in, if any.
fn offered_in(runner: &GameRunner) -> Option<ReplacementActionScope> {
    runner
        .state()
        .active_optional_effect_frame()
        .expect("the optional prompt's frame")
        .replacement_action
}

/// Answer the live optional prompt, then drive to the next stop.
fn decide(runner: &mut GameRunner, accept: bool, events: &mut Vec<GameEvent>) {
    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("answer the optional prompt")
            .events,
    );
    drive(runner, &mut Vec::new(), events);
}

/// Channel Harm: "Prevent all damage that would be dealt to you and permanents you control this
/// turn by sources you don't control. If damage is prevented this way, you may have Channel Harm
/// deal that much damage to target creature." Outside combat: P0 cast it at P1's Hill Giant, and
/// P1's Lightning Bolt ("Lightning Bolt deals 3 damage to any target.") at P0's Grizzly Bears, or
/// at P0, is prevented. The additional effect happens immediately afterward and may refer to the
/// amount prevented (CR 615.5); the "may" is P0's (CR 608.2d). So "that much" is the 3 prevented
/// whenever P0 answers, a save and load at the prompt included (CR 614.6): accepted, Channel Harm
/// deals 3 to the Giant, not combat damage, and the Giant dies; declined, it deals nothing.
#[test]
fn channel_harms_may_deals_the_prevented_amount_after_a_spell() {
    for (at_player, accept, reload) in [
        (false, true, false),
        (false, true, true),
        (false, false, false),
        (true, true, false),
    ] {
        let label = format!("Bolt at P0: {at_player}, accept: {accept}, reload: {reload}");
        let mut scenario = rider_board();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
        let channel_harm = spell(&mut scenario, "Channel Harm", CHANNEL_HARM, true);
        let bolt = scenario
            .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, BOLT)
            .id();
        let mut runner = scenario.build();
        cast(&mut runner, channel_harm, vec![TargetRef::Object(giant)]);
        let mut events = cast_by_p1(&mut runner, bolt);
        let target = if at_player {
            TargetRef::Player(P0)
        } else {
            TargetRef::Object(bears)
        };
        drive(&mut runner, &mut vec![target], &mut events);

        // Reach guard: the Bolt's 3 was prevented, and P0 is asked.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player, .. } if player == P0
            ),
            "{label}: got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(prevented(&events), 1, "{label}");
        let action = Some(ReplacementActionScope { amount: 3 });
        assert_eq!(offered_in(&runner), action, "{label}");
        if reload {
            let json = serde_json::to_string(runner.state()).expect("serialize at the prompt");
            *runner.state_mut() = serde_json::from_str::<GameState>(&json).expect("deserialize");
            assert_eq!(offered_in(&runner), action, "{label}");
        }
        decide(&mut runner, accept, &mut events);

        assert_settled(&runner);
        assert!(runner.state().resolution_stack.is_empty(), "{label}");
        let harm = channel_harm_damage(&events, channel_harm);
        if accept {
            assert_eq!(harm, vec![(TargetRef::Object(giant), 3, false)], "{label}");
            assert_eq!(
                runner.state().objects[&giant].zone,
                Zone::Graveyard,
                "{label}"
            );
        } else {
            assert!(harm.is_empty(), "{label}: {harm:?}");
            assert_eq!(
                runner.state().objects[&giant].zone,
                Zone::Battlefield,
                "{label}"
            );
            assert_eq!(runner.state().objects[&giant].damage_marked, 0, "{label}");
        }
        assert_eq!(
            runner.state().objects[&bears].zone,
            Zone::Battlefield,
            "{label}"
        );
        assert_eq!(runner.state().objects[&bears].damage_marked, 0, "{label}");
        assert_eq!(life(&runner, P0), 20, "{label}");
    }
}

/// Inside a resolving trigger. Shockmaw Dragon: "Whenever this creature deals combat damage to a
/// player, it deals 1 damage to each creature that player controls." It deals 4 combat damage to
/// P1, and with its trigger on the stack P1 casts Channel Harm at the Dragon. The trigger's 1 to
/// P1's Grizzly Bears is prevented, and Channel Harm's "that much" is that 1 (CR 615.5), not the
/// 4 of the event the trigger resolves for (CR 603.2, CR 614.6): accepted, the Dragon is dealt 1
/// and survives; declined, it is dealt nothing.
#[test]
fn channel_harms_may_reads_the_prevented_amount_inside_a_resolving_trigger() {
    for accept in [true, false] {
        let mut scenario = rider_board();
        let dragon = creature_with_keywords(
            &mut scenario,
            P0,
            "Shockmaw Dragon",
            (4, 4),
            &["Flying"],
            SHOCKMAW_DRAGON,
        );
        let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let channel_harm = scenario
            .add_spell_to_hand_from_oracle(P1, "Channel Harm", true, CHANNEL_HARM)
            .id();
        let mut runner = scenario.build();
        let mut events = Vec::new();
        let (mut attacked, mut cast_harm, mut asked) = (false, false, false);
        for _ in 0..400 {
            let state = runner.state();
            if asked
                && state.phase == Phase::EndCombat
                && state.stack.is_empty()
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match &state.waiting_for {
                WaitingFor::DeclareAttackers { .. } => {
                    let attacks = if attacked {
                        Vec::new()
                    } else {
                        vec![(dragon, AttackTarget::Player(P1))]
                    };
                    attacked = true;
                    GameAction::DeclareAttackers {
                        attacks,
                        bands: vec![],
                    }
                }
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: Vec::new(),
                },
                // CR 117.4: the trigger waits on the stack until both players pass in
                // succession, so P1 gets priority with it still there.
                WaitingFor::Priority { player }
                    if *player == P1 && !cast_harm && !state.stack.is_empty() =>
                {
                    assert_eq!(state.phase, Phase::CombatDamage);
                    assert_eq!(state.stack.len(), 1, "Shockmaw Dragon's trigger");
                    assert_eq!(life(&runner, P1), 16, "the combat damage was dealt");
                    cast_harm = true;
                    GameAction::CastSpell {
                        object_id: channel_harm,
                        card_id: state.objects[&channel_harm].card_id,
                        targets: vec![],
                        payment_mode: CastPaymentMode::Auto,
                    }
                }
                WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                    GameAction::PassPriority
                }
                WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(dragon)),
                },
                WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
                WaitingFor::OptionalEffectChoice { player, .. } => {
                    assert!(!asked, "Channel Harm asks once");
                    assert_eq!(*player, P1);
                    assert!(cast_harm);
                    assert_eq!(life(&runner, P1), 16);
                    assert_eq!(prevented(&events), 1);
                    assert_eq!(
                        offered_in(&runner),
                        Some(ReplacementActionScope { amount: 1 })
                    );
                    asked = true;
                    GameAction::DecideOptionalEffect { accept }
                }
                other => panic!("an unexpected prompt: {other:?}"),
            };
            events.extend(runner.act(action).expect("combat action").events);
        }

        assert!(asked, "P1 was asked");
        assert_settled(&runner);
        assert_eq!(runner.state().phase, Phase::EndCombat);
        assert!(runner.state().pending_combat_lifelink.is_none());
        assert!(runner.state().resolution_stack.is_empty());
        let harm = channel_harm_damage(&events, channel_harm);
        let dragon_state = &runner.state().objects[&dragon];
        if accept {
            assert_eq!(harm, vec![(TargetRef::Object(dragon), 1, false)]);
            assert_eq!(dragon_state.zone, Zone::Battlefield);
            assert_eq!(dragon_state.damage_marked, 1);
        } else {
            assert!(harm.is_empty(), "{harm:?}");
            assert_eq!(dragon_state.zone, Zone::Battlefield);
            assert_eq!(dragon_state.damage_marked, 0);
        }
        assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&bears].damage_marked, 0);
    }
}

/// Channel Harm's ruling: two Channel Harms make "a loop that either player can break by choosing
/// to not have Channel Harm try to deal damage", and "no damage will be dealt in either case". P0's
/// Channel Harm is at P1's Hill Giant, P1's at P0's Grizzly Bears, and P1's Lightning Bolt at P0's
/// Centaur Courser. P0's prevents the Bolt's 3 and asks P0; accepted, its 3 to the Giant is
/// prevented by P1's, which asks P1; accepted, its 3 to the Bears is prevented by P0's, which asks
/// P0 again (CR 615.5, CR 608.2d). Each prompt's "that much" is 3, and the first decline ends it.
#[test]
fn two_channel_harms_loop_until_a_player_declines() {
    for answers in [vec![true, true, false], vec![true, false], vec![false]] {
        let label = format!("{answers:?}");
        let mut scenario = rider_board();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let courser = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
        let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
        let p0_harm = spell(&mut scenario, "Channel Harm", CHANNEL_HARM, true);
        let p1_harm = scenario
            .add_spell_to_hand_from_oracle(P1, "Channel Harm", true, CHANNEL_HARM)
            .id();
        let bolt = scenario
            .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, BOLT)
            .id();
        let mut runner = scenario.build();
        cast(&mut runner, p0_harm, vec![TargetRef::Object(giant)]);
        let mut events = cast_by_p1(&mut runner, p1_harm);
        drive(
            &mut runner,
            &mut vec![TargetRef::Object(bears)],
            &mut events,
        );
        assert_settled(&runner);
        events.extend(cast_by_p1(&mut runner, bolt));
        drive(
            &mut runner,
            &mut vec![TargetRef::Object(courser)],
            &mut events,
        );

        let mut choosers = Vec::new();
        for &accept in &answers {
            let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
                panic!(
                    "{label}: prompt {} is not asked: {:?}",
                    choosers.len() + 1,
                    runner.state().waiting_for
                );
            };
            choosers.push(player);
            assert_eq!(
                offered_in(&runner),
                Some(ReplacementActionScope { amount: 3 }),
                "{label}"
            );
            decide(&mut runner, accept, &mut events);
        }

        assert_eq!(choosers, [P0, P1, P0][..answers.len()].to_vec(), "{label}");
        assert_settled(&runner);
        assert!(runner.state().resolution_stack.is_empty(), "{label}");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::DamageDealt { .. })),
            "{label}: no damage is dealt"
        );
        for id in [bears, courser, giant] {
            assert_eq!(
                runner.state().objects[&id].zone,
                Zone::Battlefield,
                "{label}"
            );
            assert_eq!(runner.state().objects[&id].damage_marked, 0, "{label}");
        }
    }
}

/// CR 615.5 + CR 608.2c + CR 616.1f: a rider's "may" resolves inside its action, and the spell
/// resolving around it keeps its own amount. P1's Fateful Showdown ("deals damage to any target
/// equal to the number of cards in your hand. Discard all the cards in your hand, then draw that
/// many cards.") at P0 with five other cards in hand deals 5 (its ruling: Fateful Showdown "is on
/// the stack at this time, so it's not counted"). P0's Urza's Armor ("If a source would deal
/// damage to you, prevent 1 of that damage.") and P0's Channel Harm, cast at P1's Hill Giant, both
/// apply, so P0 orders them (CR 616.1). Armor first prevents 1 and Channel Harm the other 4, so its
/// "that much" is 4; Channel Harm first prevents all 5. Its "may" happens immediately afterward,
/// before the spell resumes (CR 615.5), and then P1 discards five and draws "that many", the five
/// discarded, never the rider's 4.
#[test]
fn a_riders_may_amount_does_not_reach_the_spells_tail() {
    for (armor_first, accept, rider) in [(true, true, 4), (true, false, 4), (false, true, 5)] {
        let label = format!("Armor first: {armor_first}, accept: {accept}");
        let mut scenario = rider_board();
        let armor = scenario
            .add_artifact_from_oracle(P0, "Urza's Armor", URZAS_ARMOR)
            .id();
        let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
        let channel_harm = spell(&mut scenario, "Channel Harm", CHANNEL_HARM, true);
        let showdown = scenario
            .add_spell_to_hand_from_oracle(P1, "Fateful Showdown", true, FATEFUL_SHOWDOWN)
            .id();
        let held: Vec<ObjectId> = (0..5)
            .map(|index| scenario.add_card_to_hand(P1, &format!("Held {index}")))
            .collect();
        let mut runner = scenario.build();
        cast(&mut runner, channel_harm, vec![TargetRef::Object(giant)]);
        let mut events = cast_by_p1(&mut runner, showdown);
        drive(&mut runner, &mut vec![TargetRef::Player(P0)], &mut events);
        let pick = if armor_first {
            Pick::From(armor)
        } else {
            Pick::NotFrom(armor)
        };
        let order = answer_one(&mut runner, pick, giant, &mut events);
        assert_eq!(order.sources.len(), 2, "{label}: {:?}", order.sources);
        assert!(order.sources.contains(&armor), "{label}");

        // Reach guard: P0 is asked, and the spell's tail waits beneath the prompt.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player, .. } if player == P0
            ),
            "{label}: got {:?}",
            runner.state().waiting_for
        );
        let p1_hand = &runner.state().players[1].hand;
        assert!(held.iter().all(|id| p1_hand.contains(id)), "{label}");
        assert!(draw_positions(&events, P1).is_empty(), "{label}");
        assert_eq!(life(&runner, P0), 20, "{label}");
        assert_eq!(
            offered_in(&runner),
            Some(ReplacementActionScope {
                amount: i32::try_from(rider).expect("a small amount")
            }),
            "{label}"
        );
        decide(&mut runner, accept, &mut events);

        assert_settled(&runner);
        assert!(runner.state().resolution_stack.is_empty(), "{label}");
        let harm = channel_harm_damage(&events, channel_harm);
        let drawn = draw_positions(&events, P1);
        assert_eq!(drawn.len(), 5, "{label}: P1 draws the five discarded");
        if accept {
            assert_eq!(
                harm,
                vec![(TargetRef::Object(giant), rider, false)],
                "{label}"
            );
            assert_eq!(
                runner.state().objects[&giant].zone,
                Zone::Graveyard,
                "{label}"
            );
            let dealt = events
                .iter()
                .position(|e| {
                    matches!(e, GameEvent::DamageDealt { source_id, .. } if *source_id == channel_harm)
                })
                .expect("Channel Harm dealt damage");
            assert!(
                drawn.iter().all(|&index| index > dealt),
                "{label}: the spell's tail follows the rider"
            );
        } else {
            assert!(harm.is_empty(), "{label}: {harm:?}");
            assert_eq!(
                runner.state().objects[&giant].zone,
                Zone::Battlefield,
                "{label}"
            );
            assert_eq!(runner.state().objects[&giant].damage_marked, 0, "{label}");
        }
        assert_eq!(hand_size(&runner, P1), 5, "{label}");
        let p1_graveyard = &runner.state().players[1].graveyard;
        assert!(held.iter().all(|id| p1_graveyard.contains(id)), "{label}");
        assert_eq!(life(&runner, P0), 20, "{label}");
    }
}

/// P0 passes priority, and P1 casts `spell`; the caller answers its prompts.
fn cast_by_p1(runner: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0) {
        runner
            .act(GameAction::PassPriority)
            .expect("P0 passes priority to P1");
    }
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts the spell")
        .events
}

/// No damage, prevention, counter, life change or zone change follows the `GameOver` event, no
/// ability is put on the stack after it, and the stack is empty.
fn assert_nothing_after_the_game_ended(runner: &GameRunner, events: &[GameEvent]) {
    let over = events
        .iter()
        .position(|e| matches!(e, GameEvent::GameOver { .. }))
        .expect("the game ended");
    let after: Vec<&GameEvent> = events[over + 1..]
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::DamageDealt { .. }
                    | GameEvent::DamagePrevented { .. }
                    | GameEvent::CounterAdded { .. }
                    | GameEvent::LifeChanged { .. }
                    | GameEvent::ZoneChanged { .. }
                    | GameEvent::ReplacementApplied { .. }
                    | GameEvent::StackPushed { .. }
            )
        })
        .collect();
    assert!(after.is_empty(), "after the game ended: {after:?}");
    assert!(
        runner.state().stack.is_empty(),
        "{:?}",
        runner.state().stack
    );
}

/// CR 104.1 outside combat: Flame Rift ("Flame Rift deals 4 damage to each player.") cast by P0,
/// who controls Nefarious Lich with an empty graveyard. P0's portion ends the game, and P1's 4 is
/// never dealt.
#[test]
fn nothing_happens_after_a_noncombat_portion_ends_the_game() {
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH);
    let rift = spell(&mut scenario, "Flame Rift", FLAME_RIFT, false);
    let mut runner = scenario.build();

    let events = cast(&mut runner, rift, Vec::new());

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::GameOver { winner: Some(winner) } if winner == P1),
        "got {:?}",
        state.waiting_for
    );
    assert!(state.players[0].is_eliminated);
    // CR 104.3e: P0 loses because the effect says so.
    let losses: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerLost { player_id, reasons } => Some((*player_id, reasons.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        losses,
        vec![(
            P0,
            std::collections::BTreeSet::from([engine::types::events::LossReason::EffectStatedLoss])
        )]
    );
    assert_nothing_after_the_game_ended(&runner, &events);
    assert!(damage_dealt_to_player(&events, P1).is_empty());
    assert_eq!(life(&runner, P1), 20);
}

fn damage_dealt_to_player(events: &[GameEvent], player: PlayerId) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamageDealt {
                target: TargetRef::Player(target),
                amount,
                ..
            } if *target == player => Some(*amount),
            _ => None,
        })
        .collect()
}

/// CR 800.4a: with three players the loss isn't the game's end. P1 leaves inside the batch, and
/// the rest of the batch still happens for the players left: P2 takes the Grizzly Bears' 2, and
/// Ironscale Hydra, blocked by P2's Centaur Courser, prevents its damage and gains its counter.
/// A second Grizzly Bears attacking P1 after the Hill Giant has no recipient left (CR 800.4a,
/// CR 800.4e): P1 isn't dealt its 2, so P0's Exquisite Blood ("Whenever an opponent loses life,
/// you gain that much life.") sees only P2's loss. The Hydra's counter meets P0's Hardened Scales
/// and Doubling Season, an order P0 is asked (CR 616.1): Hardened Scales first 4, Doubling Season
/// first 3.
#[test]
fn a_player_who_loses_inside_the_batch_leaves_and_the_batch_goes_on() {
    for scales_first in [true, false] {
        a_player_who_loses_inside_the_batch_leaves_with(scales_first);
    }
}

fn a_player_who_loses_inside_the_batch_leaves_with(scales_first: bool) {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, p2] {
        for index in 0..8 {
            scenario.add_spell_to_library_top(player, &format!("Library Filler {index}"), true);
        }
    }
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    scenario.add_enchantment_from_oracle(P0, "Exquisite Blood", EXQUISITE_BLOOD);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let second_bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE_HYDRA)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let (scales, season) = scales_and_season(&mut scenario, P0);
    let courser = scenario.add_creature(p2, "Centaur Courser", 3, 3).id();
    let mut runner = scenario.build();
    let first = if scales_first { scales } else { season };
    let mut prompts = Vec::new();

    let attacks = [
        (giant, AttackTarget::Player(P1)),
        (second_bears, AttackTarget::Player(P1)),
        (hydra, AttackTarget::Player(p2)),
        (bears, AttackTarget::Player(p2)),
    ];
    let events = combat_at(&mut runner, &attacks, p2, &[(courser, hydra)], |state| {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &state.waiting_for
        else {
            unreachable!("combat_at answers only a ReplacementChoice here");
        };
        prompts.push(*player);
        let index = candidates
            .iter()
            .position(|c| c.source_id == first)
            .expect("the counter order offers both");
        GameAction::ChooseReplacement { index }
    });

    let state = runner.state();
    assert!(state.players[1].is_eliminated);
    assert_eq!(state.game_end, None);
    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::GameOver { .. })));
    assert_eq!(state.players[2].life, 18);
    assert!(damage_dealt_to_player(&events, P1).is_empty());
    assert_eq!(state.players[1].life, 20);
    assert_eq!(state.players[0].life, 22);
    assert_eq!(prevented(&events), 1);
    assert_eq!(prompts, vec![P0]);
    assert_eq!(
        plus_counters(&runner, hydra),
        if scales_first { 4 } else { 3 }
    );
    assert_eq!(state.objects[&courser].zone, Zone::Graveyard);
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
    assert_eq!(state.phase, Phase::EndCombat);
}

/// Drive one combat with per-attacker targets: `defender` blocks with `blocks`, every other
/// defending player blocks nothing. A replacement prompt is answered by `answer`.
fn combat_at(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
    defender: PlayerId,
    blocks: &[(ObjectId, ObjectId)],
    mut answer: impl FnMut(&GameState) -> GameAction,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut attacked = false;
    for _ in 0..400 {
        let state = runner.state();
        if matches!(state.phase, Phase::EndCombat | Phase::PostCombatMain) {
            return events;
        }
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::DeclareAttackers { .. } => {
                let attacks = if attacked {
                    Vec::new()
                } else {
                    attacks.to_vec()
                };
                attacked = true;
                GameAction::DeclareAttackers {
                    attacks,
                    bands: vec![],
                }
            }
            WaitingFor::DeclareBlockers { player, .. } => GameAction::DeclareBlockers {
                assignments: if *player == defender {
                    blocks.to_vec()
                } else {
                    Vec::new()
                },
            },
            WaitingFor::ReplacementChoice { .. } => answer(state),
            other => panic!("combat stopped at {other:?}"),
        };
        events.extend(runner.act(action).expect("combat action").events);
    }
    panic!("combat did not end");
}

/// Dralnu, Lich Lord: "If damage would be dealt to Dralnu, sacrifice that many permanents
/// instead." Blocking a 2/2 with four other permanents under its controller: the 2 damage is
/// replaced by sacrificing 2 permanents, its controller's choice (CR 608.2d, CR 701.21a; Dralnu
/// is one of the permanents it may pick, ruling S11), asked inside the combat batch. P1 picks two
/// Grizzly Bears, exactly those two are sacrificed, Dralnu is dealt no damage, and its 3 kills
/// P0's Grizzly Bears.
#[test]
fn dralnus_sacrifice_choice_is_asked_inside_the_batch() {
    let mut scenario = rider_board();
    let dralnu = scenario
        .add_creature_from_oracle(P1, "Dralnu, Lich Lord", 3, 3, DRALNU_LICH_LORD)
        .id();
    let others: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
        .collect();
    let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
    let mut runner = scenario.build();
    let picked = vec![others[0], others[2]];
    let mut asked = Vec::new();

    let combat = fight_answering(
        &mut runner,
        &[bears],
        &[(dralnu, bears)],
        |state| match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                asked.push((*player, *count, cards.contains(&dralnu)));
                GameAction::SelectCards {
                    cards: picked.clone(),
                }
            }
            other => panic!("an unexpected prompt: {other:?}"),
        },
    );

    assert_batch_settled(&runner);
    assert!(combat.prompts.is_empty());
    assert_eq!(asked, vec![(P1, 2, true)]);
    // Reach guard: the substitution applied and Dralnu dealt its damage.
    assert_eq!(runner.state().objects[&dralnu].damage_marked, 0);
    assert_eq!(runner.state().objects[&dralnu].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    for &other in &picked {
        assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
    }
    assert_eq!(runner.state().objects[&others[1]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Battlefield);

    bolt_at(&mut runner, bolt, courser);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
}

/// Outfitted Jouster attacking, blocked by one or two 2/2s. CR 615.13 + CR 608.2d: one
/// prevention, one trigger, one Equipment of P0's choice; the double block is
/// `outfitted_jouster_double_blocked_sacrifices_one_equipment`. With one Equipment attached, the
/// Steelclaw Lance is sacrificed with no prompt, whether one or two creatures block. With two
/// attached and one blocker, P0 is offered exactly the two and the one picked is sacrificed; the
/// other stays attached (CR 701.21a).
#[test]
fn outfitted_jousters_sacrifice_in_combat() {
    for (equipped, blockers) in [(1usize, 1usize), (1, 2), (2, 1)] {
        let picks: Vec<Option<usize>> = if equipped == 2 {
            vec![Some(0), Some(1)]
        } else {
            vec![None]
        };
        for pick in picks {
            let mut scenario = rider_board();
            let jouster = scenario
                .add_creature_from_oracle(P0, "Outfitted Jouster", 2, 2, JOUSTER)
                .id();
            let mut attached = vec![equipment_of(
                &mut scenario,
                P0,
                "Steelclaw Lance",
                STEELCLAW_LANCE,
            )];
            if equipped == 2 {
                attached.push(equipment_of(
                    &mut scenario,
                    P0,
                    "Brawler's Plate",
                    BRAWLERS_PLATE,
                ));
            }
            let blocking: Vec<ObjectId> = (0..blockers)
                .map(|_| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
                .collect();
            let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
            let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
            let mut runner = scenario.build();
            for &equipment in &attached {
                attach_to(runner.state_mut(), equipment, jouster);
            }
            let blocks: Vec<(ObjectId, ObjectId)> =
                blocking.iter().map(|&b| (b, jouster)).collect();
            let chosen = pick.map(|index| attached[index]);
            let mut offers = Vec::new();

            let combat = fight_answering(&mut runner, &[jouster], &blocks, |state| {
                match &state.waiting_for {
                    WaitingFor::EffectZoneChoice {
                        player,
                        cards,
                        count,
                        ..
                    } => {
                        let mut offer = cards.clone();
                        offer.sort();
                        offers.push((*player, *count, offer));
                        GameAction::SelectCards {
                            cards: vec![chosen.expect("a pick is asked only with two attached")],
                        }
                    }
                    other => panic!("an unexpected prompt: {other:?}"),
                }
            });

            let row = format!("{equipped} equipped, {blockers} blocking, pick {pick:?}");
            assert_batch_settled(&runner);
            assert!(combat.prompts.is_empty(), "{row}");
            assert_eq!(prevented(&combat.events), 1, "{row}");
            assert_eq!(runner.state().objects[&jouster].damage_marked, 0, "{row}");
            for &b in &blocking {
                assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard, "{row}");
            }
            match chosen {
                None => {
                    assert!(offers.is_empty(), "{row}");
                    assert_eq!(
                        runner.state().objects[&attached[0]].zone,
                        Zone::Graveyard,
                        "{row}"
                    );
                }
                Some(sacrificed) => {
                    let mut both = attached.clone();
                    both.sort();
                    assert_eq!(offers, vec![(P0, 1, both)], "{row}");
                    for &equipment in &attached {
                        let object = &runner.state().objects[&equipment];
                        if equipment == sacrificed {
                            assert_eq!(object.zone, Zone::Graveyard, "{row}");
                        } else {
                            assert_eq!(object.zone, Zone::Battlefield, "{row}");
                            assert_eq!(
                                object.attached_to,
                                Some(AttachTarget::Object(jouster)),
                                "{row}"
                            );
                        }
                    }
                }
            }

            bolt_at(&mut runner, bolt, courser);
            assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
        }
    }
}

/// Outfitted Jouster: "If damage would be dealt to Outfitted Jouster while it's equipped, prevent
/// that damage. When damage is prevented this way, sacrifice an Equipment attached to Outfitted
/// Jouster." Wearing Steelclaw Lance and Brawler's Plate, it attacks and two Grizzly Bears block
/// it. Derived reading:
/// 1. The two Bears' damage to the Jouster is dealt simultaneously (CR 510.2); the Jouster is
///    equipped, so the one prevention effect prevents both events.
/// 2. "When damage is prevented this way" triggers each time a prevention effect is applied to
///    one or more simultaneous damage events (CR 615.13): once here, not once per Bear
///    (CR 603.2c).
/// 3. The trigger does nothing when it triggers (CR 603.2); it goes on the stack before the
///    active player gets priority in the combat damage step (CR 603.3, CR 510.3a).
/// 4. As it resolves, P0 chooses which attached Equipment to sacrifice (CR 608.2d) and sacrifices
///    that one (CR 701.21a); the other stays attached.
///
/// A verified red witness, not run by default: the engine runs the rider once per damage event,
/// inside the batch.
#[test]
#[ignore = "CR 615.13: one prevention applied to simultaneous combat damage triggers once; the engine runs Outfitted Jouster's rider once per damage event (known gap: one rider per simultaneous batch for recipient-phrased prevention effects)"]
fn outfitted_jouster_double_blocked_sacrifices_one_equipment() {
    for pick in [0usize, 1] {
        let mut scenario = rider_board();
        let jouster = scenario
            .add_creature_from_oracle(P0, "Outfitted Jouster", 2, 2, JOUSTER)
            .id();
        let attached = [
            equipment_of(&mut scenario, P0, "Steelclaw Lance", STEELCLAW_LANCE),
            equipment_of(&mut scenario, P0, "Brawler's Plate", BRAWLERS_PLATE),
        ];
        let blocking = [
            scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
            scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
        ];
        let mut runner = scenario.build();
        for &equipment in &attached {
            attach_to(runner.state_mut(), equipment, jouster);
        }

        // Combat until the first priority in the combat damage step, or the first other prompt.
        let mut events = Vec::new();
        let (mut attacked, mut blocked) = (false, false);
        for _ in 0..200 {
            let state = runner.state();
            if state.phase == Phase::CombatDamage
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match &state.waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } if !attacked => {
                    attacked = true;
                    GameAction::DeclareAttackers {
                        attacks: vec![(jouster, AttackTarget::Player(P1))],
                        bands: vec![],
                    }
                }
                WaitingFor::DeclareBlockers { .. } if !blocked => {
                    blocked = true;
                    GameAction::DeclareBlockers {
                        assignments: blocking.iter().map(|&b| (b, jouster)).collect(),
                    }
                }
                WaitingFor::AssignCombatDamage {
                    blockers,
                    total_damage,
                    ..
                } => {
                    // CR 510.1c: lethal damage to each blocker, the rest to the last one.
                    let mut remaining = *total_damage;
                    let mut assignments: Vec<(ObjectId, u32)> = blockers
                        .iter()
                        .map(|slot| {
                            let assign = remaining.min(slot.lethal_minimum);
                            remaining -= assign;
                            (slot.blocker_id, assign)
                        })
                        .collect();
                    if let Some(last) = assignments.last_mut() {
                        last.1 += remaining;
                    }
                    GameAction::AssignCombatDamage {
                        mode: CombatDamageAssignmentMode::Normal,
                        assignments,
                        trample_damage: 0,
                        controller_damage: 0,
                    }
                }
                _ => break,
            };
            events.extend(runner.act(action).expect("combat action").events);
        }

        let state = runner.state();
        // 1. No choice is asked while the damage is dealt.
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "the sacrifice was asked inside the combat damage batch"
        );
        // 2. One trigger, the Jouster's, on the stack at P0's priority in the damage step.
        assert_eq!(state.phase, Phase::CombatDamage);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.stack[0].source_id, jouster);
        // 3. Both Equipment are still attached.
        for &equipment in &attached {
            assert_eq!(state.objects[&equipment].zone, Zone::Battlefield);
            assert_eq!(
                state.objects[&equipment].attached_to,
                Some(AttachTarget::Object(jouster))
            );
        }
        // 4. Reach-guard: the prevention applied.
        assert_eq!(state.objects[&jouster].damage_marked, 0);
        for &b in &blocking {
            assert_eq!(state.objects[&b].zone, Zone::Graveyard);
        }
        assert_eq!(prevented(&events), 1);

        // The trigger resolves: P0 picks one of the two.
        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority")
                    .events,
            );
        }
        let WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected the sacrifice choice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((player, count), (P0, 1));
        let mut offered = cards.clone();
        offered.sort();
        let mut both = attached.to_vec();
        both.sort();
        assert_eq!(offered, both);
        runner
            .act(GameAction::SelectCards {
                cards: vec![attached[pick]],
            })
            .expect("pick the Equipment");
        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                || runner.state().stack.is_empty()
            {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass priority");
        }

        let state = runner.state();
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ));
        assert_eq!(state.objects[&attached[pick]].zone, Zone::Graveyard);
        let kept = attached[1 - pick];
        assert_eq!(state.objects[&kept].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&kept].attached_to,
            Some(AttachTarget::Object(jouster))
        );
        assert!(state.stack.is_empty());
        assert!(state.pending_replacement.is_none());
        assert!(state.resolution_stack.is_empty());
    }
}
