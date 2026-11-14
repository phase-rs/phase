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
        assert_eq!(prevented(&events), 2, "{picks:?}");
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
    assert_eq!(prevented(&events), 2);
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
    assert_eq!(prevented(&events), 2);
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
                GameEvent::DamagePrevented { source_id, target: TargetRef::Object(id), .. }
                    if *source_id == giant && *id == knight
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
            readings(&forwarded.context.settled_damage_guards),
            vec![(champion, DamageGuardReading::Holds)]
        );
        let else_clause = forwarded.else_ability.as_deref().expect("the else clause");
        assert_eq!(
            readings(&else_clause.context.settled_damage_guards),
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
/// first) or 6 (Hardened Scales first), its controller's choice. Nothing outlives the batch, and
/// the state-based actions after it run (Lightning Bolt kills a 3/3).
#[test]
fn phytohydras_counter_order_is_settled_inside_the_batch() {
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
            // strict-failure: the order is P1's to choose (CR 616.1), and combat can't ask it yet
            // (Command Zone #140); the engine takes the first candidate, not P1's pick.
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
        // Zone #140). The engine declines each Abundance "may", which keeps the draw, and clears
        // Channel Harm's unasked; neither is P0's answer. CR 121.2: the Swans' 2 is two draws, so 2
        // cards.
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

/// Nothing happens after the game ends (CR 104.1: "A game ends immediately when a player wins").
/// Nefarious Lich ends it at the first portion of the batch: P0's unblocked Hill Giant deals its 3
/// to P1, whose graveyard is empty. The later portions are never dealt: Outfitted Jouster wearing
/// Steelclaw Lance and Brawler's Plate, blocked, would prevent and sacrifice; Ironscale Hydra under
/// P0's Hardened Scales and Doubling Season, blocked, would prevent and gain a counter.
#[test]
fn nothing_happens_in_the_batch_after_the_game_ends() {
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
    assert_nothing_after_the_game_ended(&runner, &combat.events);
    assert_eq!(prevented(&combat.events), 0);
    for equipment in [lance, plate] {
        assert_eq!(runner.state().objects[&equipment].zone, Zone::Battlefield);
    }
    assert_eq!(runner.state().objects[&blocker].damage_marked, 0);

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
    assert_nothing_after_the_game_ended(&runner, &combat.events);
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
/// you gain that much life.") sees only P2's loss.
#[test]
fn a_player_who_loses_inside_the_batch_leaves_and_the_batch_goes_on() {
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
    scales_and_season(&mut scenario, P0);
    let courser = scenario.add_creature(p2, "Centaur Courser", 3, 3).id();
    let mut runner = scenario.build();

    let attacks = [
        (giant, AttackTarget::Player(P1)),
        (second_bears, AttackTarget::Player(P1)),
        (hydra, AttackTarget::Player(p2)),
        (bears, AttackTarget::Player(p2)),
    ];
    let events = combat_at(&mut runner, &attacks, p2, &[(courser, hydra)]);

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
    // strict-failure: CR 616.1: the counter's order (Doubling Season first: 3; Hardened Scales
    // first: 4) is P0's to choose, and combat can't ask it yet (Command Zone #140); the engine takes
    // the first candidate, not P0's pick.
    let counters = plus_counters(&runner, hydra);
    assert!([3, 4].contains(&counters), "got {counters} counters");
    assert_eq!(state.objects[&courser].zone, Zone::Graveyard);
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
    assert_eq!(state.phase, Phase::EndCombat);
}

/// Drive one combat with per-attacker targets: `defender` blocks with `blocks`, every other
/// defending player blocks nothing.
fn combat_at(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
    defender: PlayerId,
    blocks: &[(ObjectId, ObjectId)],
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
            other => panic!("combat stopped at {other:?}"),
        };
        events.extend(runner.act(action).expect("combat action").events);
    }
    panic!("combat did not end");
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
    // (CR 510.2, Command Zone #140), and nothing is sacrificed (an engine limit, not derived). The
    // card reports unsupported for it.
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
