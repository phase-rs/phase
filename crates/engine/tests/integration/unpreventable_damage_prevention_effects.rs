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
//! reading.
//!
//! A rider's pause inside a combat damage batch is settled before the batch moves on, at the
//! per-portion drain and at the aggregate drain after it (CR 510.2). Combat damage can't pause for
//! a rider's choice yet (Command Zone #140), so an order the rider's event meets takes one legal
//! order (CR 616.1, CR 616.1f), a "may" on it is declined, and the rider's own choice is cleared.
//! The last section holds those rows; the Ironscale Hydra rows are in
//! `ironscale_hydra_combat_prevention`.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{DamageGuardReading, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CombatDamageAssignmentMode, GameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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

/// CR 608.2f: one spell's damage from several sources to one creature is one simultaneous batch,
/// so the guard is read once at each noncombat resolver: Band Together ("Up to two target
/// creatures you control each deal damage equal to their power to another target creature."),
/// Moonlight Hunt and Coordinated Clobbering.
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
    assert_eq!(prevented(&events), 2, "Band Together");

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
    assert_eq!(prevented(&events), 2, "Moonlight Hunt");

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
    assert_eq!(prevented(&events), 2, "Coordinated Clobbering");
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
    assert_eq!(prevented(&events), 2);

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
/// portion parks, the shield takes the counter, and the second portion is still offered the shield
/// with no counter left, in every order of answers.
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
        assert_eq!(prevented(&events), 2, "{picks:?}");
        assert_eq!(prompts.len(), 2, "{picks:?}: {prompts:?}");
        assert_eq!(prompts[1].watched_counters, 0, "{picks:?}");
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
    assert_eq!(prevented(&events), 2);
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
    assert_eq!(prevented(&events), 3, "Moonlight Hunt");

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
    assert_eq!(prevented(&events), 2, "Coordinated Clobbering");
}

/// The Champion's printed shield and Acolyte's Reward's resolved shield, as the prompt names them.
const CHAMPION_SHIELD: &str = "If damage would be dealt to ~ while it has a +1/+1 counter on it, prevent that damage and remove a +1/+1 counter from ~.";
const ACOLYTES_REWARD_SHIELD: &str = "Prevent damage";

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
    assert_eq!(prevented(&events), 2);
    assert!(damage_dealt_to(&events, champion).is_empty());
    assert_shielded_once(&runner, champion);
}

