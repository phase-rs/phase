//! CR 615.12 for prevention effects that have additional effects. The module holds the Outfitted
//! Jouster population rows, then the printed "while" guards on prevention lines and the one
//! reading a guard gives a simultaneous damage batch. The admission of these effects under damage
//! that can't be prevented extends it later.
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
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
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

fn bolt_at(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) -> Vec<GameEvent> {
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
/// can't be cast without a planeswalker (filed); the planeswalker is a textless filler.
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
/// (After the pause the else clause's damage goes to the planeswalker, at base too; that is filed,
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