/// CR 701.14a + CR 616.1: a fight whose first direction parks on an order choice resumes its second
/// direction under the fight's reading. Pit Fight with Nomads en-Kor fighting the Champion under
/// Furnace of Rath, the Nomads' shield aimed at the Champion. The Nomads' 1 damage to the Champion
/// meets Furnace of Rath and the Champion's shield and is answered shield first, which takes the
/// counter; the Champion's damage back is stashed behind that choice, and the 1 of it the Nomads'
/// shield then deals to the counterless Champion is still prevented.
#[test]
fn a_paused_fight_keeps_its_guard_reading() {
    let mut scenario = main_phase();
    let champion = undergrowth_champion(&mut scenario, P0, 1);
    let nomads = nomads_shielding(&mut scenario, "Nomads en-Kor");
    furnace_of_rath(&mut scenario);
    let pit_fight = spell(&mut scenario, "Pit Fight", PIT_FIGHT, true);
    let mut runner = scenario.build();
    activate_shields_at(&mut runner, &[nomads], champion);

    let mut events = cast(
        &mut runner,
        pit_fight,
        vec![TargetRef::Object(nomads), TargetRef::Object(champion)],
    );
    let prompts = answer(&mut runner, &[Pick::From(champion)], champion, &mut events);

    assert_settled(&runner);
    assert!(
        !prompts.is_empty(),
        "the first direction parks on an order choice"
    );
    assert_eq!(prompts[0].watched_counters, 1, "{prompts:?}");
    assert!(prompts[0].sources.contains(&champion), "{prompts:?}");
    // The rest of the Champion's damage stays on the Nomads, which dies.
    assert!(!damage_dealt_to(&events, nomads).is_empty());
    assert_eq!(runner.state().objects[&nomads].zone, Zone::Graveyard);
    assert_eq!(prevented(&events), 2);
    assert!(damage_dealt_to(&events, champion).is_empty());
    assert_shielded_once(&runner, champion);
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
    assert_eq!(prevented(&events), 2);
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
/// whose else clause is the run's second clause, carries the reading in the paused continuation.
/// (After the pause the else clause's damage goes to the planeswalker, at base too; that is Command Zone #175,
/// so this row asserts the carrier, not the second clause's recipient.)
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
            readings(&forwarded.context.settled_damage_guards),
            vec![(champion, DamageGuardReading::Holds)]
        );
        let else_clause = forwarded.else_ability.as_deref().expect("the else clause");
        assert_eq!(
            readings(&else_clause.context.settled_damage_guards),
            vec![(champion, DamageGuardReading::Holds)]
        );

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
        assert_shielded_once(&runner, champion);
        assert_eq!(runner.state().players[1].life, 20);
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
const NEFARIOUS_LICH: &str = "If damage would be dealt to you, exile that many cards from your graveyard instead. If you can't, you lose the game.\nIf you would gain life, draw that many cards instead.\nWhen this enchantment leaves the battlefield, you lose the game.";
const DRALNU_LICH_LORD: &str = "If damage would be dealt to Dralnu, sacrifice that many permanents instead.\n{T}: Target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
const BOROS_SWIFTBLADE: &str = "Double strike";

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
/// order its controller chooses (CR 616.1).
pub(super) fn scales_and_season(scenario: &mut GameScenario, owner: PlayerId) {
    scenario.add_enchantment_from_oracle(owner, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(owner, "Doubling Season", DOUBLING_SEASON);
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
/// answered.
pub(super) struct Combat {
    pub(super) events: Vec<GameEvent>,
    pub(super) prompts: Vec<Vec<ObjectId>>,
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
    let mut combat = Combat {
        events: Vec::new(),
        prompts: Vec::new(),
    };
    let (mut attacked, mut blocked) = (false, false);
    for _ in 0..400 {
        let state = runner.state();
        if matches!(state.phase, Phase::EndCombat | Phase::PostCombatMain)
            || matches!(state.waiting_for, WaitingFor::GameOver { .. })
        {
            return combat;
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
                    attackers
                        .iter()
                        .map(|&attacker| (attacker, AttackTarget::Player(P1)))
                        .collect()
                };
                attacked = true;
                GameAction::DeclareAttackers {
                    attacks,
                    bands: vec![],
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                let assignments = if blocked { Vec::new() } else { blocks.to_vec() };
                blocked = true;
                GameAction::DeclareBlockers { assignments }
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
                GameAction::AssignCombatDamage {
                    mode: CombatDamageAssignmentMode::Normal,
                    assignments,
                    trample_damage: 0,
                    controller_damage: 0,
                }
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let sources: Vec<ObjectId> = candidates.iter().map(|c| c.source_id).collect();
                let index = prefer
                    .and_then(|preferred| sources.iter().position(|&s| s == preferred))
                    .unwrap_or_else(|| panic!("an unexpected replacement prompt: {sources:?}"));
                combat.prompts.push(sources);
                GameAction::ChooseReplacement { index }
            }
            other => panic!("combat stopped at {other:?}"),
        };
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

/// CR 616.1 at the per-portion drain, on a substitution's rider. Phytohydra: "If damage would be
/// dealt to this creature, put that many +1/+1 counters on it instead." Blocking a 2/2 under its
/// controller's Hardened Scales and Doubling Season, its 2 counters meet both: 5 (Doubling Season
/// first) or 6 (Hardened Scales first), one legal order. Nothing outlives the batch, and the
/// state-based actions after it run (Lightning Bolt kills a 3/3).
#[test]
fn phytohydras_counters_take_one_legal_order_inside_the_batch() {
    for scales in [true, false] {
        let mut scenario = rider_board();
        let hydra = scenario
            .add_creature_from_oracle(P1, "Phytohydra", 1, 1, PHYTOHYDRA)
            .id();
        if scales {
            scales_and_season(&mut scenario, P1);
        }
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
        let mut runner = scenario.build();

        let combat = fight(&mut runner, &[bears], &[(hydra, bears)], None);

        assert_batch_settled(&runner);
        assert!(combat.prompts.is_empty());
        let counters = plus_counters(&runner, hydra);
        if scales {
            assert!([5, 6].contains(&counters), "got {counters} counters");
        } else {
            // Reach guard: the 2 damage became 2 counters.
            assert_eq!(counters, 2);
        }
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
fn gatta_and_luzzus_counters_take_one_legal_order_after_the_batch() {
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
            // each counter takes its own legal order, 3 or 4.
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
            // Zone #174) gives each counter its own legal order, 3 or 4.
            assert!([6, 7, 8].contains(&counters), "got {counters} counters");
        } else {
            assert_eq!(counters, 2);
        }

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// The rider's own prompt at the aggregate drain. Channel Harm: "If damage is prevented this way,
/// you may have Channel Harm deal that much damage to target creature." Its ruling: either player
/// can break the loop "by choosing to not have Channel Harm try to deal damage", so the declined
/// "may" is a legal answer: the Hill Giant keeps only the Bears' 2. Unblocked, nothing is
/// prevented and P1 takes 2.
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

        // strict-failure: CR 510.2: whether Channel Harm deals the 3 is P0's choice, and combat
        // can't ask it yet (Command Zone #140); the "may" is cleared, as declined.
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

/// Both drains pause in one batch. Swans of Bryn Argoll blocks one of P0's Grizzly Bears: "The
/// source's controller draws cards equal to the damage prevented this way", each draw meeting
/// P0's Abundance. P0's Channel Harm, cast at the Hill Giant blocking the other Bears, prevents
/// the blockers' damage and asks its "may" after the batch.
#[test]
fn a_may_park_and_a_riders_own_prompt_settle_in_one_batch() {
    for abundance in [true, false] {
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

        let combat = fight(
            &mut runner,
            &[first, second],
            &[(giant, first), (swans, second)],
            None,
        );

        // strict-failure: CR 510.2 + CR 616.1: Abundance's "you may instead" on each draw and
        // Channel Harm's "you may" are P0's to answer, and combat can't ask them yet (Command
        // Zone #140). Each Abundance "may" is declined, which keeps the draw; Channel Harm's is
        // cleared, as declined. CR 121.2: the Swans' 2 is two draws, so 2 cards.
        assert_batch_settled(&runner);
        assert!(combat.prompts.is_empty());
        assert_eq!(prevented(&combat.events), 2);
        assert_eq!(hand_size(&runner, P0) - hand_before, 2);
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

/// A "may" park at the per-portion drain. Swans of Bryn Argoll blocks P0's Grizzly Bears; the
/// Swans' draws meet P0's Abundance ("If you would draw a card, you may instead …").
#[test]
fn swans_of_bryn_argolls_draws_are_kept_under_abundance() {
    for abundance in [true, false] {
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

        let combat = fight(&mut runner, &[bears], &[(swans, bears)], None);

        // strict-failure: CR 510.2 + CR 616.1: Abundance's "may" on each draw is P0's to answer,
        // and combat can't ask it yet (Command Zone #140); each is declined, which keeps the
        // draw. CR 121.2: "draws cards equal to the damage prevented" is 2 draws, so 2 cards.
        assert_batch_settled(&runner);
        assert!(combat.prompts.is_empty());
        assert_eq!(prevented(&combat.events), 1);
        assert_eq!(hand_size(&runner, P0) - hand_before, 2);
        assert_eq!(runner.state().objects[&swans].damage_marked, 0);
        assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);

        bolt_at(&mut runner, bolt, courser);
        assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
    }
}

/// The "may" park in each combat damage step (CR 510.4, CR 702.4b): Boros Swiftblade (double
/// strike) blocked by Swans of Bryn Argoll draws one card in each step, then dies to the Swans.
#[test]
fn double_strike_keeps_the_swans_draw_in_each_damage_step() {
    for abundance in [true, false] {
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

        let combat = fight(&mut runner, &[blade], &[(swans, blade)], None);

        // strict-failure: Abundance's "may" on each draw is declined (CR 510.2 + CR 616.1,
        // Command Zone #140).
        assert_batch_settled(&runner);
        assert!(combat.prompts.is_empty());
        assert_eq!(prevented(&combat.events), 2);
        assert_eq!(hand_size(&runner, P0) - hand_before, 2);
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
/// graveyard: the exile is the rider's own choice of 3.
#[test]
fn nefarious_lichs_exile_choice_is_cleared_inside_the_batch() {
    let mut scenario = rider_board();
    for index in 0..4 {
        scenario.add_creature_to_graveyard(P1, &format!("Graveyard Filler {index}"), 1, 1);
    }
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    let bolt = spell(&mut scenario, "Lightning Bolt", BOLT, true);
    let mut runner = scenario.build();

    let combat = fight(&mut runner, &[giant], &[], None);

    // strict-failure: the card has P1 exile 3 of its 4 graveyard cards, its choice, and not lose.
    // CR 510.2: combat can't ask that choice yet (Command Zone #140), so it is cleared: nothing is
    // exiled, and nobody loses.
    assert_batch_settled(&runner);
    assert!(combat.prompts.is_empty());
    assert!(!runner.state().players[1].is_eliminated);
    // Reach guard: the substitution applied, so no damage reached P1.
    assert_eq!(life(&runner, P1), 20);
    assert_eq!(runner.state().players[1].graveyard.len(), 4);

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

/// A second rider pauses in the batch after Nefarious Lich ended the game (P1's graveyard is
/// empty). Outfitted Jouster wearing Steelclaw Lance and Brawler's Plate, blocked: its own
/// prompt. Ironscale Hydra under P0's Hardened Scales and Doubling Season, blocked: an order park.
/// The game's result stands, and the second pause is still settled.
#[test]
fn a_rider_pause_after_the_game_ended_inside_the_batch() {
    // The Jouster's board.
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let jouster = scenario
        .add_creature_from_oracle(P0, "Outfitted Jouster", 2, 2, JOUSTER)
        .id();
    let lance = equipment_of(&mut scenario, P0, "Steelclaw Lance", STEELCLAW_LANCE);
    let plate = equipment_of(&mut scenario, P0, "Brawler's Plate", BRAWLERS_PLATE);
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), lance, jouster);
    attach_to(runner.state_mut(), plate, jouster);

    let combat = fight(&mut runner, &[giant, jouster], &[(blocker, jouster)], None);

    assert_p1_lost_inside_the_batch(&runner, &combat.events);
    assert_eq!(prevented(&combat.events), 1);
    assert_eq!(runner.state().objects[&jouster].damage_marked, 0);
    for equipment in [lance, plate] {
        assert_eq!(runner.state().objects[&equipment].zone, Zone::Battlefield);
    }

    // The Hydra's board.
    let mut scenario = rider_board();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH);
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let hydra = scenario
        .add_creature_from_oracle(P0, "Ironscale Hydra", 5, 5, IRONSCALE_HYDRA)
        .id();
    scales_and_season(&mut scenario, P0);
    let courser = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    let mut runner = scenario.build();

    let combat = fight(&mut runner, &[giant, hydra], &[(courser, hydra)], None);

    assert_p1_lost_inside_the_batch(&runner, &combat.events);
    assert_eq!(prevented(&combat.events), 1);
    // CR 616.1: one counter, Doubling Season first (3) or Hardened Scales first (4).
    let counters = plus_counters(&runner, hydra);
    assert!([3, 4].contains(&counters), "got {counters} counters");
}

/// Dralnu, Lich Lord: "If damage would be dealt to Dralnu, sacrifice that many permanents
/// instead." Blocking a 2/2 with four other permanents under its controller.
#[test]
fn dralnus_sacrifice_choice_is_cleared_inside_the_batch() {
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

    let combat = fight(&mut runner, &[bears], &[(dralnu, bears)], None);

    // strict-failure: the card has P1 sacrifice 2 permanents of its choice; combat can't ask yet
    // (CR 510.2, Command Zone #140), and nothing is sacrificed (an engine limit, not derived).
    assert_batch_settled(&runner);
    assert!(combat.prompts.is_empty());
    // Reach guard: the substitution applied and Dralnu dealt its damage.
    assert_eq!(runner.state().objects[&dralnu].damage_marked, 0);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    for other in others {
        assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
    }

    bolt_at(&mut runner, bolt, courser);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
}

/// Outfitted Jouster attacking, blocked by one or two 2/2s: one Equipment attached is sacrificed
/// with no prompt; with two, the sacrifice is its controller's pick (CR 608.2d, CR 701.21a).
#[test]
fn outfitted_jousters_sacrifice_in_combat() {
    for equipped in [1usize, 2] {
        for blockers in [1usize, 2] {
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

            let combat = fight(&mut runner, &[jouster], &blocks, None);

            assert_batch_settled(&runner);
            assert!(combat.prompts.is_empty());
            assert_eq!(prevented(&combat.events), 1);
            assert_eq!(runner.state().objects[&jouster].damage_marked, 0);
            for &b in &blocking {
                assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard);
            }
            let expected = if equipped == 1 {
                Zone::Graveyard
            } else {
                // strict-failure: the card has P0 pick one of the two to sacrifice; combat can't
                // ask yet (CR 510.2, Command Zone #140), and none is sacrificed (an engine limit,
                // not derived).
                Zone::Battlefield
            };
            for &equipment in &attached {
                assert_eq!(
                    runner.state().objects[&equipment].zone,
                    expected,
                    "{equipped} equipped, {blockers} blocking"
                );
            }

            bolt_at(&mut runner, bolt, courser);
            assert_eq!(runner.state().objects[&courser].zone, Zone::Graveyard);
        }
    }
}
