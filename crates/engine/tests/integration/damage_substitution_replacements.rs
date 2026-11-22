//! "If damage would be dealt to X, <Y> that many … instead" is a replacement effect, not a
//! prevention effect (CR 614.1a, CR 615.1a), and it is order-material against every other
//! damage replacement (CR 616.1).
//!
//! The class is every `DamageDone` replacement `replacement::is_damage_substitution` selects:
//! these seven (Phytohydra, Lichenthrope, Force Bubble, Delaying Shield, Crumbling Sanctuary,
//! Dralnu, Lich Lord and Nefarious Lich), and Soul-Scar Mage, Szadek, Lord of Secrets and Undead
//! Alchemist. Phytohydra's ruling states the class:
//! "Phytohydra's ability doesn't prevent damage. Damage that can't be prevented will still be
//! replaced by +1/+1 counters being put on Phytohydra."
//!
//! Three seams meet here, and each row below goes through the production pipeline:
//!
//! - A rider that picks from a population acts on what the player picks, and the spell's next
//!   instruction follows all of it (CR 608.2c, CR 608.2d): Lightning Helix's "and you gain 3
//!   life" under Nefarious Lich.
//! - A combat damage batch asks each CR 616.1 order and each prompt a replacement's action or
//!   rider raises, at the leg or at the aggregate prevention-rider drain (CR 510.2 rules out a
//!   priority window, not these choices); a substitution's action precedes an earlier rider
//!   there as elsewhere (CR 614.6 + CR 615.5).
//! - A spell's later instruction runs once, behind its remaining recipients, whichever
//!   replacement answer reaches it.

use std::collections::BTreeSet;

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, EffectKind, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, LossReason};
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, ReplacementCandidateSummary, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::{Player, PlayerId};
use engine::types::zones::Zone;

use super::rules::run_combat;

const P2: PlayerId = PlayerId(2);

const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const STOMP: &str = "Damage can't be prevented this turn. Stomp deals 2 damage to any target.";
const HELIX: &str = "Lightning Helix deals 3 damage to any target and you gain 3 life.";
const CHILL: &str = "Creeping Chill deals 3 damage to each opponent and you gain 3 life.\nWhen Creeping Chill is put into your graveyard from your library, you may exile it. If you do, Creeping Chill deals 3 damage to each opponent and you gain 3 life.";
const SHAKE: &str =
    "Shake the Foundations deals 1 damage to each creature without flying.\nDraw a card.";
const PYROCLASM: &str = "Pyroclasm deals 2 damage to each creature.";
const FLAME_RIFT: &str = "Flame Rift deals 4 damage to each player.";
const RAKDOS_CHARM: &str = "Choose one —\n• Exile target player's graveyard.\n• Destroy target artifact.\n• Each creature deals 1 damage to its controller.";
const PIT_FIGHT: &str = "Target creature you control fights another target creature. (Each deals damage equal to its power to the other.)";
const PARIAH: &str =
    "Enchant creature\nAll damage that would be dealt to you is dealt to enchanted creature instead.";
const COORDINATED_CLOBBERING: &str = "Tap one or two target untapped creatures you control. They each deal damage equal to their power to target creature an opponent controls.";
const ALLIES_AT_LAST: &str = "Affinity for Allies (This spell costs {1} less to cast for each Ally you control.)\nUp to two target creatures you control each deal damage equal to their power to target creature an opponent controls.";
const FLAME_SLASH: &str = "Flame Slash deals 4 damage to target creature.";
const HYMN: &str = "Kicker {W} (You may pay an additional {W} as you cast this spell.)\nHurloon Battle Hymn deals 4 damage to target creature or planeswalker. If this spell was kicked, you gain 4 life.";
const AARDVARK_SLOTH: &str =
    "Lifelink (Damage dealt by this creature also causes you to gain that much life.)";
const TANNUK: &str = "Landfall — Whenever a land you control enters, Tannuk deals 1 damage to each opponent. If this is the second time this ability has resolved this turn, draw a card.";
const EXPLORATION: &str = "You may play an additional land on each of your turns.";
const NIBELHEIM: &str = "Choose target creature you control. It deals damage equal to its power to each other creature. If this spell was cast from a graveyard, discard your hand and draw four cards.\nFlashback {5}{R}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const NEW_WAY_FORWARD: &str = "The next time a source of your choice would deal damage to you this turn, prevent that damage. When damage is prevented this way, New Way Forward deals that much damage to that source's controller and you draw that many cards.";
const MENDING_HANDS: &str =
    "Prevent the next 4 damage that would be dealt to any target this turn.";
const TEST_OF_FAITH: &str = "Prevent the next 3 damage that would be dealt to target creature this turn. For each 1 damage prevented this way, put a +1/+1 counter on that creature.";
const TEMPER: &str = "Prevent the next X damage that would be dealt to target creature this turn. For each 1 damage prevented this way, put a +1/+1 counter on that creature.";

const PHYTOHYDRA: &str =
    "If damage would be dealt to this creature, put that many +1/+1 counters on it instead.";
const LICHENTHROPE: &str = "If damage would be dealt to this creature, put that many -1/-1 counters on it instead.\nAt the beginning of your upkeep, remove a -1/-1 counter from this creature.";
const PHYREXIAN_HYDRA: &str = "Infect (This creature deals damage to creatures in the form of -1/-1 counters and to players in the form of poison counters.)\nIf damage would be dealt to this creature, prevent that damage. Put a -1/-1 counter on this creature for each 1 damage prevented this way.";
const SWANS_OF_BRYN_ARGOLL: &str = "Flying\nIf a source would deal damage to this creature, prevent that damage. The source's controller draws cards equal to the damage prevented this way.";
const ABUNDANCE: &str = "If you would draw a card, you may instead choose land or nonland and reveal cards from the top of your library until you reveal a card of the chosen kind. Put that card into your hand and put all other cards revealed this way on the bottom of your library in any order.";
const FORCE_BUBBLE: &str = "If damage would be dealt to you, put that many depletion counters on this enchantment instead.\nWhen there are four or more depletion counters on this enchantment, sacrifice it.\nAt the beginning of each end step, remove all depletion counters from this enchantment.";
const DELAYING_SHIELD: &str = "If damage would be dealt to you, put that many delay counters on this enchantment instead.\nAt the beginning of your upkeep, remove all delay counters from this enchantment. For each delay counter removed this way, you lose 1 life unless you pay {1}{W}.";
const CRUMBLING_SANCTUARY: &str = "If damage would be dealt to a player, that player exiles that many cards from the top of their library instead.";
const DRALNU: &str = "If damage would be dealt to Dralnu, sacrifice that many permanents instead.\n{T}: Target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
const SANWELL: &str = "As long as an artifact creature you control is attacking, prevent all damage that would be dealt to Sanwell.\nWhenever Sanwell becomes tapped, exile the top six cards of your library. You may cast a Vehicle or artifact creature spell from among them. Then put the rest on the bottom of your library in a random order.";
const ORNITHOPTER: &str = "Flying";
const NEFARIOUS_LICH: &str = "If damage would be dealt to you, exile that many cards from your graveyard instead. If you can't, you lose the game.\nIf you would gain life, draw that many cards instead.\nWhen this enchantment leaves the battlefield, you lose the game.";

const FURNACE: &str = "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";
const TORBRAN: &str = "If a red source you control would deal damage to an opponent or a permanent an opponent controls, it deals that much damage plus 2 instead.";
const GRATUITOUS_VIOLENCE: &str =
    "If a creature you control would deal damage to a permanent or player, it deals double that damage instead.";
const POLUKRANOS: &str = "Polukranos enters with six +1/+1 counters on it. It escapes with twelve +1/+1 counters on it instead.\nIf damage would be dealt to Polukranos while it has a +1/+1 counter on it, prevent that damage and remove that many +1/+1 counters from it.\n{1}{B}{G}: Polukranos fights another target creature.\nEscape—{4}{B}{G}, Exile six other cards from your graveyard.";
const SHIELDMATE: &str = "This creature enters with a shield counter on it. (If it would be dealt damage or destroyed, remove a shield counter from it instead.)\nDuring your turn, this creature gets +2/+0.";
const ALCHEMIST: &str = "If a source would deal damage to a player, you may prevent X of that damage, where X is the number of Clerics you control.";
const SARKHAN: &str = "[0]: Reveal the top card of your library and put it into your hand. Sarkhan deals damage to himself equal to that card's mana value.\n[−2]: Target creature's controller sacrifices it, then that player creates a 5/5 red Dragon creature token with flying.\n[−4]: Each Dragon creature you control deals damage equal to its power to target player or planeswalker.";
const URZAS_ARMOR: &str = "If a source would deal damage to you, prevent 1 of that damage.";
const PRODIGAL_SORCERER: &str = "{T}: This creature deals 1 damage to any target.";
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const SOUL_SCAR_MAGE: &str = "Prowess\nIf a source you control would deal noncombat damage to a creature an opponent controls, put that many -1/-1 counters on that creature instead.";
const SZADEK_NAME: &str = "Szadek, Lord of Secrets";
const SZADEK: &str = "Flying\nIf Szadek would deal combat damage to a player, instead put that many +1/+1 counters on Szadek and that player mills that many cards.";
const UNDEAD_ALCHEMIST: &str = "If a Zombie you control would deal combat damage to a player, instead that player mills that many cards.\nWhenever a creature card is put into an opponent's graveyard from their library, exile that card and create a 2/2 black Zombie creature token.";
const SILVER_KNIGHT: &str = "First strike, protection from red";
const TEFERIS_PROTECTION: &str = "Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out. (While they're phased out, they're treated as though they don't exist. They phase in before you untap during your untap step.)\nExile Teferi's Protection.";
const DAUNTING_DEFENDER: &str =
    "If a source would deal damage to a Cleric creature you control, prevent 1 of that damage.";
const VIZIER_OF_REMEDIES: &str = "If one or more -1/-1 counters would be put on a creature you control, that many -1/-1 counters minus one are put on it instead.";

/// The candidate description of a resolution-created prevention shield. Its `source_id` is the
/// shielded object, so a shield on Phytohydra can't be named by `source_id`.
const SHIELD: &str = "Prevent damage";

fn depletion() -> CounterType {
    CounterType::Generic("depletion".to_string())
}

fn delay() -> CounterType {
    CounterType::Generic("delay".to_string())
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn object_target(id: ObjectId) -> TargetRef {
    TargetRef::Object(id)
}

fn player_target(player: PlayerId) -> TargetRef {
    TargetRef::Player(player)
}

fn instant(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .id()
}

fn sorcery(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, false, text)
        .id()
}

fn bolt(scenario: &mut GameScenario) -> ObjectId {
    instant(scenario, "Lightning Bolt", BOLT)
}

fn stomp(scenario: &mut GameScenario) -> ObjectId {
    instant(scenario, "Stomp", STOMP)
}

fn helix(scenario: &mut GameScenario) -> ObjectId {
    instant(scenario, "Lightning Helix", HELIX)
}

fn phytohydra(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(controller, "Phytohydra", 1, 1, PHYTOHYDRA)
        .id()
}

fn dralnu(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(controller, "Dralnu, Lich Lord", 3, 3, DRALNU)
        .id()
}

fn force_bubble(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(controller, "Force Bubble", FORCE_BUBBLE)
        .id()
}

fn nefarious_lich(scenario: &mut GameScenario, graveyard_cards: usize) -> ObjectId {
    for index in 0..graveyard_cards {
        scenario.add_creature_to_graveyard(P1, &format!("Filler {index}"), 1, 1);
    }
    scenario
        .add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH)
        .id()
}

fn soul_scar_mage(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_creature(controller, "Soul-Scar Mage", 1, 2)
        .with_subtypes(vec!["Human", "Wizard"])
        .from_oracle_text_with_keywords(&["Prowess"], SOUL_SCAR_MAGE)
        .id()
}

fn szadek(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, SZADEK_NAME, 5, 5)
        .with_subtypes(vec!["Vampire"])
        .from_oracle_text_with_keywords(&["Flying"], SZADEK)
        .id()
}

fn undead_alchemist(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Undead Alchemist", 4, 2, UNDEAD_ALCHEMIST)
        .with_subtypes(vec!["Zombie"])
        .id()
}

fn vizier_of_remedies(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(controller, "Vizier of Remedies", 2, 1, VIZIER_OF_REMEDIES)
        .with_subtypes(vec!["Human", "Cleric"])
        .id()
}

/// The counters put on `id`, event by event, in order.
fn counters_put(events: &[GameEvent], id: ObjectId) -> Vec<(CounterType, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CounterAdded {
                object_id,
                counter_type,
                count,
                ..
            } if *object_id == id => Some((counter_type.clone(), *count)),
            _ => None,
        })
        .collect()
}

/// The total of `kind` counters put on `id` across `events`.
fn counters_put_total(events: &[GameEvent], id: ObjectId, kind: &CounterType) -> u32 {
    counters_put(events, id)
        .iter()
        .filter(|(found, _)| found == kind)
        .map(|(_, count)| count)
        .sum()
}

/// The library and graveyard sizes of `id`.
fn library_and_graveyard(runner: &GameRunner, id: PlayerId) -> (usize, usize) {
    let seat = player(runner, id);
    (seat.library.len(), seat.graveyard.len())
}

/// The `ReplacementChoice` that must be up: its chooser and candidate sources.
#[track_caller]
fn replacement_prompt(runner: &GameRunner) -> (PlayerId, BTreeSet<ObjectId>) {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => (
            *player,
            candidates
                .iter()
                .map(|candidate| candidate.source_id)
                .collect(),
        ),
        other => panic!("expected ReplacementChoice, got {other:?}"),
    }
}

fn furnace(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Furnace of Rath", FURNACE)
        .id()
}

fn lands(scenario: &mut GameScenario, controller: PlayerId, count: usize) -> Vec<ObjectId> {
    (0..count)
        .map(|_| scenario.add_basic_land(controller, ManaColor::Blue))
        .collect()
}

/// Drive to the next prompt, or to `Priority` with an empty stack.
fn settle(runner: &mut GameRunner, targets: &mut Vec<TargetRef>, events: &mut Vec<GameEvent>) {
    for _ in 0..120 {
        let action = match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(targets.remove(0)),
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                GameAction::PassPriority
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            _ => return,
        };
        events.extend(runner.act(action).expect("settle action").events);
    }
    panic!("the game did not settle");
}

fn cast_action(runner: &GameRunner, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn cast(runner: &mut GameRunner, spell: ObjectId, targets: Vec<TargetRef>) -> Vec<GameEvent> {
    let action = cast_action(runner, spell);
    let mut events = runner.act(action).expect("cast").events;
    let mut targets = targets;
    settle(runner, &mut targets, &mut events);
    events
}

fn answer(runner: &mut GameRunner, action: GameAction, events: &mut Vec<GameEvent>) {
    events.extend(runner.act(action).expect("answer").events);
    settle(runner, &mut Vec::new(), events);
}

/// Answer the `ReplacementChoice` that must be up with the candidate `pick` selects.
#[track_caller]
fn choose(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    pick: impl Fn(&ReplacementCandidateSummary) -> bool,
) {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "expected ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let index = candidates
        .iter()
        .position(pick)
        .expect("the candidate is offered");
    answer(runner, GameAction::ChooseReplacement { index }, events);
}

#[track_caller]
fn choose_source(runner: &mut GameRunner, events: &mut Vec<GameEvent>, source: ObjectId) {
    choose(runner, events, |candidate| candidate.source_id == source);
}

#[track_caller]
fn choose_not_source(runner: &mut GameRunner, events: &mut Vec<GameEvent>, source: ObjectId) {
    choose(runner, events, |candidate| candidate.source_id != source);
}

#[track_caller]
fn choose_shield(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    choose(runner, events, |candidate| candidate.description == SHIELD);
}

#[track_caller]
fn choose_not_shield(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    choose(runner, events, |candidate| candidate.description != SHIELD);
}

/// The `(count, cards offered)` of the `EffectZoneChoice` that must be up.
#[track_caller]
fn zone_prompt(runner: &GameRunner) -> (usize, usize) {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { count, cards, .. } => (*count, cards.len()),
        other => panic!("expected EffectZoneChoice, got {other:?}"),
    }
}

/// Answer the `EffectZoneChoice` that must be up with the first `count` offered cards.
#[track_caller]
fn take_offered(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    let WaitingFor::EffectZoneChoice { count, cards, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected EffectZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let cards = cards.into_iter().take(count).collect();
    answer(runner, GameAction::SelectCards { cards }, events);
}

/// Combat, keeping the events `run_combat` discards.
fn combat_events(
    runner: &mut GameRunner,
    attackers: Vec<ObjectId>,
    blocks: Vec<(ObjectId, ObjectId)>,
) -> Vec<GameEvent> {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .into_iter()
                .map(|id| (id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .expect("declare attackers");
    runner.pass_both_players();
    let mut events = Vec::new();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        events.extend(
            runner
                .act(GameAction::DeclareBlockers {
                    assignments: blocks,
                })
                .expect("declare blockers")
                .events,
        );
    }
    for _ in 0..8 {
        // H0, CR 510.2: no priority window while the combat damage batch is parked.
        super::unpreventable_damage_prevention_effects::assert_no_priority_inside_the_batch(
            runner.state(),
        );
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority")
                .events,
        );
    }
    events
}

fn counters(runner: &GameRunner, id: ObjectId, counter: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(counter)
        .copied()
        .unwrap_or(0)
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    counters(runner, id, &CounterType::Plus1Plus1)
}

fn damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn player(runner: &GameRunner, id: PlayerId) -> &Player {
    runner
        .state()
        .players
        .iter()
        .find(|player| player.id == id)
        .expect("the player exists")
}

fn life(runner: &GameRunner, id: PlayerId) -> i32 {
    player(runner, id).life
}

fn prevented(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::DamagePrevented { .. }))
        .count()
}

fn dealt(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::DamageDealt { .. }))
        .count()
}

fn life_gains(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::LifeChanged { amount, .. } if *amount > 0))
        .count()
}

fn draws(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { .. }))
        .count()
}

fn resolved(events: &[GameEvent], kind: EffectKind) -> bool {
    events.iter().any(
        |event| matches!(event, GameEvent::EffectResolved { kind: found, .. } if *found == kind),
    )
}

/// CR 104.3e + CR 104.2b: each player who lost, with the reasons recorded, in player order.
pub(super) fn player_losses(events: &[GameEvent]) -> Vec<(PlayerId, BTreeSet<LossReason>)> {
    let mut losses: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerLost { player_id, reasons } => Some((*player_id, reasons.clone())),
            _ => None,
        })
        .collect();
    losses.sort();
    losses
}

/// The events that show the order of a resolution: one label per damage dealt, card drawn,
/// and sacrifice or discard resolved.
fn order_labels(events: &[GameEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt { amount, .. } => Some(format!("damage {amount}")),
            GameEvent::CardDrawn { .. } => Some("draw".to_string()),
            GameEvent::EffectResolved {
                kind: EffectKind::Sacrifice,
                ..
            } => Some("sacrifice".to_string()),
            GameEvent::EffectResolved {
                kind: EffectKind::Discard,
                ..
            } => Some("discard".to_string()),
            _ => None,
        })
        .collect()
}

#[track_caller]
fn assert_prompt_is_replacement_choice(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
}

#[track_caller]
fn assert_game_over(runner: &GameRunner, winner: PlayerId) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(found), .. } if found == winner
        ),
        "got {:?}",
        runner.state().waiting_for
    );
}

/// The spell or ability finished: `Priority`, an empty stack, and no suspended resolution.
#[track_caller]
fn assert_at_rest(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().resolution_stack.len(), 0);
}

/// Phytohydra: "Damage that can't be prevented will still be replaced by +1/+1 counters being
/// put on Phytohydra." (CR 614.1a, CR 614.6, CR 615.13.)
#[test]
fn phytohydra_replaces_damage_preventable_or_not() {
    for (spell, amount) in [("Stomp", 2), ("Lightning Bolt", 3)] {
        let mut scenario = scenario();
        let hydra = phytohydra(&mut scenario, P1);
        let spell = if spell == "Stomp" {
            stomp(&mut scenario)
        } else {
            bolt(&mut scenario)
        };
        let mut runner = scenario.build();
        let events = cast(&mut runner, spell, vec![object_target(hydra)]);

        assert_eq!(zone(&runner, hydra), Zone::Battlefield);
        assert_eq!(plus_counters(&runner, hydra), amount);
        assert_eq!(damage(&runner, hydra), 0);
        assert_eq!(prevented(&events), 0);
        assert_eq!(dealt(&events), 0);
        assert_at_rest(&runner);
    }

    let mut scenario = scenario();
    let hydra = phytohydra(&mut scenario, P1);
    let attacker = scenario.add_creature(P0, "Ogre", 3, 3).id();
    let mut runner = scenario.build();
    let events = combat_events(&mut runner, vec![attacker], vec![(hydra, attacker)]);

    assert_eq!(plus_counters(&runner, hydra), 3);
    assert_eq!(damage(&runner, hydra), 0);
    assert_eq!(prevented(&events), 0);
}

/// Lichenthrope: "This card's effect replaces damage with placement of -1/-1 counters, so
/// side-effects of damage to it will not trigger."
#[test]
fn lichenthrope_replaces_damage_preventable_or_not() {
    for (spell, amount) in [("Stomp", 2), ("Lightning Bolt", 3)] {
        let mut scenario = scenario();
        let id = scenario
            .add_creature_from_oracle(P1, "Lichenthrope", 5, 5, LICHENTHROPE)
            .id();
        let spell = if spell == "Stomp" {
            stomp(&mut scenario)
        } else {
            bolt(&mut scenario)
        };
        let mut runner = scenario.build();
        let events = cast(&mut runner, spell, vec![object_target(id)]);

        assert_eq!(counters(&runner, id, &CounterType::Minus1Minus1), amount);
        assert_eq!(damage(&runner, id), 0);
        assert_eq!(prevented(&events), 0);
        assert_eq!(dealt(&events), 0);
    }
}

/// Nefarious Lich: "exile that many cards from your graveyard instead. If you can't, you lose
/// the game." The player picks which cards (CR 608.2d), under damage that can't be prevented.
#[test]
fn nefarious_lich_exiles_the_cards_its_controller_picks() {
    let mut scenario = scenario();
    nefarious_lich(&mut scenario, 3);
    let spell = stomp(&mut scenario);
    let mut runner = scenario.build();
    let graveyard: Vec<ObjectId> = player(&runner, P1).graveyard.iter().copied().collect();
    let mut events = cast(&mut runner, spell, vec![player_target(P1)]);

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            count,
            min_count,
            cards,
            ..
        } => {
            assert_eq!((*count, *min_count, cards.len()), (2, 2, 3));
        }
        other => panic!("expected EffectZoneChoice, got {other:?}"),
    }
    assert_eq!(life(&runner, P1), 20);

    answer(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![graveyard[0], graveyard[1]],
        },
        &mut events,
    );

    assert_eq!(zone(&runner, graveyard[0]), Zone::Exile);
    assert_eq!(zone(&runner, graveyard[1]), Zone::Exile);
    assert_eq!(zone(&runner, graveyard[2]), Zone::Graveyard);
    assert_eq!(life(&runner, P1), 20);
    assert!(!player(&runner, P1).is_eliminated);
    assert_at_rest(&runner);
}

/// Nefarious Lich: "If you can't, you lose the game."
#[test]
fn nefarious_lich_loses_when_it_cannot_exile_enough() {
    let mut scenario = scenario();
    nefarious_lich(&mut scenario, 2);
    let spell = bolt(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![player_target(P1)]);

    assert_game_over(&runner, P0);
}

/// Lightning Helix: "deals 3 damage to any target and you gain 3 life." The gain is the
/// spell's next instruction (CR 608.2c), and it follows the whole replaced event (CR 614.6).
#[test]
fn a_later_instruction_follows_a_prompting_rider() {
    // Helix at P1, Nefarious Lich, 3 graveyard cards.
    let mut scenario = scenario();
    nefarious_lich(&mut scenario, 3);
    let spell = helix(&mut scenario);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![player_target(P1)]);
    assert_eq!(zone_prompt(&runner), (3, 3));
    take_offered(&mut runner, &mut events);
    assert_eq!(life(&runner, P0), 23);
    assert_eq!(life(&runner, P1), 20);
    assert!(!player(&runner, P1).is_eliminated);
    assert_at_rest(&runner);

    // Helix at P1, Nefarious Lich, 2 graveyard cards: the rider loses the game.
    let mut scenario = self::scenario();
    nefarious_lich(&mut scenario, 2);
    let spell = helix(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![player_target(P1)]);
    assert_game_over(&runner, P0);
    assert_eq!(life(&runner, P1), 20);

    // Helix at Dralnu with 3 other permanents.
    let mut scenario = self::scenario();
    let id = dralnu(&mut scenario, P1);
    lands(&mut scenario, P1, 3);
    let spell = helix(&mut scenario);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![object_target(id)]);
    assert_eq!(zone_prompt(&runner), (3, 4));
    take_offered(&mut runner, &mut events);
    assert_eq!(life(&runner, P0), 23);
    assert_at_rest(&runner);

    // Creeping Chill, a `DamageEachPlayer` head, under Nefarious Lich.
    let mut scenario = self::scenario();
    nefarious_lich(&mut scenario, 3);
    let spell = sorcery(&mut scenario, "Creeping Chill", CHILL);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![]);
    assert_eq!(zone_prompt(&runner), (3, 3));
    take_offered(&mut runner, &mut events);
    assert_eq!(life(&runner, P0), 23);
    assert_eq!(life(&runner, P1), 20);
    assert!(!player(&runner, P1).is_eliminated);
    assert_at_rest(&runner);

    // Shake the Foundations, a `DamageAll` head, under Dralnu.
    let mut scenario = self::scenario();
    dralnu(&mut scenario, P1);
    lands(&mut scenario, P1, 3);
    scenario.with_library_top(P0, &["Top", "Second", "Third"]);
    let spell = instant(&mut scenario, "Shake the Foundations", SHAKE);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![]);
    assert_eq!(zone_prompt(&runner), (1, 4));
    assert_eq!(draws(&events), 0);
    take_offered(&mut runner, &mut events);
    assert_eq!(draws(&events), 1);
    assert_at_rest(&runner);

    // Helix at P1 with Force Bubble: no prompt, and nothing is dealt or prevented.
    let mut scenario = self::scenario();
    let bubble = force_bubble(&mut scenario, P1);
    let spell = helix(&mut scenario);
    let mut runner = scenario.build();
    let events = cast(&mut runner, spell, vec![player_target(P1)]);
    assert_eq!(counters(&runner, bubble, &depletion()), 3);
    assert_eq!(life(&runner, P0), 23);
    assert_eq!(life(&runner, P1), 20);
    assert_eq!(prevented(&events), 0);
    assert_at_rest(&runner);
}

/// Helix at P1's Phytohydra with Hardened Scales and Doubling Season. The prompt belongs to
/// the rider's counter event, not to a park of Helix's damage, and the gain follows the rider
/// once (CR 608.2c, CR 616.1).
#[test]
fn a_rider_opening_its_own_order_prompt_keeps_the_later_instruction_behind_it() {
    let mut scenario = scenario();
    let hydra = phytohydra(&mut scenario, P1);
    scenario.add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    let spell = helix(&mut scenario);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![object_target(hydra)]);

    assert_prompt_is_replacement_choice(&runner);
    assert_eq!(plus_counters(&runner, hydra), 0);
    assert_eq!(life(&runner, P0), 20);
    answer(
        &mut runner,
        GameAction::ChooseReplacement { index: 0 },
        &mut events,
    );

    assert_eq!(plus_counters(&runner, hydra), 7);
    assert_eq!(life(&runner, P0), 23);
    assert_eq!(life(&runner, P1), 20);
    assert_at_rest(&runner);
}

/// P1's Lightning Helix aimed at P0, resolved under P0's New Way Forward naming Helix. Returns
/// the game at the first prompt or at rest, with the events so far and Helix's id.
fn new_way_forward_game(
    build: impl FnOnce(&mut GameScenario) -> Option<ObjectId>,
) -> (GameRunner, Vec<GameEvent>, ObjectId) {
    let mut scenario = scenario();
    let mending_hands = build(&mut scenario);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four", "Five", "Six", "Seven"]);
    let helix = scenario
        .add_spell_to_hand_from_oracle(P1, "Lightning Helix", true, HELIX)
        .id();
    let new_way_forward = instant(&mut scenario, "New Way Forward", NEW_WAY_FORWARD);
    let mut runner = scenario.build();
    if let Some(mending_hands) = mending_hands {
        cast(&mut runner, mending_hands, vec![player_target(P1)]);
    }

    // P1 needs priority to cast Helix in P0's main phase.
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let mut events = Vec::new();
    let action = cast_action(&runner, helix);
    events.extend(runner.act(action).expect("cast Helix").events);
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(player_target(P0)),
            })
            .expect("target P0")
            .events,
    );
    while matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(runner.act(GameAction::PassPriority).expect("pay").events);
    }
    events.extend(runner.act(GameAction::PassPriority).expect("pass").events);

    let action = cast_action(&runner, new_way_forward);
    events.extend(runner.act(action).expect("cast New Way Forward").events);
    while matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(runner.act(GameAction::PassPriority).expect("pay").events);
    }
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        events.extend(runner.act(GameAction::PassPriority).expect("pass").events);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    answer(
        &mut runner,
        GameAction::ChooseDamageSource { source: helix },
        &mut events,
    );
    (runner, events, helix)
}

/// New Way Forward's rider deals damage of its own, and that resolver parking is not Helix's
/// park. "you gain 3 life." is Helix's next instruction (CR 608.2c), behind the reflexive
/// trigger's rider (CR 603.12). The rider's draw count is deliberately unasserted.
#[test]
fn a_riders_own_damage_resolver_does_not_take_the_spells_tail() {
    for shield_first in [true, false] {
        let (mut runner, mut events, helix) = new_way_forward_game(|scenario| {
            scenario.add_artifact_from_oracle(P1, "Urza's Armor", URZAS_ARMOR);
            Some(instant(scenario, "Mending Hands", MENDING_HANDS))
        });

        // P1 is asked, and P0 is the player Helix would damage, so this is the rider's event.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P1, .. }
            ),
            "got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(life_gains(&events), 0);
        if shield_first {
            choose_shield(&mut runner, &mut events);
        } else {
            choose_not_shield(&mut runner, &mut events);
        }

        assert_eq!(life(&runner, P1), 23);
        assert_eq!(life(&runner, P0), 20);
        assert_eq!(life_gains(&events), 1);
        assert_eq!(zone(&runner, helix), Zone::Graveyard);
        assert_at_rest(&runner);
    }
}

/// A rider's own damage replaced at P1's prompt ends neither "you draw that many cards" nor
/// Helix's "you gain 3 life" (CR 603.12, CR 608.2c, CR 614.6).
#[test]
fn a_replaced_answer_under_a_paused_rider_leaves_the_rest_of_the_spell() {
    // Two Force Bubbles, and Force Bubble with Delaying Shield. Force Bubble's ruling: "each
    // time you take damage you can decide which one replaces the damage".
    for (name, text, counter) in [
        ("Force Bubble", FORCE_BUBBLE, depletion()),
        ("Delaying Shield", DELAYING_SHIELD, delay()),
    ] {
        for first in [true, false] {
            let mut ids = None;
            let (mut runner, mut events, helix) = new_way_forward_game(|scenario| {
                let bubble = force_bubble(scenario, P1);
                let other = scenario.add_enchantment_from_oracle(P1, name, text).id();
                ids = Some((bubble, other));
                None
            });
            let (bubble, other) = ids.expect("the board was built");
            let source = if first { bubble } else { other };
            choose_source(&mut runner, &mut events, source);

            let counter = if first { depletion() } else { counter.clone() };
            assert_eq!(counters(&runner, source, &counter), 3, "{name}");
            assert!(resolved(&events, EffectKind::Draw));
            assert_eq!(life_gains(&events), 1);
            assert_eq!(life(&runner, P1), 23);
            assert_eq!(life(&runner, P0), 20);
            assert_eq!(zone(&runner, helix), Zone::Graveyard);
            assert_at_rest(&runner);
        }
    }

    // P1's Force Bubble and Furnace of Rath: P0 orders Helix's event, the shield first, then
    // P1 orders the rider's. Bubble first takes 3; Furnace first takes 6 and sacrifices it.
    for bubble_first in [true, false] {
        let mut ids = None;
        let (mut runner, mut events, helix) = new_way_forward_game(|scenario| {
            let bubble = force_bubble(scenario, P1);
            let furnace = scenario
                .add_enchantment_from_oracle(P1, "Furnace of Rath", FURNACE)
                .id();
            ids = Some((bubble, furnace));
            None
        });
        let (bubble, furnace) = ids.expect("the board was built");
        choose_shield(&mut runner, &mut events);
        choose_source(
            &mut runner,
            &mut events,
            if bubble_first { bubble } else { furnace },
        );

        if bubble_first {
            assert_eq!(counters(&runner, bubble, &depletion()), 3);
        } else {
            assert_eq!(zone(&runner, bubble), Zone::Graveyard);
        }
        assert!(resolved(&events, EffectKind::Draw));
        assert_eq!(life_gains(&events), 1);
        assert_eq!(life(&runner, P1), 23);
        assert_eq!(life(&runner, P0), 20);
        assert_eq!(zone(&runner, helix), Zone::Graveyard);
        assert_at_rest(&runner);
    }
}

/// A fully prevented or replaced order answer leaves "and you gain 3 life" (CR 608.2c, CR 615.5).
/// The orders that keep the tail without the fix are the paired guards.
#[test]
fn a_fully_prevented_order_answer_leaves_the_spells_later_instruction() {
    for shield_first in [true, false] {
        let mut scenario = scenario();
        let bear = scenario.add_creature(P1, "Bear", 2, 9).id();
        furnace(&mut scenario);
        let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
        let spell = helix(&mut scenario);
        let mut runner = scenario.build();
        cast(&mut runner, hands, vec![object_target(bear)]);
        let mut events = cast(&mut runner, spell, vec![object_target(bear)]);
        if shield_first {
            choose_shield(&mut runner, &mut events);
        } else {
            choose_not_shield(&mut runner, &mut events);
        }

        // Mending Hands first prevents the 3. Furnace of Rath first: 6, 4 prevented, 2 dealt.
        assert_eq!(damage(&runner, bear), if shield_first { 0 } else { 2 });
        assert_eq!(prevented(&events), 1);
        assert_eq!(life(&runner, P0), 23);
        assert_eq!(zone(&runner, spell), Zone::Graveyard);
        assert_at_rest(&runner);
    }

    for shield_first in [true, false] {
        let mut scenario = self::scenario();
        let hydra = phytohydra(&mut scenario, P0);
        let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
        let spell = helix(&mut scenario);
        let mut runner = scenario.build();
        cast(&mut runner, hands, vec![object_target(hydra)]);
        let mut events = cast(&mut runner, spell, vec![object_target(hydra)]);
        if shield_first {
            choose_shield(&mut runner, &mut events);
        } else {
            choose_not_shield(&mut runner, &mut events);
        }

        assert_eq!(
            plus_counters(&runner, hydra),
            if shield_first { 0 } else { 3 }
        );
        assert_eq!(life(&runner, P0), 23);
        assert_at_rest(&runner);
    }
}

/// Dralnu's ruling: "If Dralnu would be dealt more damage than the number of permanents you
/// control, instead you sacrifice all permanents you control." Furnace of Rath's ruling gives
/// the order to the recipient's controller (CR 616.1).
#[test]
fn a_rider_prompt_follows_the_order_chosen_under_furnace_of_rath() {
    for dralnu_first in [true, false] {
        let mut scenario = scenario();
        let id = dralnu(&mut scenario, P1);
        let others: Vec<ObjectId> = (0..4)
            .map(|index| {
                scenario
                    .add_creature(P1, &format!("Bear {index}"), 1, 1)
                    .id()
            })
            .collect();
        let doubler = furnace(&mut scenario);
        let spell = bolt(&mut scenario);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![object_target(id)]);
        assert_prompt_is_replacement_choice(&runner);

        if dralnu_first {
            choose_not_source(&mut runner, &mut events, doubler);
            assert_eq!(zone_prompt(&runner), (3, 5));
            take_offered(&mut runner, &mut events);
        } else {
            choose_source(&mut runner, &mut events, doubler);
            assert_eq!(zone(&runner, id), Zone::Graveyard);
            assert!(others
                .iter()
                .all(|other| zone(&runner, *other) == Zone::Graveyard));
        }
        assert_at_rest(&runner);
    }

    for lich_first in [true, false] {
        let mut scenario = self::scenario();
        let lich = nefarious_lich(&mut scenario, 7);
        furnace(&mut scenario);
        let spell = bolt(&mut scenario);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![player_target(P1)]);
        assert_prompt_is_replacement_choice(&runner);
        if lich_first {
            choose_source(&mut runner, &mut events, lich);
        } else {
            choose_not_source(&mut runner, &mut events, lich);
        }

        let count = if lich_first { 3 } else { 6 };
        assert_eq!(zone_prompt(&runner), (count, 7));
        take_offered(&mut runner, &mut events);
        assert_eq!(life(&runner, P1), 20);
        assert!(!player(&runner, P1).is_eliminated);
        assert_eq!(player(&runner, P1).graveyard.len(), 7 - count);
        assert_at_rest(&runner);
    }
}

/// Lightning Helix at an order park gains 3 once, whichever effect goes first (CR 608.2c).
#[test]
fn a_later_instruction_runs_once_at_an_order_park() {
    for hydra_first in [true, false] {
        let mut scenario = scenario();
        let hydra = phytohydra(&mut scenario, P0);
        furnace(&mut scenario);
        let spell = helix(&mut scenario);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![object_target(hydra)]);
        assert_prompt_is_replacement_choice(&runner);
        if hydra_first {
            choose_source(&mut runner, &mut events, hydra);
        } else {
            choose_not_source(&mut runner, &mut events, hydra);
        }

        assert_eq!(
            plus_counters(&runner, hydra),
            if hydra_first { 3 } else { 6 }
        );
        assert_eq!(life(&runner, P0), 23);
        assert_at_rest(&runner);
    }

    for lich_first in [true, false] {
        let mut scenario = self::scenario();
        let lich = nefarious_lich(&mut scenario, 7);
        furnace(&mut scenario);
        let spell = helix(&mut scenario);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![player_target(P1)]);
        assert_prompt_is_replacement_choice(&runner);
        if lich_first {
            choose_source(&mut runner, &mut events, lich);
        } else {
            choose_not_source(&mut runner, &mut events, lich);
        }
        take_offered(&mut runner, &mut events);

        assert_eq!(life(&runner, P0), 23);
        assert!(!player(&runner, P1).is_eliminated);
        assert_eq!(
            player(&runner, P1).graveyard.len(),
            if lich_first { 4 } else { 1 }
        );
        assert_at_rest(&runner);
    }
}

/// Flame Rift: "deals 4 damage to each player." P0's Nefarious Lich asks which 4 of P0's 6
/// graveyard cards to exile, and P1's 4 meets P1's Nefarious Lich and Force Bubble, an order P1
/// chooses (CR 616.1). One prompt stands at a time: P0 answers first (CR 616.1: "choices are
/// made in APNAP order"), and P1's order prompt follows that answer instead of replacing it
/// (CR 608.2c).
#[test]
fn a_riders_prompt_holds_the_next_players_order_prompt() {
    let mut scenario = scenario();
    for index in 0..6 {
        scenario.add_creature_to_graveyard(P0, &format!("P0 Filler {index}"), 1, 1);
    }
    scenario.add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH);
    nefarious_lich(&mut scenario, 5);
    let bubble = force_bubble(&mut scenario, P1);
    let spell = sorcery(&mut scenario, "Flame Rift", FLAME_RIFT);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![]);

    assert_eq!(zone_prompt(&runner), (4, 6));
    assert_eq!(player(&runner, P1).graveyard.len(), 5);
    assert_eq!(counters(&runner, bubble, &depletion()), 0);
    assert!(runner.state().pending_replacement.is_none());
    take_offered(&mut runner, &mut events);
    assert_eq!(player(&runner, P0).graveyard.len(), 2);

    assert_prompt_is_replacement_choice(&runner);
    choose_source(&mut runner, &mut events, bubble);
    // Force Bubble took all 4 as depletion counters, so its own trigger sacrificed it; P1's
    // graveyard is the 5 cards Nefarious Lich didn't exile, and Force Bubble.
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::CounterAdded { object_id, count: 4, .. } if *object_id == bubble
    )));
    assert_eq!(zone(&runner, bubble), Zone::Graveyard);
    assert_eq!(player(&runner, P1).graveyard.len(), 6);
    assert_eq!((life(&runner, P0), life(&runner, P1)), (20, 20));
    assert!(resolved(&events, EffectKind::DamageEachPlayer));
    assert_at_rest(&runner);

    // Creeping Chill, with two opponents: "deals 3 damage to each opponent and you gain 3
    // life." P1's Nefarious Lich asks which 3 cards; P2's 3 meets P2's Nefarious Lich and Force
    // Bubble. The gain is the spell's next instruction (CR 608.2c): it follows both answers, once.
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    nefarious_lich(&mut scenario, 4);
    for index in 0..5 {
        scenario.add_creature_to_graveyard(p2, &format!("P2 Filler {index}"), 1, 1);
    }
    scenario.add_enchantment_from_oracle(p2, "Nefarious Lich", NEFARIOUS_LICH);
    let bubble = force_bubble(&mut scenario, p2);
    let spell = sorcery(&mut scenario, "Creeping Chill", CHILL);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![]);

    assert_eq!(zone_prompt(&runner), (3, 4));
    assert_eq!(life(&runner, P0), 20);
    take_offered(&mut runner, &mut events);
    assert_eq!(player(&runner, P1).graveyard.len(), 1);
    assert_eq!(life(&runner, P0), 20);

    assert_prompt_is_replacement_choice(&runner);
    choose_source(&mut runner, &mut events, bubble);
    assert_eq!(counters(&runner, bubble, &depletion()), 3);
    assert_eq!(player(&runner, p2).graveyard.len(), 5);
    assert_eq!(life(&runner, P0), 23);
    assert_eq!(life_gains(&events), 1);
    assert_at_rest(&runner);
}

/// Flame Rift cast by P1 on P1's turn. Each player's 4 meets that player's Nefarious Lich and
/// Force Bubble, so each player chooses one (CR 616.1), and "choices are made in APNAP order":
/// P1, the active player, first, then P0 (CR 101.4).
#[test]
fn simultaneous_replacement_choices_follow_apnap_order() {
    let mut scenario = scenario();
    for index in 0..5 {
        scenario.add_creature_to_graveyard(P0, &format!("P0 Filler {index}"), 1, 1);
    }
    scenario.add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH);
    let p0_bubble = force_bubble(&mut scenario, P0);
    nefarious_lich(&mut scenario, 5);
    let p1_bubble = force_bubble(&mut scenario, P1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Flame Rift", false, FLAME_RIFT)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let mut events = cast(&mut runner, spell, vec![]);

    let mut choosers = Vec::new();
    while let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for.clone() {
        choosers.push(player);
        let bubble = if player == P1 { p1_bubble } else { p0_bubble };
        choose_source(&mut runner, &mut events, bubble);
    }

    assert_eq!(choosers, vec![P1, P0]);
    for bubble in [p0_bubble, p1_bubble] {
        assert_eq!(zone(&runner, bubble), Zone::Graveyard);
    }
    assert_eq!((life(&runner, P0), life(&runner, P1)), (20, 20));
    assert_at_rest(&runner);
}

/// Rakdos Charm: "Each creature deals 1 damage to its controller." P0's Rhox Faithmender
/// (lifelink) deals 1 to P0, and that gain meets Rhox Faithmender and Leyline of Hope (CR 616.1).
/// P1's Bear, the batch's last source, deals 1 to P1, whose Nefarious Lich asks which of 3
/// graveyard cards to exile. Neither answer replaces the other (CR 608.2c): P1 exiles one card,
/// and P0 loses 1 and gains 3 or 4, in the order P0 chose.
#[test]
fn a_last_sources_rider_prompt_survives_an_earlier_lifelink_choice() {
    let mut scenario = scenario();
    let rhox =
        super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
    nefarious_lich(&mut scenario, 3);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let charm = sorcery(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    let graveyard: Vec<ObjectId> = player(&runner, P1).graveyard.iter().copied().collect();
    let action = cast_action(&runner, charm);
    let mut events = runner.act(action).expect("cast Rakdos Charm").events;
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ModeChoice { .. }),
        "got {:?}",
        runner.state().waiting_for
    );
    answer(
        &mut runner,
        GameAction::SelectModes { indices: vec![2] },
        &mut events,
    );

    let (mut exiles, mut orders) = (0, 0);
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { .. } => {
                assert_eq!(zone_prompt(&runner), (1, 3));
                exiles += 1;
                take_offered(&mut runner, &mut events);
            }
            WaitingFor::ReplacementChoice { .. } => {
                orders += 1;
                answer(
                    &mut runner,
                    GameAction::ChooseReplacement { index: 0 },
                    &mut events,
                );
            }
            _ => break,
        }
    }

    assert_eq!((exiles, orders), (1, 1));
    let exiled = graveyard
        .iter()
        .filter(|card| zone(&runner, **card) == Zone::Exile)
        .count();
    assert_eq!(exiled, 1);
    assert_eq!(life(&runner, P1), 20);
    let p0 = life(&runner, P0);
    assert!([22, 23].contains(&p0), "got {p0}");
    assert_eq!(damage(&runner, rhox), 0);
    assert_eq!(damage(&runner, bear), 0);
    assert_at_rest(&runner);
}

/// Rakdos Charm: "Each creature deals 1 damage to its controller." Every portion goes through
/// replacement before any is dealt (CR 120.4b: damage is dealt "as modified by replacement and
/// prevention effects that interact with damage"; CR 120.4c then processes it "into its
/// results"), and a pause keeps that order. P0's Rhox Faithmender (lifelink) is first: its
/// portion survives, and its gain will meet Rhox Faithmender and Leyline of Hope. P1's Bear is
/// next: Nefarious Lich replaces its damage. P2's Hill Giant is last: P2's 1 meets Force Bubble
/// and Delaying Shield, an order P2 chooses (CR 616.1). P2's order, an original choice, is asked
/// first; then, with the damage, P0's gain choice and the Lich's exile (its substitution's
/// action, CR 614.6), in APNAP order (CR 101.4). No damage is dealt before P2's answer, and the
/// instruction completes once, after the last.
#[test]
fn a_paused_batch_finishes_its_replacements_before_dealing_damage() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
    nefarious_lich(&mut scenario, 3);
    scenario.add_creature(P1, "Bear", 2, 2);
    scenario.add_creature(p2, "Hill Giant", 3, 3);
    let bubble = force_bubble(&mut scenario, p2);
    scenario.add_enchantment_from_oracle(p2, "Delaying Shield", DELAYING_SHIELD);
    let charm = sorcery(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    let action = cast_action(&runner, charm);
    let mut events = runner.act(action).expect("cast Rakdos Charm").events;
    answer(
        &mut runner,
        GameAction::SelectModes { indices: vec![2] },
        &mut events,
    );

    let mut prompts = Vec::new();
    let mut dealt_before_last_replacement_answer = 0;
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { player, .. } => {
                prompts.push(("exile", player));
                take_offered(&mut runner, &mut events);
            }
            WaitingFor::ReplacementChoice { player, .. } if player == p2 => {
                prompts.push(("order", player));
                dealt_before_last_replacement_answer = dealt(&events);
                choose_source(&mut runner, &mut events, bubble);
            }
            WaitingFor::ReplacementChoice { player, .. } => {
                prompts.push(("gain", player));
                answer(
                    &mut runner,
                    GameAction::ChooseReplacement { index: 0 },
                    &mut events,
                );
            }
            _ => break,
        }
    }

    assert_eq!(prompts, vec![("order", p2), ("gain", P0), ("exile", P1)]);
    assert_eq!(dealt_before_last_replacement_answer, 0);
    assert_eq!(dealt(&events), 1, "only Rhox Faithmender's 1 is dealt");
    assert_eq!(counters(&runner, bubble, &depletion()), 1);
    let p0 = life(&runner, P0);
    assert!([22, 23].contains(&p0), "got {p0}");
    let completions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e,
                GameEvent::EffectResolved { kind: EffectKind::EachSourceDealsDamage, source_id, .. }
                    if *source_id == charm
            )
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(completions.len(), 1, "one completion for the instruction");
    let last_dealt = events
        .iter()
        .rposition(|e| matches!(e, GameEvent::DamageDealt { .. }))
        .expect("damage was dealt");
    assert!(completions[0] > last_dealt);
    assert_at_rest(&runner);
}

/// What a batch's prompts were, in order: the prompt kind, who answered, and how many
/// `DamageDealt` events had happened by then.
type BatchPrompts = Vec<(&'static str, PlayerId, usize)>;

/// Answer every prompt a resolving batch raises: an exile choice with the first offered cards,
/// an order choice by the candidate `pick` returns (else the first), and record each.
fn answer_batch_prompts(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    pick: impl Fn(PlayerId, &[ReplacementCandidateSummary]) -> Option<ObjectId>,
) -> BatchPrompts {
    let mut prompts = Vec::new();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { player, .. } => {
                prompts.push(("exile", player, dealt(events)));
                take_offered(runner, events);
            }
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                prompts.push(("order", player, dealt(events)));
                let index = pick(player, &candidates)
                    .and_then(|source| candidates.iter().position(|c| c.source_id == source))
                    .unwrap_or(0);
                answer(runner, GameAction::ChooseReplacement { index }, events);
            }
            _ => break,
        }
    }
    prompts
}

/// The instruction's completion events: `kind` from `source`.
fn completions(events: &[GameEvent], kind: EffectKind, source: ObjectId) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(e, GameEvent::EffectResolved { kind: k, source_id, .. }
                if *k == kind && *source_id == source)
        })
        .map(|(index, _)| index)
        .collect()
}

/// The instruction completes once, after its last damage.
#[track_caller]
fn assert_completes_once_after_its_damage(
    events: &[GameEvent],
    kind: EffectKind,
    source: ObjectId,
) {
    let completions = completions(events, kind, source);
    assert_eq!(completions.len(), 1, "{completions:?}");
    if let Some(last_dealt) = events
        .iter()
        .rposition(|e| matches!(e, GameEvent::DamageDealt { .. }))
    {
        assert!(completions[0] > last_dealt);
    }
}

fn cast_rakdos_charm_damage_mode(runner: &mut GameRunner, charm: ObjectId) -> Vec<GameEvent> {
    let action = cast_action(runner, charm);
    let mut events = runner.act(action).expect("cast Rakdos Charm").events;
    answer(
        runner,
        GameAction::SelectModes { indices: vec![2] },
        &mut events,
    );
    events
}

/// CR 614.6 + CR 104.1: Nefarious Lich's "If you can't, you lose the game" is its substitution's
/// action, part of the batch's one simultaneous event, so P0's cached survivor is dealt with it
/// (Dralnu ruling: "dealt damage at the same time"). The loss then ends the game: nothing after
/// it, and no completion. Exercises run_damage_batch's post-rider terminal guard.
#[test]
fn a_substitution_loss_comes_with_the_batchs_damage() {
    let mut scenario = scenario();
    let cached =
        super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
    let lich = nefarious_lich(&mut scenario, 0);
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    assert!(player(&runner, P1).graveyard.is_empty());
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    answer_batch_prompts(&mut runner, &mut events, |_, _| None);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::PlayerEliminated { player_id } if *player_id == P1)));
    assert!(events.iter().any(
        |e| matches!(e, GameEvent::ReplacementApplied { source_id, .. } if *source_id == lich)
    ));
    let over = events
        .iter()
        .position(|e| matches!(e, GameEvent::GameOver { winner: Some(P0) }))
        .expect("P0 won with the batch's damage");
    assert!(!events[over + 1..]
        .iter()
        .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
    assert!(events[..over]
        .iter()
        .any(|e| matches!(e, GameEvent::DamageDealt { source_id, .. } if *source_id == cached)));
    assert!(completions(&events, EffectKind::EachSourceDealsDamage, charm).is_empty());
    // P0's 1 and its doubled lifelink gain (Rhox Faithmender, Leyline of Hope).
    let p0 = life(&runner, P0);
    assert!([22, 23].contains(&p0), "got {p0}");
}

/// CR 120.4 + CR 702.15b + CR 104.4a: Allies at Last's two sources deal their damage as one
/// simultaneous event. The Vampire's lifelink gain becomes a draw under Nefarious Lich and a win
/// under Laboratory Maniac; that win settles when the event does, so the Bears' damage is dealt
/// with it, one win is published, and nothing after it runs (CR 104.1). With Rhox Faithmender and
/// Leyline of Hope too, P0 orders the gain's replacements and picks the Lich: the win then comes
/// inside that answer, while the batch is parked, and still waits for the event.
#[test]
fn a_lifelink_win_settles_with_the_batchs_damage() {
    for ordered in [false, true] {
        let mut scenario = scenario();
        if ordered {
            super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
        }
        let vampire = super::unpreventable_damage_prevention_effects::creature_with_keywords(
        &mut scenario, P0, "Vampire of the Dire Moon", (1, 1),
        &["Deathtouch", "Lifelink"],
        "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)\nLifelink (Damage dealt by this creature also causes you to gain that much life.)",
    );
        let second = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let target = scenario.add_creature(P1, "Wall", 0, 20).id();
        let lich = scenario
            .add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH)
            .id();
        let maniac = scenario.add_creature_from_oracle(P0, "Laboratory Maniac", 2, 2,
        "If you would draw a card while your library has no cards in it, you win the game instead.").id();
        let spell = sorcery(&mut scenario, "Allies at Last", ALLIES_AT_LAST);
        let mut runner = scenario.build();
        assert!(player(&runner, P0).library.is_empty());
        let mut events = cast(
            &mut runner,
            spell,
            vec![
                object_target(vampire),
                object_target(second),
                object_target(target),
            ],
        );
        let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| Some(lich));
        if ordered {
            assert!(
                prompts
                    .iter()
                    .any(|(kind, player, _)| *kind == "order" && *player == P0),
                "reach: P0 orders the gain replacements and picks the Lich: {prompts:?}"
            );
        }

        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ));
        let over = events
            .iter()
            .position(|e| matches!(e, GameEvent::GameOver { winner: Some(P0) }))
            .expect("P0 won during lifelink");
        assert!(events[..over].iter().any(|e| matches!(e, GameEvent::DamageDealt { source_id, amount: 1, .. } if *source_id == vampire)));
        for (source, kind) in [(lich, "GainLife"), (maniac, "Draw")] {
            assert!(events[..over].iter().any(|e| matches!(e, GameEvent::ReplacementApplied { source_id, event_type } if *source_id == source && event_type == kind)));
        }
        assert!(events[..over].iter().any(
        |e| matches!(e, GameEvent::DamageDealt { source_id, amount: 2, .. } if *source_id == second)
    ));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::GameOver { .. }))
                .count(),
            1
        );
        assert!(!events[over + 1..]
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
        assert!(completions(&events, EffectKind::EachDealsDamageEqualToPower, spell).is_empty());
        // CR 104.2b: P0's win makes its opponent lose.
        assert_eq!(
            player_losses(&events),
            vec![(P1, BTreeSet::from([LossReason::OpponentWon]))]
        );
    }
}

/// CR 104.3f: a player who would both win and lose in one simultaneous event loses. Rakdos Charm's
/// "Each creature deals 1 damage to its controller" with P0's Vampire of the Dire Moon (lifelink),
/// Grizzly Bears, Laboratory Maniac, Nefarious Lich, and Pariah on P0's Wall; P0's library and
/// graveyard are empty. P0 sends the Vampire's 1 to the Wall with Pariah, so its lifelink gain
/// becomes the Lich's draw and Laboratory Maniac's win; the Bears' and the Maniac's damage meet
/// the Lich, which can't exile, so P0 loses. P0 loses rather than winning, and P1 wins.
#[test]
fn a_player_who_wins_and_loses_in_one_batch_loses() {
    let mut scenario = scenario();
    let vampire = super::unpreventable_damage_prevention_effects::creature_with_keywords(
        &mut scenario, P0, "Vampire of the Dire Moon", (1, 1),
        &["Deathtouch", "Lifelink"],
        "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)\nLifelink (Damage dealt by this creature also causes you to gain that much life.)",
    );
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature_from_oracle(
        P0,
        "Laboratory Maniac",
        2,
        2,
        "If you would draw a card while your library has no cards in it, you win the game instead.",
    );
    let wall = scenario.add_creature(P0, "Wall", 0, 20).id();
    let pariah = scenario
        .add_enchantment_from_oracle(P0, "Pariah", PARIAH)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
        .id();
    let lich = scenario
        .add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH)
        .id();
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), pariah, wall);
    assert!(player(&runner, P0).library.is_empty());
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);

    // The Vampire's portion is asked first (it was created first): Pariah; the others: the Lich.
    let pariah_picked = std::cell::Cell::new(false);
    let prompts = answer_batch_prompts(&mut runner, &mut events, |_, candidates| {
        if !pariah_picked.get() && candidates.iter().any(|c| c.source_id == pariah) {
            pariah_picked.set(true);
            Some(pariah)
        } else {
            Some(lich)
        }
    });

    assert!(
        events.iter().any(
            |e| matches!(e, GameEvent::DamageDealt { source_id, .. } if *source_id == vampire)
        ),
        "reach: the Vampire's 1 is dealt to the Wall: {prompts:?}"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::PlayerEliminated { player_id } if *player_id == P0)));
    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::PlayerEliminated { player_id } if *player_id == P1)));
    // CR 104.3e + CR 104.3f: P0 loses to the effect, and its win counts for nothing.
    assert_eq!(
        player_losses(&events),
        vec![(P0, BTreeSet::from([LossReason::EffectStatedLoss]))]
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P1) }
    ));
}

/// CR 810.8a + CR 104.3f: in Two-Headed Giant a team wins and loses only as a team. P0 and P1
/// are one team, P2 and P3 the other. Under Rakdos Charm's "Each creature deals 1 damage to its
/// controller", P0 sends its Vampire of the Dire Moon's and Laboratory Maniac's damage to its Wall
/// with Pariah, so the Vampire's lifelink becomes the Lich's draw and Laboratory Maniac's win;
/// P1's Grizzly Bears meet P1's Nefarious Lich, which can't exile, so P1 loses. P1's loss is the
/// team's, so P0 would both win and lose and loses: P0's team loses and P2's team wins. The test
/// first requires both outcomes (the draw-to-win and the Lich's loss), so neither alone passes it.
#[test]
fn a_two_headed_giant_team_that_wins_and_loses_in_one_batch_loses() {
    let mut scenario = GameScenario::new_with_format(
        engine::types::format::FormatConfig::two_headed_giant(),
        4,
        42,
    );
    scenario.at_phase(Phase::PreCombatMain);
    let vampire = super::unpreventable_damage_prevention_effects::creature_with_keywords(
        &mut scenario, P0, "Vampire of the Dire Moon", (1, 1),
        &["Deathtouch", "Lifelink"],
        "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)\nLifelink (Damage dealt by this creature also causes you to gain that much life.)",
    );
    let maniac = scenario
        .add_creature_from_oracle(
            P0,
            "Laboratory Maniac",
            2,
            2,
            "If you would draw a card while your library has no cards in it, you win the game instead.",
        )
        .id();
    let wall = scenario.add_creature(P0, "Wall", 0, 20).id();
    let pariah = scenario
        .add_enchantment_from_oracle(P0, "Pariah", PARIAH)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
        .id();
    let p0_lich = scenario
        .add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH)
        .id();
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let p1_lich = scenario
        .add_enchantment_from_oracle(P1, "Nefarious Lich", NEFARIOUS_LICH)
        .id();
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), pariah, wall);
    assert!(player(&runner, P0).library.is_empty());
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);

    // P0's portions: Pariah over the Lich, sending them to the Wall.
    let prompts = answer_batch_prompts(&mut runner, &mut events, |_, candidates| {
        candidates
            .iter()
            .any(|c| c.source_id == pariah)
            .then_some(pariah)
    });

    assert!(
        events.iter().any(
            |e| matches!(e, GameEvent::DamageDealt { source_id, .. } if *source_id == vampire)
        ),
        "reach: the Vampire's 1 is dealt to the Wall: {prompts:?}"
    );
    // Both outcomes happened: P0's lifelink gain became the Lich's draw and Laboratory Maniac's
    // win, and P1's Lich replaced the Bears' damage and its loss resolved.
    let applied = |source: ObjectId, kind: &str| {
        events.iter().any(|e| {
            matches!(e, GameEvent::ReplacementApplied { source_id, event_type }
                if *source_id == source && event_type == kind)
        })
    };
    assert!(
        applied(p0_lich, "GainLife"),
        "reach: P0's gain becomes a draw"
    );
    assert!(
        applied(maniac, "Draw"),
        "reach: the draw becomes Laboratory Maniac's win"
    );
    assert!(
        applied(p1_lich, "DamageDone"),
        "reach: P1's Lich replaces the Bears' damage"
    );
    for kind in [EffectKind::WinTheGame, EffectKind::LoseTheGame] {
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::EffectResolved { kind: k, .. } if *k == kind)),
            "reach: {kind:?} resolved"
        );
    }
    for loser in [P0, P1] {
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::PlayerEliminated { player_id } if *player_id == loser)
            ),
            "{loser:?} loses with its team"
        );
    }
    for winner in [PlayerId(2), PlayerId(3)] {
        assert!(
            !events.iter().any(
                |e| matches!(e, GameEvent::PlayerEliminated { player_id } if *player_id == winner)
            ),
            "{winner:?}'s team wins"
        );
    }
    // CR 104.3e + CR 810.8a: the team loses to the effect, each head for that reason.
    assert_eq!(
        player_losses(&events),
        vec![
            (P0, BTreeSet::from([LossReason::EffectStatedLoss])),
            (P1, BTreeSet::from([LossReason::EffectStatedLoss])),
        ]
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(_) }
    ));
}

/// Both players' Nefarious Lich, each with an empty graveyard, under Rakdos Charm's "Each creature
/// deals 1 damage to its controller". `p0_cards` puts cards in P0's graveyard for P0's first
/// exiles, so a substitution action pauses on an exile choice before P0's loss.
fn two_lich_charm(
    active: PlayerId,
    p0_creatures: usize,
    p0_cards: usize,
) -> (GameRunner, Vec<GameEvent>) {
    let mut scenario = scenario();
    for index in 0..p0_creatures {
        scenario.add_creature(P0, &format!("Bear {index}"), 2, 2);
    }
    for index in 0..p0_cards {
        scenario.add_creature_to_graveyard(P0, &format!("Filler {index}"), 1, 1);
    }
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    for player in [P0, P1] {
        scenario.add_enchantment_from_oracle(player, "Nefarious Lich", NEFARIOUS_LICH);
    }
    let charm = scenario
        .add_spell_to_hand_from_oracle(active, "Rakdos Charm", true, RAKDOS_CHARM)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = active;
    runner.state_mut().priority_player = active;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: active };
    let action = cast_action(&runner, charm);
    let mut events = runner.act(action).expect("cast Rakdos Charm").events;
    answer(
        &mut runner,
        GameAction::SelectModes { indices: vec![2] },
        &mut events,
    );
    (runner, events)
}

/// CR 614.6 + CR 101.4 + CR 104.4a: two Nefarious Lich substitutions in one batch are one
/// simultaneous event, so both players lose together and the game is a draw, with either seat
/// active. In the third case P1 is active and P0 has three creatures and two graveyard cards: P1's
/// loss is recorded first, P0's first exile asks which card, so the event pauses there, and P0
/// loses at its third exile.
#[test]
fn two_lich_losses_in_one_batch_are_a_draw() {
    for (active, p0_creatures, p0_cards) in [(P0, 1, 0), (P1, 1, 0), (P1, 3, 2)] {
        let (mut runner, mut events) = two_lich_charm(active, p0_creatures, p0_cards);
        let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| None);

        let case = (active, p0_creatures, p0_cards);
        if p0_cards > 0 {
            assert!(
                prompts
                    .iter()
                    .any(|(kind, player, _)| *kind == "exile" && *player == P0),
                "{case:?}: reach: P0's exile pauses the event: {prompts:?}"
            );
        }
        for player in [P0, P1] {
            assert!(
                events.iter().any(|e| matches!(e, GameEvent::PlayerEliminated { player_id } if *player_id == player)),
                "{case:?}: {player:?} lost"
            );
        }
        let game_overs: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::GameOver { winner } => Some(*winner),
                _ => None,
            })
            .collect();
        assert_eq!(game_overs, vec![None], "{case:?}");
        // CR 104.3e + CR 104.4a: both lose to the effect at once.
        assert_eq!(
            player_losses(&events),
            vec![
                (P0, BTreeSet::from([LossReason::EffectStatedLoss])),
                (P1, BTreeSet::from([LossReason::EffectStatedLoss])),
            ],
            "{case:?}"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: None }
        ));
    }
}

/// CR 800.4a: P1 loses to Nefarious Lich's substitution action, which comes with the batch's
/// damage (CR 614.6); the other players' portions are dealt and the batch completes for them.
#[test]
fn a_batch_continues_for_survivors_after_one_of_three_players_loses() {
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let cached =
        super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
    nefarious_lich(&mut scenario, 0);
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    let departed = scenario.add_creature(P1, "Grey Ogre", 2, 2).id();
    let survivor = scenario.add_creature(P2, "Runeclaw Bear", 2, 2).id();
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| None);
    assert_eq!(prompts, vec![("order", P0, 1)]);

    let lost = events
        .iter()
        .position(|e| matches!(e, GameEvent::PlayerEliminated { player_id } if *player_id == P1))
        .expect("P1 lost with the batch's damage");
    assert!(!engine::game::players::is_alive(runner.state(), P1));
    assert!(runner.state().game_end.is_none());
    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::GameOver { .. })));
    // CR 614.6: the Lich's loss is its substitution's action, with the batch's damage: the
    // survivors' portions are dealt, P1's never is.
    for (source, recipient) in [(cached, P0), (survivor, P2)] {
        assert!(events[..lost].iter().any(|e| matches!(e, GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 1, .. } if *source_id == source && *player == recipient)));
    }
    assert_eq!(life(&runner, P2), 19);
    assert!(!events.iter().any(|e| matches!(e, GameEvent::DamageDealt { source_id, target, .. } if *source_id == departed || *target == player_target(P1))));
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// CR 616.1: affected players choose in APNAP order, regardless of source insertion order.
/// Exercises in_apnap_order at the shared batch boundary.
#[test]
fn a_creature_damage_batch_asks_the_active_player_before_the_first_source_owner() {
    let mut scenario = scenario();
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    let p0_bubble = force_bubble(&mut scenario, P0);
    let p1_bubble = force_bubble(&mut scenario, P1);
    for player in [P0, P1] {
        scenario.add_enchantment_from_oracle(player, "Delaying Shield", DELAYING_SHIELD);
    }
    let charm = scenario
        .add_spell_to_hand_from_oracle(P1, "Rakdos Charm", true, RAKDOS_CHARM)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    let prompts = answer_batch_prompts(&mut runner, &mut events, |player, _| {
        Some(if player == P1 { p1_bubble } else { p0_bubble })
    });
    assert_eq!(prompts, vec![("order", P1, 0), ("order", P0, 0)]);
    assert_eq!(counters(&runner, p0_bubble, &depletion()), 1);
    assert_eq!(counters(&runner, p1_bubble, &depletion()), 1);
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// Rakdos Charm with two lifelink survivors under Rhox Faithmender and Leyline of Hope: each
/// survivor's gain asks P0 an order (CR 616.1), in delivery (CR 120.4c). The instruction
/// completes once, after the second answer; with only the last survivor asking, once after its
/// answer.
#[test]
fn a_batchs_delivery_choices_keep_its_single_completion() {
    for second_lifelinker in [true, false] {
        let mut scenario = scenario();
        super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
        if second_lifelinker {
            scenario
                .add_creature(P0, "Vampire", 2, 2)
                .with_keyword(Keyword::Lifelink);
        }
        let charm = sorcery(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
        let mut runner = scenario.build();
        let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);

        let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| None);

        let gains = if second_lifelinker { 2 } else { 1 };
        assert_eq!(prompts.len(), gains, "{prompts:?}");
        assert!(prompts
            .iter()
            .all(|(kind, player, _)| *kind == "order" && *player == P0));
        assert_eq!(dealt(&events), gains);
        assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
        assert_at_rest(&runner);
    }
}

/// Rakdos Charm: P0's Rhox Faithmender survives first. P1's Bear meets P1's Nefarious Lich and
/// Force Bubble; P1 orders the Lich first (CR 616.1). P2's Hill Giant, last, still goes through
/// replacement before anything is dealt. With P2 under Force Bubble and Delaying Shield too, P2
/// orders after P1 (two order choices). The Lich's exile, its substitution's action, comes with
/// the batch's damage (CR 614.6), after P0's gain choice (CR 101.4).
#[test]
fn a_batch_keeps_its_order_through_an_answer_that_pauses_again() {
    for p2_orders in [false, true] {
        let p2 = PlayerId(2);
        let mut scenario = GameScenario::new_n_player(3, 11);
        scenario.at_phase(Phase::PreCombatMain);
        super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
        let lich = nefarious_lich(&mut scenario, 3);
        force_bubble(&mut scenario, P1);
        scenario.add_creature(P1, "Bear", 2, 2);
        scenario.add_creature(p2, "Hill Giant", 3, 3);
        let p2_bubble = p2_orders.then(|| {
            scenario.add_enchantment_from_oracle(p2, "Delaying Shield", DELAYING_SHIELD);
            force_bubble(&mut scenario, p2)
        });
        let charm = sorcery(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
        let mut runner = scenario.build();
        let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);

        let prompts = answer_batch_prompts(&mut runner, &mut events, |player, _| {
            if player == P1 {
                Some(lich)
            } else if player == p2 {
                p2_bubble
            } else {
                None
            }
        });

        let mut expected: Vec<(&str, PlayerId, usize)> = vec![("order", P1, 0)];
        if p2_orders {
            expected.push(("order", p2, 0));
        }
        // Rhox Faithmender's gain is asked in delivery, after every replacement answer, once
        // its own 1 is dealt. The Lich's exile is its substitution's action, which happens with
        // the batch's damage (CR 614.6), after the gain in APNAP order (CR 101.4).
        expected.push(("order", P0, 1));
        // By then P0's 1 is dealt, and P2's 3 too unless P2's shields replaced it.
        expected.push(("exile", P1, if p2_orders { 1 } else { 2 }));
        assert_eq!(prompts, expected, "p2 orders {p2_orders}");
        assert_eq!(player(&runner, P1).graveyard.len(), 2);
        assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
        assert_at_rest(&runner);
    }
}

/// Rakdos Charm with a redirect answer. P1's Pariah ("All damage that would be dealt to you is
/// dealt to enchanted creature instead.") enchants P1's Wall, and P0 controls Furnace of Rath.
/// P0's lifelink Rhox Faithmender survives first. P1's two Bears each deal 1 to P1, which meets
/// Pariah and Furnace of Rath, an order P1 chooses (CR 616.1); P1 picks Pariah, which sends the
/// damage to the Wall. Each redirected portion still waits for the batch's other replacements,
/// so nothing is dealt before P1's last answer (CR 120.4b), and Rhox Faithmender's gain is asked
/// in delivery, after both (CR 120.4c).
#[test]
fn a_redirected_answer_still_waits_for_the_batch() {
    let mut scenario = scenario();
    super::unpreventable_damage_prevention_effects::faithmender_and_leyline(&mut scenario);
    let wall = scenario.add_creature(P1, "Wall", 0, 20).id();
    let pariah = scenario
        .add_enchantment_from_oracle(P1, "Pariah", PARIAH)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
        .id();
    furnace(&mut scenario);
    scenario.add_creature(P1, "Bear A", 2, 2);
    scenario.add_creature(P1, "Bear B", 2, 2);
    let charm = sorcery(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), pariah, wall);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);

    let prompts = answer_batch_prompts(&mut runner, &mut events, |player, _| {
        (player == P1).then_some(pariah)
    });

    let p1_orders: Vec<usize> = prompts
        .iter()
        .filter(|(kind, player, _)| *kind == "order" && *player == P1)
        .map(|(_, _, dealt)| *dealt)
        .collect();
    assert!(!p1_orders.is_empty(), "{prompts:?}");
    assert!(p1_orders.iter().all(|dealt| *dealt == 0), "{prompts:?}");
    let gain = prompts
        .iter()
        .position(|(kind, player, _)| *kind == "order" && *player == P0)
        .expect("Rhox Faithmender's gain asks P0");
    assert!(
        prompts[..gain].iter().all(|(_, player, _)| *player == P1),
        "{prompts:?}"
    );
    assert_eq!(life(&runner, P1), 20);
    assert!(damage(&runner, wall) > 0);
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// Allies at Last: "Up to two target creatures you control each deal damage equal to their
/// power to target creature an opponent controls." Coordinated Clobbering: "Tap one or two
/// target untapped creatures you control. They each deal damage equal to their power to target
/// creature an opponent controls." P0's lifelink Vampire's 2 meets only Furnace of Rath, and
/// survives; P0's red Hound's 2 meets Furnace of Rath and Torbran, Thane of Red Fell, an order P1
/// chooses for P1's creature (CR 616.1). Nothing is dealt before that answer (CR 120.4b), then
/// both are dealt and the Vampire's lifelink gains (CR 120.4c).
#[test]
fn a_targeted_source_batch_finishes_its_replacements_before_dealing_damage() {
    for (name, text, kind) in [
        (
            "Allies at Last",
            ALLIES_AT_LAST,
            EffectKind::EachDealsDamageEqualToPower,
        ),
        (
            "Coordinated Clobbering",
            COORDINATED_CLOBBERING,
            EffectKind::DealDamage,
        ),
    ] {
        let mut scenario = scenario();
        let vampire = scenario
            .add_creature(P0, "Vampire", 2, 2)
            .with_keyword(Keyword::Lifelink)
            .id();
        let hound = scenario
            .add_creature(P0, "Hound", 2, 2)
            .with_color(vec![ManaColor::Red])
            .id();
        let recipient = scenario.add_creature(P1, "Wall", 0, 20).id();
        furnace(&mut scenario);
        scenario.add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN);
        let spell = sorcery(&mut scenario, name, text);
        let mut runner = scenario.build();
        let mut events = cast(
            &mut runner,
            spell,
            vec![
                object_target(vampire),
                object_target(hound),
                object_target(recipient),
            ],
        );

        let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| None);

        assert_eq!(prompts, vec![("order", P1, 0)], "{name}");
        assert_eq!(dealt(&events), 2, "{name}");
        assert!(damage(&runner, recipient) >= 4 + 6, "{name}");
        assert_eq!(life(&runner, P0), 24, "{name}");
        assert_completes_once_after_its_damage(&events, kind, spell);
        assert_at_rest(&runner);
    }
}

/// Pyroclasm: "deals 2 damage to each creature." A rider's prompt doesn't stop the other
/// recipients, whichever is dealt damage first (CR 120.3, CR 608.2d): one dealt after Dralnu
/// waits behind its sacrifice answer (CR 608.2c), and is dealt once it is given.
#[test]
fn a_riders_prompt_does_not_stop_the_other_recipients() {
    for bear_first in [false, true] {
        let mut scenario = scenario();
        let bear = bear_first.then(|| scenario.add_creature(P0, "Bear", 2, 3).id());
        let id = dralnu(&mut scenario, P1);
        let bear = bear.unwrap_or_else(|| scenario.add_creature(P0, "Bear", 2, 3).id());
        lands(&mut scenario, P1, 3);
        let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![]);

        assert_eq!(zone_prompt(&runner), (2, 4));
        let before_answer = if bear_first { 2 } else { 0 };
        assert_eq!(
            damage(&runner, bear),
            before_answer,
            "bear first {bear_first}"
        );
        assert_eq!(damage(&runner, id), 0);
        take_offered(&mut runner, &mut events);
        assert_eq!(damage(&runner, bear), 2);
        assert_at_rest(&runner);
    }

    // A later creature that needs its own answer: P0's Phytohydra under Mending Hands, whose 2
    // meets the shield and the substitution (CR 616.1). Its order prompt follows Dralnu's
    // sacrifice answer instead of replacing it (CR 608.2c).
    let mut scenario = scenario();
    let id = dralnu(&mut scenario, P1);
    let hydra = phytohydra(&mut scenario, P0);
    lands(&mut scenario, P1, 3);
    let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
    let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
    let mut runner = scenario.build();
    cast(&mut runner, hands, vec![object_target(hydra)]);
    let mut events = cast(&mut runner, spell, vec![]);

    assert_eq!(zone_prompt(&runner), (2, 4));
    assert!(runner.state().pending_replacement.is_none());
    take_offered(&mut runner, &mut events);
    assert_eq!(damage(&runner, id), 0);
    assert_eq!(player(&runner, P1).graveyard.len(), 2);
    assert_prompt_is_replacement_choice(&runner);
    choose_not_shield(&mut runner, &mut events);
    assert_eq!(plus_counters(&runner, hydra), 2);
    assert_eq!(damage(&runner, hydra), 0);
    assert!(resolved(&events, EffectKind::DamageAll));
    assert_at_rest(&runner);

    let mut scenario = self::scenario();
    dralnu(&mut scenario, P1);
    let hydra = phytohydra(&mut scenario, P0);
    lands(&mut scenario, P1, 3);
    let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![]);

    assert_eq!(zone_prompt(&runner), (2, 4));
    take_offered(&mut runner, &mut events);
    assert_eq!(plus_counters(&runner, hydra), 2);
    assert_at_rest(&runner);
}

/// Shake the Foundations: "deals 1 damage to each creature without flying. / Draw a card." The
/// draw is the second instruction, so every recipient comes first, and it runs once (CR 608.2c).
#[test]
fn a_later_instruction_follows_the_remaining_recipients_once() {
    for dralnu_first in [true, false] {
        let mut scenario = scenario();
        let id = dralnu(&mut scenario, P1);
        let bear = scenario.add_creature(P0, "Bear", 2, 3).id();
        lands(&mut scenario, P1, 3);
        let doubler = furnace(&mut scenario);
        scenario.with_library_top(P0, &["One", "Two", "Three", "Four"]);
        let spell = instant(&mut scenario, "Shake the Foundations", SHAKE);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![]);

        assert_prompt_is_replacement_choice(&runner);
        if dralnu_first {
            choose_not_source(&mut runner, &mut events, doubler);
        } else {
            choose_source(&mut runner, &mut events, doubler);
        }
        let count = if dralnu_first { 1 } else { 2 };
        assert_eq!(zone_prompt(&runner), (count, 4));
        assert_eq!(damage(&runner, bear), 0);
        assert_eq!(zone(&runner, id), Zone::Battlefield);
        take_offered(&mut runner, &mut events);

        assert_eq!(order_labels(&events), ["sacrifice", "damage 2", "draw"]);
        assert_at_rest(&runner);
    }

    // The Bear first: its damage precedes the park.
    for dralnu_first in [true, false] {
        let mut scenario = scenario();
        let bear = scenario.add_creature(P0, "Bear", 2, 3).id();
        dralnu(&mut scenario, P1);
        lands(&mut scenario, P1, 3);
        let doubler = furnace(&mut scenario);
        scenario.with_library_top(P0, &["One", "Two", "Three", "Four"]);
        let spell = instant(&mut scenario, "Shake the Foundations", SHAKE);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![]);

        assert_prompt_is_replacement_choice(&runner);
        assert_eq!(damage(&runner, bear), 2);
        if dralnu_first {
            choose_not_source(&mut runner, &mut events, doubler);
        } else {
            choose_source(&mut runner, &mut events, doubler);
        }
        take_offered(&mut runner, &mut events);

        assert_eq!(order_labels(&events), ["damage 2", "sacrifice", "draw"]);
        assert_at_rest(&runner);
    }

    // Two Phytohydras, two parks in one resolution.
    let mut scenario = scenario();
    let first = phytohydra(&mut scenario, P0);
    let second = phytohydra(&mut scenario, P1);
    let doubler = furnace(&mut scenario);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four"]);
    let spell = instant(&mut scenario, "Shake the Foundations", SHAKE);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![]);
    choose_source(&mut runner, &mut events, doubler);
    assert_eq!(draws(&events), 0);
    choose_source(&mut runner, &mut events, doubler);

    assert_eq!(plus_counters(&runner, first), 2);
    assert_eq!(plus_counters(&runner, second), 2);
    assert_eq!(draws(&events), 1);
    assert_at_rest(&runner);

    // Creeping Chill: a `DamageEachPlayer` head parked at Force Bubble and Furnace of Rath.
    for bubble_first in [true, false] {
        let mut scenario = self::scenario();
        let bubble = force_bubble(&mut scenario, P1);
        let doubler = furnace(&mut scenario);
        let spell = sorcery(&mut scenario, "Creeping Chill", CHILL);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![]);
        choose_source(
            &mut runner,
            &mut events,
            if bubble_first { bubble } else { doubler },
        );

        assert_eq!(life(&runner, P0), 23);
        assert_eq!(life(&runner, P1), 20);
        if bubble_first {
            assert_eq!(counters(&runner, bubble, &depletion()), 3);
        } else {
            assert_eq!(zone(&runner, bubble), Zone::Graveyard);
        }
        assert_at_rest(&runner);
    }
}

/// Shake the Foundations cast while Ornithopter ("Flying", an artifact creature) attacks, under
/// Furnace of Rath, at Dralnu, Lich Lord and Sanwell, Avenger Ace ("As long as an artifact
/// creature you control is attacking, prevent all damage that would be dealt to Sanwell."). The
/// damage to every creature without flying is one simultaneous event (CR 608.2f, CR 700.1), so
/// Sanwell's guard is read as it opens: Dralnu's controller sacrificing the Ornithopter at the
/// order answer for Dralnu's portion (CR 616.1) doesn't strip Sanwell's shield, which still
/// applies against Furnace of Rath (CR 616.1e).
#[test]
fn a_substitution_inside_the_batch_does_not_strip_a_board_census_guard() {
    let mut scenario = scenario();
    let id = dralnu(&mut scenario, P0);
    let sanwell = scenario
        .add_creature_from_oracle(P0, "Sanwell, Avenger Ace", 3, 1, SANWELL)
        .id();
    let thopter = {
        let mut builder = scenario.add_creature_from_oracle(P0, "Ornithopter", 0, 2, ORNITHOPTER);
        builder.as_artifact_creature();
        builder.id()
    };
    let doubler = furnace(&mut scenario);
    scenario.with_library_top(P0, &["One", "Two"]);
    let spell = instant(&mut scenario, "Shake the Foundations", SHAKE);
    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(thopter, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");

    let mut events = cast(&mut runner, spell, vec![]);
    assert_prompt_is_replacement_choice(&runner);
    choose_not_source(&mut runner, &mut events, doubler);
    assert_eq!(zone_prompt(&runner), (1, 4));
    answer(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![thopter],
        },
        &mut events,
    );
    assert_eq!(zone(&runner, thopter), Zone::Graveyard);
    // Reach guard: with the guard read live, Furnace of Rath alone would apply and no order
    // would be asked for Sanwell's portion.
    assert_prompt_is_replacement_choice(&runner);
    choose_source(&mut runner, &mut events, sanwell);

    assert_eq!(zone(&runner, sanwell), Zone::Battlefield);
    assert_eq!(damage(&runner, sanwell), 0);
    assert_eq!(zone(&runner, id), Zone::Battlefield);
    assert_eq!(damage(&runner, id), 0);
    assert_eq!(draws(&events), 1);
    assert_at_rest(&runner);
}

fn chain_effects(chain: &ResolvedAbility) -> Vec<&Effect> {
    let mut effects = Vec::new();
    let mut node = Some(chain);
    while let Some(ability) = node {
        effects.push(&ability.effect);
        node = ability.sub_ability.as_deref();
    }
    effects
}

/// At the pause the parked chain is the remaining recipient, then Shake the Foundations' "Draw a
/// card." once. A tail ahead of that recipient would break CR 608.2c.
#[test]
fn the_parked_chain_holds_the_tail_once_behind_the_remaining_recipient() {
    let mut scenario = scenario();
    phytohydra(&mut scenario, P1);
    scenario.add_creature(P0, "Bear", 2, 3);
    furnace(&mut scenario);
    scenario.with_library_top(P0, &["One", "Two"]);
    let spell = instant(&mut scenario, "Shake the Foundations", SHAKE);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![]);

    assert_prompt_is_replacement_choice(&runner);
    let chain = &runner
        .state()
        .active_ability_continuation()
        .expect("the resolver parked the rest of the chain")
        .chain;
    let effects = chain_effects(chain);
    assert_eq!(effects.len(), 2, "got {effects:?}");
    assert!(matches!(effects[0], Effect::DealDamage { .. }));
    assert!(matches!(effects[1], Effect::Draw { .. }));
}

/// Hurloon Battle Hymn: "If this spell was kicked, you gain 4 life." The tail keeps the chain's
/// context through the park (CR 608.2c), and the unkicked game shows that it reads the kicker.
#[test]
fn a_gated_tail_reads_the_kicker_through_an_order_park() {
    for (kicked, hydra_first) in [(true, true), (true, false), (false, true)] {
        let mut scenario = scenario();
        let hydra = phytohydra(&mut scenario, P0);
        furnace(&mut scenario);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Hurloon Battle Hymn", true, HYMN)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            })
            .id();
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Red);
        }
        scenario.add_basic_land(P0, ManaColor::White);
        let mut runner = scenario.build();
        {
            let cast = runner.cast(spell).target_object(hydra);
            let _ = if kicked {
                cast.accept_optional().commit()
            } else {
                cast.decline_optional().commit()
            };
        }
        let mut events = Vec::new();
        settle(&mut runner, &mut Vec::new(), &mut events);
        assert_prompt_is_replacement_choice(&runner);
        if hydra_first {
            choose_source(&mut runner, &mut events, hydra);
        } else {
            choose_not_source(&mut runner, &mut events, hydra);
        }

        assert_eq!(
            plus_counters(&runner, hydra),
            if hydra_first { 4 } else { 8 }
        );
        assert_eq!(life(&runner, P0), if kicked { 24 } else { 20 });
        assert_at_rest(&runner);
    }
}

/// Nibelheim Aflame: "If this spell was cast from a graveyard, discard your hand and draw four
/// cards." follows all the damage, once, from a chain parked behind the remaining recipients
/// (CR 608.2c, CR 702.34a). The games cast from hand show that the tail reads the cast.
#[test]
fn a_gated_tail_behind_remaining_recipients_reads_the_cast() {
    for (flashback, two_hydras) in [(true, false), (true, true), (false, false), (false, true)] {
        let mut scenario = scenario();
        let chosen = scenario.add_creature(P0, "Bear", 2, 9).id();
        let hydra = phytohydra(&mut scenario, P1);
        let second = two_hydras.then(|| phytohydra(&mut scenario, P1));
        let bear = scenario.add_creature(P1, "Bear", 2, 9).id();
        let doubler = furnace(&mut scenario);
        scenario.add_card_to_hand(P0, "Kept");
        scenario.add_card_to_hand(P0, "Also Kept");
        scenario.with_library_top(P0, &["One", "Two", "Three", "Four", "Five", "Six"]);
        let spell = if flashback {
            scenario
                .add_spell_to_graveyard(P0, "Nibelheim Aflame", false)
                .from_oracle_text(NIBELHEIM)
                .id()
        } else {
            sorcery(&mut scenario, "Nibelheim Aflame", NIBELHEIM)
        };
        scenario.with_mana_pool(
            P0,
            (0..7)
                .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();

        let mut events = Vec::new();
        if flashback {
            let _ = runner
                .cast(spell)
                .casting_variant(CastingVariant::Flashback)
                .target_object(chosen)
                .commit();
            settle(&mut runner, &mut Vec::new(), &mut events);
        } else {
            events = cast(&mut runner, spell, vec![object_target(chosen)]);
        }

        assert_prompt_is_replacement_choice(&runner);
        choose_not_source(&mut runner, &mut events, doubler);
        if let Some(second) = second {
            assert_eq!(damage(&runner, bear), 0);
            assert_eq!(plus_counters(&runner, second), 0);
            choose_not_source(&mut runner, &mut events, doubler);
            assert_eq!(plus_counters(&runner, second), 2);
        }

        assert_eq!(plus_counters(&runner, hydra), 2);
        assert_eq!(damage(&runner, bear), 4);
        let hand = player(&runner, P0).hand.len();
        if flashback {
            assert_eq!(
                order_labels(&events),
                ["damage 4", "discard", "draw", "draw", "draw", "draw"]
            );
            assert_eq!(hand, 4);
        } else {
            assert_eq!(draws(&events), 0);
            assert_eq!(hand, 2);
        }
        assert_at_rest(&runner);
    }
}

/// Tannuk, Memorial Ensign: "If this is the second time this ability has resolved this turn,
/// draw a card." follows the damage to each opponent, from a chain parked behind the remaining
/// opponent (CR 608.2c). The first landfall is the paired guard that the tail reads the count.
#[test]
fn a_gated_tail_behind_remaining_recipients_reads_the_resolution_count() {
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Tannuk, Memorial Ensign", 2, 4, TANNUK);
    scenario.add_enchantment_from_oracle(P0, "Exploration", EXPLORATION);
    let bubble = force_bubble(&mut scenario, P1);
    furnace(&mut scenario);
    scenario.with_library_top(P0, &["One", "Two", "Three"]);
    let plains = scenario.add_land_to_hand(P0, "Plains").id();
    let island = scenario.add_land_to_hand(P0, "Island").id();
    let mut runner = scenario.build();

    for (landfall, land, expected_draws) in [(1, plains, 0), (2, island, 1)] {
        let card_id = runner.state().objects[&land].card_id;
        let mut events = runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .expect("play land")
            .events;
        settle(&mut runner, &mut Vec::new(), &mut events);

        assert_prompt_is_replacement_choice(&runner);
        assert_eq!(life(&runner, P2), 22 - 2 * landfall);
        choose_source(&mut runner, &mut events, bubble);

        assert_eq!(counters(&runner, bubble, &depletion()), landfall as u32);
        assert_eq!(life(&runner, P1), 20);
        assert_eq!(life(&runner, P2), 20 - 2 * landfall);
        assert_eq!(draws(&events), expected_draws);
        assert_at_rest(&runner);
    }
}

/// CR 120.8: "If a source would deal 0 damage, it does not deal damage at all." Urza's Armor
/// first leaves a ping at 0, so Nefarious Lich has no event to replace and nothing to lose by.
#[test]
fn a_substitution_has_no_zero_damage_event_to_replace() {
    for (armor_first, graveyard_cards) in [(true, 0), (true, 1), (false, 1), (false, 0)] {
        let mut scenario = scenario();
        nefarious_lich(&mut scenario, graveyard_cards);
        let armor = scenario
            .add_artifact_from_oracle(P1, "Urza's Armor", URZAS_ARMOR)
            .id();
        let sorcerer = scenario
            .add_creature_from_oracle(P0, "Prodigal Sorcerer", 1, 1, PRODIGAL_SORCERER)
            .id();
        let mut runner = scenario.build();
        let mut events = runner
            .act(GameAction::ActivateAbility {
                source_id: sorcerer,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("activate")
            .events;
        settle(&mut runner, &mut vec![player_target(P1)], &mut events);
        assert_prompt_is_replacement_choice(&runner);
        if armor_first {
            choose_source(&mut runner, &mut events, armor);
        } else {
            choose_not_source(&mut runner, &mut events, armor);
        }

        if armor_first {
            assert_eq!(player(&runner, P1).graveyard.len(), graveyard_cards);
            assert_eq!(prevented(&events), 1);
            assert!(!player(&runner, P1).is_eliminated);
            assert_at_rest(&runner);
        } else if graveyard_cards == 1 {
            assert_eq!(player(&runner, P1).graveyard.len(), 0);
            assert_eq!(prevented(&events), 0);
            assert!(!player(&runner, P1).is_eliminated);
            assert_at_rest(&runner);
        } else {
            assert_game_over(&runner, P0);
        }
        assert_eq!(life(&runner, P1), 20);
    }
}

/// CR 120.8: "If a source would deal 0 damage, it does not deal damage at all." Prodigal
/// Sorcerer pings P1's Daunting Defender ("If a source would deal damage to a Cleric creature you
/// control, prevent 1 of that damage.") under P0's Soul-Scar Mage, and P1 orders the two
/// (CR 616.1). Defender first leaves the ping at 0, so Soul-Scar Mage has no event to replace:
/// no -1/-1 counter. Soul-Scar Mage first replaces the 1 with one -1/-1 counter.
#[test]
fn a_soul_scar_substitution_has_no_zero_damage_event_to_replace() {
    for defender_first in [true, false] {
        let mut scenario = scenario();
        let mage = soul_scar_mage(&mut scenario, P0);
        let defender = scenario
            .add_creature_from_oracle(P1, "Daunting Defender", 3, 3, DAUNTING_DEFENDER)
            .with_subtypes(vec!["Human", "Cleric"])
            .id();
        let sorcerer = scenario
            .add_creature_from_oracle(P0, "Prodigal Sorcerer", 1, 1, PRODIGAL_SORCERER)
            .id();
        let mut runner = scenario.build();
        let mut events = runner
            .act(GameAction::ActivateAbility {
                source_id: sorcerer,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("activate")
            .events;
        settle(&mut runner, &mut vec![object_target(defender)], &mut events);
        // Reach: the Defender's own "Cleric creature you control" makes it a candidate.
        assert_eq!(
            replacement_prompt(&runner),
            (P1, BTreeSet::from([mage, defender]))
        );
        choose_source(
            &mut runner,
            &mut events,
            if defender_first { defender } else { mage },
        );

        let mage_applied = events.iter().any(|event| {
            matches!(event, GameEvent::ReplacementApplied { source_id, .. } if *source_id == mage)
        });
        if defender_first {
            assert!(!mage_applied, "no 0-damage event to replace");
            assert_eq!(counters(&runner, defender, &CounterType::Minus1Minus1), 0);
        } else {
            assert!(mage_applied);
            assert_eq!(counters(&runner, defender, &CounterType::Minus1Minus1), 1);
        }
        assert_eq!(damage(&runner, defender), 0);
        assert_at_rest(&runner);
    }
}

/// Test of Faith's ruling: "If a 1/1 creature would be dealt 6 damage, 3 damage is prevented and
/// three +1/+1 counters are put on the creature." The last damage is then replaced, and each
/// effect's "that many" reads its own amount (CR 614.6, CR 615.5).
///
/// Dralnu, Lich Lord: "If damage would be dealt to Dralnu, sacrifice that many permanents
/// instead." The sacrifice is the modified event (CR 614.6), so its prompt comes before Test of
/// Faith's counters exist, and the counters follow the answer immediately afterward (CR 615.5):
/// 0 at the prompt, 3 after it. (This row used to expect the counters before the prompt.)
#[test]
fn an_earlier_rider_is_not_lost_to_a_substitution_and_reads_its_own_amount() {
    for shield_first in [true, false] {
        let mut scenario = scenario();
        let hydra = phytohydra(&mut scenario, P0);
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(hydra)]);
        let mut events = cast(&mut runner, slash, vec![object_target(hydra)]);
        if shield_first {
            choose_shield(&mut runner, &mut events);
        } else {
            choose_not_shield(&mut runner, &mut events);
        }

        assert_eq!(plus_counters(&runner, hydra), 4);
        assert_eq!(damage(&runner, hydra), 0);
        assert_eq!(prevented(&events), usize::from(shield_first));
        assert_at_rest(&runner);
    }

    // Shield first: three +1/+1 and one -1/-1, which annihilate to two +1/+1 (CR 704.5q).
    for shield_first in [true, false] {
        let mut scenario = self::scenario();
        let id = scenario
            .add_creature_from_oracle(P0, "Lichenthrope", 1, 6, LICHENTHROPE)
            .id();
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(id)]);
        let mut events = cast(&mut runner, slash, vec![object_target(id)]);
        if shield_first {
            choose_shield(&mut runner, &mut events);
            assert_eq!(plus_counters(&runner, id), 2);
            assert_eq!(counters(&runner, id, &CounterType::Minus1Minus1), 0);
        } else {
            choose_not_shield(&mut runner, &mut events);
            assert_eq!(plus_counters(&runner, id), 0);
            assert_eq!(counters(&runner, id, &CounterType::Minus1Minus1), 4);
        }
    }

    // Dralnu: the sacrifice prompt comes first, and the shield's rider follows its answer
    // (CR 614.6 + CR 615.5).
    for shield_first in [true, false] {
        let mut scenario = self::scenario();
        let id = dralnu(&mut scenario, P0);
        let land = lands(&mut scenario, P0, 4);
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(id)]);
        let mut events = cast(&mut runner, slash, vec![object_target(id)]);
        if shield_first {
            choose_shield(&mut runner, &mut events);
            assert_eq!(zone_prompt(&runner), (1, 5));
            assert_eq!(plus_counters(&runner, id), 0);
            answer(
                &mut runner,
                GameAction::SelectCards {
                    cards: vec![land[0]],
                },
                &mut events,
            );
            assert_eq!(plus_counters(&runner, id), 3);
            assert_eq!(zone(&runner, land[0]), Zone::Graveyard);
            assert_eq!(zone(&runner, id), Zone::Battlefield);
            assert_eq!(damage(&runner, id), 0);
        } else {
            choose_not_shield(&mut runner, &mut events);
            assert_eq!(plus_counters(&runner, id), 0);
            assert_eq!(zone_prompt(&runner), (4, 5));
            take_offered(&mut runner, &mut events);
        }
        assert_at_rest(&runner);
    }

    // Temper at X = 1: 1 prevented and 2 replaced, or all 3 replaced.
    for shield_first in [true, false] {
        let mut scenario = self::scenario();
        let hydra = phytohydra(&mut scenario, P0);
        let temper = temper(&mut scenario);
        let spell = bolt(&mut scenario);
        let mut runner = scenario.build();
        cast_temper_at_one(&mut runner, temper, hydra);
        let mut events = cast(&mut runner, spell, vec![object_target(hydra)]);
        if shield_first {
            choose_shield(&mut runner, &mut events);
        } else {
            choose_not_shield(&mut runner, &mut events);
        }

        assert_eq!(plus_counters(&runner, hydra), 3);
        assert_at_rest(&runner);
    }
}

/// Answer every counter-order prompt with the replacement `first` names, recording the +1/+1
/// counters on `id` at each prompt, until a prompt of another kind or rest.
fn answer_counter_orders(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    id: ObjectId,
    first: ObjectId,
) -> Vec<u32> {
    let mut seen = Vec::new();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        seen.push(plus_counters(runner, id));
        choose_source(runner, events, first);
    }
    seen
}

/// Soul-Scar Mage: "If a source you control would deal noncombat damage to a creature an opponent
/// controls, put that many -1/-1 counters on that creature instead." Test of Faith: "Prevent the
/// next 3 damage that would be dealt to target creature this turn. For each 1 damage prevented
/// this way, put a +1/+1 counter on that creature." Flame Slash's 4 at P1's 5/5 under both, in
/// P1's order (CR 616.1).
///
/// Test of Faith first prevents 3, and Soul-Scar Mage replaces the last 1. The -1/-1 counter is
/// the modified event, which happens when the damage would have (CR 614.6), and Test of Faith's
/// counters are the rest of its effect, immediately afterward (CR 615.5; Acolyte's Reward:
/// "Then Acolyte's Reward deals its damage."). Each reads its own amount: 1 and 3. After
/// CR 704.5q the 5/5 has two +1/+1 counters. Soul-Scar Mage first replaces all 4, and Test of
/// Faith has nothing left to prevent (CR 616.1f) and isn't used up (CR 609.7b).
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174), so
/// its three counters are three events of 1; the card puts one event of 3. The order and the
/// totals hold under both.
#[test]
fn an_earlier_rider_follows_a_soul_scar_substitution() {
    for faith_first in [true, false] {
        let mut scenario = scenario();
        let mage = soul_scar_mage(&mut scenario, P0);
        let creature = scenario.add_creature(P1, "Vanilla 5/5", 5, 5).id();
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(creature)]);
        let mut events = cast(&mut runner, slash, vec![object_target(creature)]);
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("expected P1's order, got {:?}", runner.state().waiting_for);
        };
        assert_eq!(*player, P1);
        assert!(candidates.iter().any(|c| c.source_id == mage));
        assert!(candidates.iter().any(|c| c.description == SHIELD));
        if faith_first {
            choose_shield(&mut runner, &mut events);
        } else {
            choose_source(&mut runner, &mut events, mage);
        }

        let put = counters_put(&events, creature);
        if faith_first {
            assert_eq!(
                put.first(),
                Some(&(CounterType::Minus1Minus1, 1)),
                "{put:?}"
            );
            assert!(
                put[1..]
                    .iter()
                    .all(|(kind, _)| *kind == CounterType::Plus1Plus1),
                "Test of Faith's counters follow the substitute: {put:?}"
            );
            assert_eq!(
                counters_put_total(&events, creature, &CounterType::Plus1Plus1),
                3
            );
            assert_eq!(prevented(&events), 1);
            assert_eq!(plus_counters(&runner, creature), 2);
            assert_eq!(counters(&runner, creature, &CounterType::Minus1Minus1), 0);
        } else {
            assert_eq!(put, vec![(CounterType::Minus1Minus1, 4)]);
            assert_eq!(prevented(&events), 0);
            assert_eq!(plus_counters(&runner, creature), 0);
            assert_eq!(counters(&runner, creature, &CounterType::Minus1Minus1), 4);
        }
        assert_eq!(damage(&runner, creature), 0);
        assert_at_rest(&runner);
    }
}

/// The board of `an_earlier_rider_follows_a_soul_scar_substitution`, Test of Faith first, with
/// P1's Hardened Scales and Doubling Season. The substitute's -1/-1 counter comes first
/// (CR 614.6), and Doubling Season alone applies to it, so no order is asked: 2. Test of Faith's
/// counters follow it (CR 615.5) and meet both, in P1's order (CR 616.1). So at P1's first
/// counter order the 5/5 already has its two -1/-1 counters and no +1/+1 counter.
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174),
/// so the engine asks three orders and puts 12 (Scales first) or 9 (Season first), 10 or 7 after
/// CR 704.5q. The card puts three counters in one event: 8 or 7, with one order (6 or 5 after
/// CR 704.5q).
#[test]
fn an_earlier_riders_prompt_follows_a_soul_scar_substitution() {
    for (scales_first, each, total, after_sba) in [(true, 4, 12, 10), (false, 3, 9, 7)] {
        let mut scenario = scenario();
        soul_scar_mage(&mut scenario, P0);
        let creature = scenario.add_creature(P1, "Vanilla 5/5", 5, 5).id();
        let scales = scenario
            .add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES)
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
            .id();
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(creature)]);
        let mut events = cast(&mut runner, slash, vec![object_target(creature)]);
        choose_shield(&mut runner, &mut events);

        assert_eq!(
            replacement_prompt(&runner),
            (P1, BTreeSet::from([scales, season]))
        );
        assert_eq!(counters(&runner, creature, &CounterType::Minus1Minus1), 2);
        assert_eq!(plus_counters(&runner, creature), 0);
        let first = if scales_first { scales } else { season };
        let mut seen = Vec::new();
        while matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            assert_eq!(
                replacement_prompt(&runner),
                (P1, BTreeSet::from([scales, season]))
            );
            seen.push(plus_counters(&runner, creature));
            choose_source(&mut runner, &mut events, first);
        }

        assert_eq!(seen, vec![0, each, 2 * each]);
        assert_eq!(
            counters_put_total(&events, creature, &CounterType::Plus1Plus1),
            total
        );
        assert_eq!(plus_counters(&runner, creature), after_sba);
        assert_eq!(counters(&runner, creature, &CounterType::Minus1Minus1), 0);
        assert_eq!(damage(&runner, creature), 0);
        assert_at_rest(&runner);
    }
}

/// The board of `an_earlier_rider_follows_a_soul_scar_substitution`, Test of Faith first, with
/// P1's Vizier of Remedies ("If one or more -1/-1 counters would be put on a creature you
/// control, that many -1/-1 counters minus one are put on it instead.") and Doubling Season. The
/// substitute's one -1/-1 counter meets both, and P1 orders them (CR 616.1): Vizier first
/// (1 − 1) × 2 = 0, Season first 1 × 2 − 1 = 1. Test of Faith's counters wait behind that prompt
/// (CR 615.5 + CR 608.2c), so no +1/+1 counter is there while it stands. Then its 3 counters meet
/// Doubling Season alone: 6. After CR 704.5q: 6 or 5 +1/+1 counters and no -1/-1 counter.
#[test]
fn an_earlier_rider_waits_behind_a_soul_scar_substitutions_prompt() {
    for (vizier_first, minus, after_sba) in [(true, 0, 6), (false, 1, 5)] {
        let mut scenario = scenario();
        soul_scar_mage(&mut scenario, P0);
        let creature = scenario.add_creature(P1, "Vanilla 5/5", 5, 5).id();
        let vizier = vizier_of_remedies(&mut scenario, P1);
        let season = scenario
            .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
            .id();
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(creature)]);
        let mut events = cast(&mut runner, slash, vec![object_target(creature)]);
        choose_shield(&mut runner, &mut events);

        let first = if vizier_first { vizier } else { season };
        let mut prompts = 0;
        while matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            assert_eq!(
                replacement_prompt(&runner),
                (P1, BTreeSet::from([vizier, season]))
            );
            assert_eq!(
                plus_counters(&runner, creature),
                0,
                "Test of Faith's counters wait behind the substitute's prompt"
            );
            prompts += 1;
            choose_source(&mut runner, &mut events, first);
        }

        assert!(
            prompts >= 1,
            "reach: the substitute's counter asks an order"
        );
        assert_eq!(
            counters_put_total(&events, creature, &CounterType::Minus1Minus1),
            minus
        );
        assert_eq!(
            counters_put_total(&events, creature, &CounterType::Plus1Plus1),
            6
        );
        assert_eq!(plus_counters(&runner, creature), after_sba);
        assert_eq!(counters(&runner, creature, &CounterType::Minus1Minus1), 0);
        assert_eq!(damage(&runner, creature), 0);
        assert_at_rest(&runner);
    }
}

/// Test of Faith: "For each 1 damage prevented this way, put a +1/+1 counter on that
/// creature." Dralnu, Lich Lord: "If damage would be dealt to Dralnu, sacrifice that many
/// permanents instead." The sacrifice is the modified event (CR 614.6), so it is asked first,
/// before any counter, and Test of Faith's counters follow it immediately afterward (CR 615.5),
/// under the Hardened Scales and Doubling Season the sacrifice left. Sacrificing Hardened Scales
/// leaves Doubling Season alone, one applicable replacement per counter event, and Dralnu ends
/// at 3 × 2 = 6 with no order asked. Sacrificing a land leaves both, so every counter event is
/// ordered (CR 616.1). (This row used to expect the counters, ordered, before the sacrifice
/// prompt, and 12 or 9 with Hardened Scales sacrificed.)
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174),
/// so with a land sacrificed the engine asks three orders and puts 12 (Scales first) or 9
/// (Season first). The card puts three counters in one event: 8 or 7, with one order.
#[test]
fn an_earlier_riders_counters_follow_the_substitutions_sacrifice() {
    // `None`: sacrifice Hardened Scales. `Some((scales_first, each, total))`: sacrifice a land.
    for land_order in [None, Some((true, 4, 12)), Some((false, 3, 9))] {
        let mut scenario = self::scenario();
        let id = dralnu(&mut scenario, P0);
        let scales = scenario
            .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON)
            .id();
        let land = lands(&mut scenario, P0, 2);
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(id)]);
        let mut events = cast(&mut runner, slash, vec![object_target(id)]);
        choose_shield(&mut runner, &mut events);

        // The sacrifice prompt comes first: no counter order was asked and no counter is put.
        assert_eq!(zone_prompt(&runner), (1, 5));
        assert_eq!(plus_counters(&runner, id), 0);
        let sacrificed = if land_order.is_some() {
            land[0]
        } else {
            scales
        };
        answer(
            &mut runner,
            GameAction::SelectCards {
                cards: vec![sacrificed],
            },
            &mut events,
        );
        assert_eq!(zone(&runner, sacrificed), Zone::Graveyard);

        match land_order {
            None => {
                assert!(
                    !matches!(
                        runner.state().waiting_for,
                        WaitingFor::ReplacementChoice { .. }
                    ),
                    "Doubling Season alone asks no order"
                );
                assert_eq!(plus_counters(&runner, id), 6);
            }
            Some((scales_first, each, total)) => {
                let first = if scales_first { scales } else { season };
                let seen = answer_counter_orders(&mut runner, &mut events, id, first);
                assert_eq!(seen, vec![0, each, 2 * each]);
                assert_eq!(plus_counters(&runner, id), total);
            }
        }
        assert_eq!(zone(&runner, id), Zone::Battlefield);
        assert_eq!(damage(&runner, id), 0);
        assert_at_rest(&runner);
    }

    // Phytohydra: "put that many +1/+1 counters on it instead" is the modified event
    // (CR 614.6), one ordered counter event of 1, and Test of Faith's counters follow it
    // (CR 615.5). Under Hardened Scales first every 1-counter event gives (1+1)×2 = 4, so these
    // totals don't show the order (`an_earlier_riders_prompt_follows_a_soul_scar_substitution`
    // does). strict-failure: Test of Faith's three counters are three ordered events (Command
    // Zone #174), so 4 + 12; the card puts them in one event: 4, then (3+1)×2 = 8: 12, with two
    // orders.
    let mut scenario = self::scenario();
    let hydra = phytohydra(&mut scenario, P0);
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(hydra)]);
    let mut events = cast(&mut runner, slash, vec![object_target(hydra)]);
    choose_shield(&mut runner, &mut events);
    let seen = answer_counter_orders(&mut runner, &mut events, hydra, scales);
    assert_eq!(seen, vec![0, 4, 8, 12]);
    assert_eq!(plus_counters(&runner, hydra), 16);
    assert_at_rest(&runner);

    // Kicked Hurloon Battle Hymn: "If this spell was kicked, you gain 4 life." is the spell's
    // next instruction. Dralnu's sacrifice comes first (CR 614.6), Test of Faith's counters
    // follow it as part of that prevention effect (CR 615.5), and only then does the spell resume
    // (CR 608.2c). With Hardened Scales sacrificed no counter order is asked, so the order is
    // read from the answering action's events: every +1/+1 counter on Dralnu before the gain.
    let mut scenario = self::scenario();
    let id = dralnu(&mut scenario, P0);
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hurloon Battle Hymn", true, HYMN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(id)]);
    {
        let cast = runner.cast(spell).target_object(id);
        let _ = cast.accept_optional().commit();
    }
    let mut events = Vec::new();
    settle(&mut runner, &mut Vec::new(), &mut events);
    choose_shield(&mut runner, &mut events);
    assert_eq!(zone_prompt(&runner), (1, 7));
    assert_eq!(plus_counters(&runner, id), 0);
    assert_eq!(life(&runner, P0), 20);
    let sacrifice = runner
        .act(GameAction::SelectCards {
            cards: vec![scales],
        })
        .expect("sacrifice")
        .events;
    let last_counter = sacrifice.iter().rposition(|event| {
        matches!(
            event,
            GameEvent::CounterAdded {
                object_id,
                counter_type: CounterType::Plus1Plus1,
                ..
            } if *object_id == id
        )
    });
    let gains: Vec<usize> = sacrifice
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(
                event,
                GameEvent::LifeChanged { player_id, amount: 4, .. } if *player_id == P0
            )
        })
        .map(|(index, _)| index)
        .collect();
    let last_counter =
        last_counter.expect("reach: Test of Faith's counters are put in the answering action");
    assert_eq!(
        gains.len(),
        1,
        "the kicked gain happens once: {sacrifice:?}"
    );
    assert!(
        last_counter < gains[0],
        "every counter before the spell resumes: {sacrifice:?}"
    );
    events.extend(sacrifice);
    settle(&mut runner, &mut Vec::new(), &mut events);
    assert_eq!(life(&runner, P0), 24);
    assert_eq!(plus_counters(&runner, id), 6);
    assert_eq!(zone(&runner, scales), Zone::Graveyard);
    assert_eq!(zone(&runner, id), Zone::Battlefield);
    assert_at_rest(&runner);
}

/// Pyroclasm: "deals 2 damage to each creature." A rider that asks for an order leaves its
/// prompt standing while Pyroclasm goes on to the other creatures; their riders wait behind it
/// (CR 608.2c, CR 615.5). Two Bears under their own Test of Faith each get their counters. With
/// Dralnu beside a Bear under Test of Faith, in either creation order, the Bear gets its counters
/// and Dralnu sacrifices two lands.
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174),
/// so each Bear's 2 prevented damage is two ordered counter events of 4 (8). The card puts two
/// counters in one event: 6, with one order.
#[test]
fn a_paused_rider_keeps_the_batchs_later_riders_behind_it() {
    let mut scenario = self::scenario();
    let a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let b = scenario.add_creature(P1, "Bear B", 2, 2).id();
    let scales = scenario
        .add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    let faith_a = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let faith_b = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
    let mut runner = scenario.build();
    cast(&mut runner, faith_a, vec![object_target(a)]);
    cast(&mut runner, faith_b, vec![object_target(b)]);
    let mut events = cast(&mut runner, spell, vec![]);
    let mut prompts = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        prompts += 1;
        choose_source(&mut runner, &mut events, scales);
    }
    assert_eq!(prompts, 4);
    assert_eq!(
        (plus_counters(&runner, a), plus_counters(&runner, b)),
        (8, 8)
    );
    assert_at_rest(&runner);

    for bear_first in [true, false] {
        let mut scenario = self::scenario();
        let bear = bear_first.then(|| scenario.add_creature(P1, "Bear", 2, 2).id());
        let id = dralnu(&mut scenario, P1);
        let bear = bear.unwrap_or_else(|| scenario.add_creature(P1, "Bear", 2, 2).id());
        let scales = scenario
            .add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES)
            .id();
        scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
        let land = lands(&mut scenario, P1, 2);
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(bear)]);
        let mut events = cast(&mut runner, spell, vec![]);

        let mut sacrificed_at = None;
        let mut orders = Vec::new();
        for _ in 0..4 {
            match runner.state().waiting_for.clone() {
                WaitingFor::ReplacementChoice { .. } => {
                    orders.push(plus_counters(&runner, bear));
                    choose_source(&mut runner, &mut events, scales);
                }
                WaitingFor::EffectZoneChoice { count, .. } => {
                    assert_eq!(count, 2);
                    assert!(runner.state().pending_replacement.is_none());
                    sacrificed_at = Some(plus_counters(&runner, bear));
                    answer(
                        &mut runner,
                        GameAction::SelectCards {
                            cards: land.clone(),
                        },
                        &mut events,
                    );
                }
                _ => break,
            }
        }
        assert_eq!(orders, vec![0, 4]);
        // strict-failure: the order between Dralnu's substitution and the Bear's counters is not
        // asserted: by CR 614.6 + CR 615.5 the substitution comes first, which the engine doesn't
        // yet do in every creation order (Command Zone #348). That order decides what P1 can
        // sacrifice (Hardened Scales before or after the Bear's counters). Coverage does not see
        // this order.
        assert!(sacrificed_at.is_some());
        assert_eq!(plus_counters(&runner, bear), 8);
        assert_eq!(zone(&runner, id), Zone::Battlefield);
        assert!(land
            .iter()
            .all(|land| zone(&runner, *land) == Zone::Graveyard));
        assert_at_rest(&runner);
    }
}

/// Pyroclasm at a Bear under Test of Faith, two Phyrexian Hydras and Lichenthrope. Phyrexian Hydra:
/// "Put a -1/-1 counter on this creature for each 1 damage prevented this way." Lichenthrope:
/// "put that many -1/-1 counters on it instead." Both Hydras' counters wait behind the
/// Bear's paused counters, and both creatures get theirs before Pyroclasm finishes (CR 608.2c,
/// CR 615.5); neither asks an order, since Hardened Scales puts no -1/-1 counter. Doubling Season
/// doubles each: 2 × 2 = 4. The order between Lichenthrope's substitution and the Bear's counters
/// is not asserted: by CR 614.6 + CR 615.5 the substitution comes first, which the engine doesn't
/// yet do in every creation order (Command Zone #172).
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174),
/// so the Bear's 2 prevented damage is two ordered counter events of 4 (8). The card puts two
/// counters in one event: 6, with one order.
#[test]
fn every_rider_queued_behind_a_paused_rider_runs() {
    let mut scenario = self::scenario();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let hydra = scenario
        .add_creature_from_oracle(P1, "Phyrexian Hydra", 7, 7, PHYREXIAN_HYDRA)
        .id();
    // A second rider queued behind the Bear's, so the queue's second entry is a rider too.
    let second_hydra = scenario
        .add_creature_from_oracle(P1, "Phyrexian Hydra", 7, 7, PHYREXIAN_HYDRA)
        .id();
    let lichenthrope = scenario
        .add_creature_from_oracle(P1, "Lichenthrope", 5, 5, LICHENTHROPE)
        .id();
    let scales = scenario
        .add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(bear)]);
    let mut events = cast(&mut runner, spell, vec![]);
    let orders = answer_counter_orders(&mut runner, &mut events, bear, scales);

    assert_eq!(orders, vec![0, 4]);
    assert_eq!(plus_counters(&runner, bear), 8);
    assert_eq!(counters(&runner, hydra, &CounterType::Minus1Minus1), 4);
    assert_eq!(
        counters(&runner, second_hydra, &CounterType::Minus1Minus1),
        4
    );
    assert_eq!(
        counters(&runner, lichenthrope, &CounterType::Minus1Minus1),
        4
    );
    assert_eq!(damage(&runner, hydra), 0);
    assert_eq!(damage(&runner, second_hydra), 0);
    assert_eq!(damage(&runner, lichenthrope), 0);
    assert_at_rest(&runner);
}

const SHOCKMAW_DRAGON: &str = "Flying\nWhenever this creature deals combat damage to a player, it deals 1 damage to each creature that player controls.";

/// Shockmaw Dragon: "Whenever this creature deals combat damage to a player, it deals 1 damage
/// to each creature that player controls." The trigger's event is 4 combat damage; its own
/// damage is 1 to each creature. A replacement's action reads the amount that replacement
/// replaced or prevented (CR 614.6, CR 615.5), not the event of the trigger resolving around
/// it: Test of Faith's "For each 1 damage prevented this way" puts 1 counter, Phytohydra's
/// "put that many +1/+1 counters on it instead" puts 1, and Dralnu, Lich Lord's "sacrifice that
/// many permanents instead" sacrifices 1. Under Hardened Scales and Doubling Season the Bear's
/// counter asks an order and Phytohydra's substitution waits behind it (CR 608.2c, CR 616.1):
/// one counter event of 1 each, Hardened Scales first, (1 + 1) × 2 = 4.
#[test]
fn a_replacements_action_reads_its_own_amount_inside_a_resolving_trigger() {
    let mut scenario = self::scenario();
    let dragon = scenario
        .add_creature_from_oracle(P0, "Shockmaw Dragon", 4, 4, SHOCKMAW_DRAGON)
        .id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let hydra = phytohydra(&mut scenario, P1);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(bear)]);
    combat_events(&mut runner, vec![dragon], vec![]);
    assert_eq!(life(&runner, P1), 16);
    assert_eq!(
        (plus_counters(&runner, bear), plus_counters(&runner, hydra)),
        (1, 1)
    );
    assert_eq!((damage(&runner, bear), damage(&runner, hydra)), (0, 0));
    assert_at_rest(&runner);

    let mut scenario = self::scenario();
    let dragon = scenario
        .add_creature_from_oracle(P0, "Shockmaw Dragon", 4, 4, SHOCKMAW_DRAGON)
        .id();
    let id = dralnu(&mut scenario, P1);
    let land = lands(&mut scenario, P1, 2);
    let mut runner = scenario.build();
    let mut events = combat_events(&mut runner, vec![dragon], vec![]);
    assert_eq!(zone_prompt(&runner), (1, 3));
    answer(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![land[0]],
        },
        &mut events,
    );
    assert_eq!(zone(&runner, land[0]), Zone::Graveyard);
    assert_eq!(zone(&runner, land[1]), Zone::Battlefield);
    assert_eq!(zone(&runner, id), Zone::Battlefield);
    assert_eq!(damage(&runner, id), 0);
    assert_at_rest(&runner);

    let mut scenario = self::scenario();
    let dragon = scenario
        .add_creature_from_oracle(P0, "Shockmaw Dragon", 4, 4, SHOCKMAW_DRAGON)
        .id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let hydra = phytohydra(&mut scenario, P1);
    let scales = scenario
        .add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(bear)]);
    let mut events = combat_events(&mut runner, vec![dragon], vec![]);
    let mut seen = Vec::new();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        seen.push((plus_counters(&runner, bear), plus_counters(&runner, hydra)));
        choose_source(&mut runner, &mut events, scales);
    }
    assert_eq!(seen, vec![(0, 0), (4, 0)]);
    assert_eq!(
        (plus_counters(&runner, bear), plus_counters(&runner, hydra)),
        (4, 4)
    );
    assert_eq!((damage(&runner, bear), damage(&runner, hydra)), (0, 0));
    assert_at_rest(&runner);
}

const MASTER_OF_THE_WILD_HUNT: &str = "At the beginning of your upkeep, create a 2/2 green Wolf creature token.\n{T}: Tap all untapped Wolf creatures you control. Each Wolf tapped this way deals damage equal to its power to target creature. That creature deals damage equal to its power divided as its controller chooses among any number of those Wolves.";

/// Master of the Wild Hunt: "That creature deals damage equal to its power divided as its
/// controller chooses among any number of those Wolves." The division is chosen while the
/// ability resolves (CR 608.2d), and the chosen portions are one batch. Wolf A and Wolf B are
/// each under their own Test of Faith, with Hardened Scales and Doubling Season: Wolf A's
/// counters ask an order (CR 616.1), and the rest of the division (Wolf B, then Wolf C) waits
/// behind that answer (CR 608.2c, CR 615.5). One prevented damage is one counter event either
/// way, so each Wolf gets (1 + 1) × 2 = 4.
#[test]
fn a_resolution_time_division_keeps_its_later_riders_behind_a_paused_one() {
    let mut scenario = self::scenario();
    let master = scenario
        .add_creature_from_oracle(P0, "Master of the Wild Hunt", 3, 3, MASTER_OF_THE_WILD_HUNT)
        .with_subtypes(vec!["Human", "Shaman"])
        .id();
    let mut wolf = |name: &str| {
        scenario
            .add_creature(P0, name, 2, 2)
            .with_subtypes(vec!["Wolf"])
            .id()
    };
    let a = wolf("Wolf A");
    let b = wolf("Wolf B");
    let c = wolf("Wolf C");
    let brute = scenario.add_creature(P1, "Target Brute", 3, 10).id();
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    let faith_a = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let faith_b = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let mut runner = scenario.build();
    cast(&mut runner, faith_a, vec![object_target(a)]);
    cast(&mut runner, faith_b, vec![object_target(b)]);

    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: master,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Master of the Wild Hunt")
        .events;
    settle(&mut runner, &mut vec![object_target(brute)], &mut events);
    let WaitingFor::DistributeAmong { player, total, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the division prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, total), (P1, 3));
    assert_eq!(damage(&runner, brute), 6, "each Wolf dealt the target 2");

    events.extend(
        runner
            .act(GameAction::DistributeAmong {
                distribution: vec![
                    (object_target(a), 1),
                    (object_target(b), 1),
                    (object_target(c), 1),
                ],
            })
            .expect("a legal division")
            .events,
    );
    assert_prompt_is_replacement_choice(&runner);
    assert_eq!(
        (damage(&runner, b), damage(&runner, c)),
        (0, 0),
        "the division waits behind the prompt"
    );
    let mut orders = Vec::new();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        orders.push((plus_counters(&runner, a), plus_counters(&runner, b)));
        choose_source(&mut runner, &mut events, scales);
    }

    assert_eq!(orders, vec![(0, 0), (4, 0)]);
    assert_eq!(
        (plus_counters(&runner, a), plus_counters(&runner, b)),
        (4, 4)
    );
    assert_eq!((damage(&runner, a), damage(&runner, b)), (0, 0));
    assert_eq!(plus_counters(&runner, c), 0);
    assert_eq!(damage(&runner, c), 1);
    assert_eq!(prevented(&events), 2);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "CR 117.3b: the active player receives priority after resolution"
    );
    assert_at_rest(&runner);
}

/// Master of the Wild Hunt's division under Furnace of Rath, with Wolf A under Test of Faith.
/// Wolf A's portion meets two replacements, so its controller orders them (CR 616.1) and the
/// rest of the division waits for that answer (CR 608.2c). Test of Faith first prevents the 1
/// damage, leaving Furnace of Rath nothing to double, and puts one counter on Wolf A. Then
/// Furnace of Rath doubles Wolf B's and Wolf C's 1 to 2, which destroys each 2/2 (CR 704.5g).
#[test]
fn a_resolution_time_division_parks_its_other_portions_behind_an_order_choice() {
    let mut scenario = self::scenario();
    let master = scenario
        .add_creature_from_oracle(P0, "Master of the Wild Hunt", 3, 3, MASTER_OF_THE_WILD_HUNT)
        .with_subtypes(vec!["Human", "Shaman"])
        .id();
    let mut wolf = |name: &str| {
        scenario
            .add_creature(P0, name, 2, 2)
            .with_subtypes(vec!["Wolf"])
            .id()
    };
    let a = wolf("Wolf A");
    let b = wolf("Wolf B");
    let c = wolf("Wolf C");
    let brute = scenario.add_creature(P1, "Target Brute", 3, 20).id();
    furnace(&mut scenario);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(a)]);

    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: master,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Master of the Wild Hunt")
        .events;
    settle(&mut runner, &mut vec![object_target(brute)], &mut events);
    let WaitingFor::DistributeAmong { player, total, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the division prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, total), (P1, 3));
    assert_eq!(damage(&runner, brute), 12, "each Wolf's 2, doubled");

    events.extend(
        runner
            .act(GameAction::DistributeAmong {
                distribution: vec![
                    (object_target(a), 1),
                    (object_target(b), 1),
                    (object_target(c), 1),
                ],
            })
            .expect("a legal division")
            .events,
    );
    assert_prompt_is_replacement_choice(&runner);
    assert_eq!(
        (damage(&runner, b), damage(&runner, c)),
        (0, 0),
        "the other portions wait for the order"
    );
    choose_shield(&mut runner, &mut events);

    assert_eq!(plus_counters(&runner, a), 1);
    assert_eq!(damage(&runner, a), 0);
    assert_eq!(zone(&runner, a), Zone::Battlefield);
    assert_eq!(zone(&runner, b), Zone::Graveyard);
    assert_eq!(zone(&runner, c), Zone::Graveyard);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "CR 117.3b: the active player receives priority after resolution"
    );
    assert_at_rest(&runner);
}

/// Pyroclasm at Bear A, Bear B and Bear C, each under its own Test of Faith, and Dralnu, Lich
/// Lord. The counters queued behind Bear A's paused counters run in the order they were queued
/// (CR 608.2c, CR 615.5): Bear B's, then Bear C's. Dralnu sacrifices two lands, and every Bear
/// gets its counters.
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174),
/// so each Bear's 2 prevented damage is two ordered counter events of 4 (8). The card puts two
/// counters in one event: 6, with one order.
#[test]
fn work_queued_behind_a_paused_rider_runs_in_the_order_it_was_queued() {
    let mut scenario = self::scenario();
    let a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let b = scenario.add_creature(P1, "Bear B", 2, 2).id();
    let c = scenario.add_creature(P1, "Bear C", 2, 2).id();
    let id = dralnu(&mut scenario, P1);
    let scales = scenario
        .add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    let land = lands(&mut scenario, P1, 2);
    let faith_a = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let faith_b = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let faith_c = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
    let mut runner = scenario.build();
    cast(&mut runner, faith_a, vec![object_target(a)]);
    cast(&mut runner, faith_b, vec![object_target(b)]);
    cast(&mut runner, faith_c, vec![object_target(c)]);
    let mut events = cast(&mut runner, spell, vec![]);

    // The order between Dralnu's substitution and the Bears' counters is not asserted: by
    // CR 614.6 + CR 615.5 the substitution comes first, which the engine doesn't yet do in every
    // creation order (Command Zone #172). Dralnu sacrifices the two lands, so the Bears' counters
    // are the same wherever the sacrifice falls.
    let (mut orders, mut sacrifices) = (Vec::new(), 0);
    loop {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                orders.push((
                    plus_counters(&runner, a),
                    plus_counters(&runner, b),
                    plus_counters(&runner, c),
                ));
                choose_source(&mut runner, &mut events, scales);
            }
            WaitingFor::EffectZoneChoice { .. } => {
                assert_eq!(zone_prompt(&runner), (2, 8));
                sacrifices += 1;
                answer(
                    &mut runner,
                    GameAction::SelectCards {
                        cards: land.clone(),
                    },
                    &mut events,
                );
            }
            _ => break,
        }
    }
    assert_eq!(
        orders,
        vec![
            (0, 0, 0),
            (4, 0, 0),
            (8, 0, 0),
            (8, 4, 0),
            (8, 8, 0),
            (8, 8, 4)
        ]
    );
    assert_eq!(sacrifices, 1);
    assert_eq!(
        (
            plus_counters(&runner, a),
            plus_counters(&runner, b),
            plus_counters(&runner, c)
        ),
        (8, 8, 8)
    );
    assert!(land
        .iter()
        .all(|land| zone(&runner, *land) == Zone::Graveyard));
    assert_eq!(zone(&runner, scales), Zone::Battlefield);
    assert_eq!(zone(&runner, id), Zone::Battlefield);
    assert_at_rest(&runner);
}

/// Pyroclasm at P1's Swans of Bryn Argoll: "The source's controller draws cards equal to the
/// damage prevented this way." P0 draws 2, and P0's Abundance replaces each draw: "you may
/// instead choose land or nonland and reveal cards from the top of your library until you
/// reveal a card of the chosen kind. Put that card into your hand …". P0 takes both and chooses
/// nonland twice; the Swans' draw is over with its second card (CR 121.2, CR 614.6, CR 615.5).
///
/// Phytohydra's 2 +1/+1 counters and Lichenthrope's 2 -1/-1 counters are both put before
/// Pyroclasm finishes (CR 608.2c, CR 615.5). With a 0/9 in their place, Pyroclasm still
/// finishes.
#[test]
fn a_rider_draw_replaced_card_by_card_finishes_before_the_spell_does() {
    for queued in [false, true] {
        let mut scenario = self::scenario();
        for index in 0..4 {
            scenario.add_spell_to_library_top(P0, &format!("Library Filler {index}"), true);
        }
        let abundance = scenario
            .add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE)
            .id();
        let swans = scenario
            .add_creature_from_oracle(P1, "Swans of Bryn Argoll", 4, 3, SWANS_OF_BRYN_ARGOLL)
            .from_oracle_text_with_keywords(&["Flying"], SWANS_OF_BRYN_ARGOLL)
            .id();
        let substitutes = queued.then(|| {
            let hydra = phytohydra(&mut scenario, P1);
            let lichenthrope = scenario
                .add_creature_from_oracle(P1, "Lichenthrope", 5, 5, LICHENTHROPE)
                .id();
            (hydra, lichenthrope)
        });
        let wall = (!queued).then(|| scenario.add_creature(P1, "Wall", 0, 9).id());
        let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
        let mut runner = scenario.build();
        let hand_before = player(&runner, P0).hand.len();
        let library_before = player(&runner, P0).library.len();
        let mut events = cast(&mut runner, spell, vec![]);

        // The order between another creature's substitution and Swans' draw is not asserted: by
        // CR 614.6 + CR 615.5 the substitution comes first, which the engine doesn't yet do in
        // every creation order (Command Zone #172).
        let (mut accepted, mut chosen) = (0, 0);
        loop {
            match &runner.state().waiting_for {
                WaitingFor::ReplacementChoice { .. } => {
                    accepted += 1;
                    choose(&mut runner, &mut events, |candidate| {
                        candidate.source_id == abundance && candidate.description == "Accept"
                    });
                }
                WaitingFor::NamedChoice { .. } => {
                    chosen += 1;
                    let choice = "Nonland".to_string();
                    answer(
                        &mut runner,
                        GameAction::ChooseOption { choice },
                        &mut events,
                    );
                }
                _ => break,
            }
        }

        assert_eq!((accepted, chosen), (2, 2));
        assert_eq!(player(&runner, P0).hand.len(), hand_before - 1 + 2);
        assert_eq!(player(&runner, P0).library.len(), library_before - 2);
        assert_eq!(damage(&runner, swans), 0);
        if let Some(wall) = wall {
            assert_eq!(damage(&runner, wall), 2);
        }
        if let Some((hydra, lichenthrope)) = substitutes {
            assert_eq!(plus_counters(&runner, hydra), 2);
            assert_eq!(
                counters(&runner, lichenthrope, &CounterType::Minus1Minus1),
                2
            );
            assert_eq!(damage(&runner, hydra), 0);
            assert_eq!(damage(&runner, lichenthrope), 0);
        }
        assert_at_rest(&runner);
    }
}

const SWELTERING_SUNS: &str = "Sweltering Suns deals 3 damage to each creature.\nCycling {3} ({3}, Discard this card: Draw a card.)";

/// The Swans' draw under Abundance where P0 takes some cards and declines a later one, so the
/// last answer of the draw is not an Abundance reveal's land-or-nonland choice: Pyroclasm's 2
/// (take, decline), and Sweltering Suns' 3 (take, decline, decline; take, take, decline; decline,
/// take, decline). The Swans' draw is over with its last card (CR 121.2, CR 615.5), so its rider
/// retires then, and the spell finishes (CR 608.2c).
///
/// Phytohydra, under P1's Hardened Scales and Doubling Season, gets the damage as +1/+1
/// counters, Hardened Scales first: (2 + 1) × 2 = 6, (3 + 1) × 2 = 8. With a 0/9 in its place,
/// the spell still finishes.
///
/// P0 should end with one card for each card of the draw: 2 from Pyroclasm, 3 from Sweltering
/// Suns. These flows end a card short (Command Zone #178), so these rows assert no draw count.
#[test]
fn a_rider_draw_with_a_declined_replacement_finishes_before_the_spell_does() {
    for (name, text, amount, plan) in [
        ("Pyroclasm", PYROCLASM, 2, vec![true, false]),
        (
            "Sweltering Suns",
            SWELTERING_SUNS,
            3,
            vec![true, false, false],
        ),
        (
            "Sweltering Suns",
            SWELTERING_SUNS,
            3,
            vec![true, true, false],
        ),
        (
            "Sweltering Suns",
            SWELTERING_SUNS,
            3,
            vec![false, true, false],
        ),
    ] {
        for queued in [false, true] {
            let mut scenario = self::scenario();
            for index in 0..8 {
                scenario.add_spell_to_library_top(P0, &format!("Library Filler {index}"), true);
            }
            let abundance = scenario
                .add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE)
                .id();
            let swans = scenario
                .add_creature_from_oracle(P1, "Swans of Bryn Argoll", 4, 3, SWANS_OF_BRYN_ARGOLL)
                .from_oracle_text_with_keywords(&["Flying"], SWANS_OF_BRYN_ARGOLL)
                .id();
            let recipient = if queued {
                phytohydra(&mut scenario, P1)
            } else {
                scenario.add_creature(P1, "Wall", 0, 9).id()
            };
            let scales = scenario
                .add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES)
                .id();
            scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
            let spell = sorcery(&mut scenario, name, text);
            let mut runner = scenario.build();
            let mut events = cast(&mut runner, spell, vec![]);

            let row = format!("{name} {plan:?} queued: {queued}");
            // The order between another creature's substitution and Swans' draw is not
            // asserted: by CR 614.6 + CR 615.5 the substitution comes first, which the engine
            // doesn't yet do in every creation order (Command Zone #172).
            let (mut answered, mut chosen, mut orders) = (0, 0, 0);
            loop {
                let waiting_for = runner.state().waiting_for.clone();
                let abundance_prompt = matches!(
                    &waiting_for,
                    WaitingFor::ReplacementChoice { candidates, .. }
                        if candidates.iter().any(|candidate| candidate.source_id == abundance)
                );
                match waiting_for {
                    WaitingFor::ReplacementChoice { .. } if abundance_prompt => {
                        let take = plan.get(answered).copied().expect("one prompt per card");
                        answered += 1;
                        let description = if take { "Accept" } else { "Decline" };
                        choose(&mut runner, &mut events, |candidate| {
                            candidate.source_id == abundance && candidate.description == description
                        });
                    }
                    // Phytohydra's counters: the order of Hardened Scales and Doubling Season.
                    WaitingFor::ReplacementChoice { .. } => {
                        orders += 1;
                        choose_source(&mut runner, &mut events, scales);
                    }
                    WaitingFor::NamedChoice { .. } => {
                        chosen += 1;
                        let choice = "Nonland".to_string();
                        answer(
                            &mut runner,
                            GameAction::ChooseOption { choice },
                            &mut events,
                        );
                    }
                    WaitingFor::RevealUntilBottomOrder { cards, .. } => {
                        answer(&mut runner, GameAction::SelectCards { cards }, &mut events);
                    }
                    _ => break,
                }
            }

            let taken = plan.iter().filter(|take| **take).count();
            assert_eq!((answered, chosen), (plan.len(), taken), "{row}");
            assert_eq!(damage(&runner, swans), 0, "{row}");
            if queued {
                assert_eq!(orders, 1, "{row}");
                assert_eq!(plus_counters(&runner, recipient), (amount + 1) * 2, "{row}");
                assert_eq!(damage(&runner, recipient), 0, "{row}");
            } else {
                assert_eq!(orders, 0, "{row}");
                assert_eq!(damage(&runner, recipient), amount, "{row}");
            }
            assert_at_rest(&runner);
        }
    }
}

/// Temper with its printed {X}{1}{W}, and the mana to pay for it.
pub(super) fn temper(scenario: &mut GameScenario) -> ObjectId {
    let id = scenario
        .add_spell_to_hand_from_oracle(P0, "Temper", true, TEMPER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::White],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    id
}

pub(super) fn cast_temper_at_one(runner: &mut GameRunner, temper: ObjectId, target: ObjectId) {
    let action = cast_action(runner, temper);
    runner.act(action).expect("cast Temper");
    let mut target = Some(object_target(target));
    for _ in 0..40 {
        let action = match &runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: 1 },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: target.take(),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("Temper step");
    }
    panic!("Temper did not resolve");
}

/// A rider resident from one recipient isn't lost to the next recipient's substitution (Temper
/// and Pyroclasm; CR 615.5). In combat, Test of Faith on P1's Phytohydra blocking a 4/4: the two
/// are ordered by P1 (CR 616.1). Test of Faith first prevents 3 and puts 3 counters, and
/// Phytohydra replaces the last 1 with 1 more: 4. Phytohydra first replaces all 4 with 4
/// counters and leaves the shield unused. Phytohydra is dealt no damage, and no rider is left.
/// Phytohydra's counter is the modified event (CR 614.6), and Test of Faith's follow it
/// immediately afterward (CR 615.5).
#[test]
fn a_resident_rider_from_another_recipient_is_not_lost() {
    let mut scenario = scenario();
    let bear = scenario.add_creature(P0, "Bear", 1, 5).id();
    let hydra = phytohydra(&mut scenario, P0);
    let temper = temper(&mut scenario);
    let pyroclasm = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
    let mut runner = scenario.build();
    cast_temper_at_one(&mut runner, temper, bear);
    cast(&mut runner, pyroclasm, vec![]);

    assert_eq!(plus_counters(&runner, bear), 1);
    assert_eq!(damage(&runner, bear), 1);
    assert_eq!(plus_counters(&runner, hydra), 2);
    assert_eq!(damage(&runner, hydra), 0);
    assert_at_rest(&runner);

    for faith_first in [true, false] {
        let mut scenario = self::scenario();
        let hydra = phytohydra(&mut scenario, P1);
        let attacker = scenario.add_creature(P0, "Ogre", 4, 4).id();
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(hydra)]);
        let mut events = combat_events(&mut runner, vec![attacker], vec![(hydra, attacker)]);
        assert_prompt_is_replacement_choice(&runner);
        if faith_first {
            choose_shield(&mut runner, &mut events);
        } else {
            choose_source(&mut runner, &mut events, hydra);
        }

        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "one order prompt"
        );
        if faith_first {
            // CR 614.6 + CR 615.5: Phytohydra's counter (its controller P1's) is put before every
            // one of Test of Faith's (its controller P0's).
            let actors: Vec<(u32, PlayerId)> = events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::CounterAdded {
                        object_id,
                        counter_type: CounterType::Plus1Plus1,
                        count,
                        actor,
                    } if *object_id == hydra => Some((*count, *actor)),
                    _ => None,
                })
                .collect();
            let phytohydras = actors
                .iter()
                .position(|&(_, actor)| actor == P1)
                .expect("Phytohydra's counter is put");
            assert_eq!(actors[phytohydras], (1, P1), "{actors:?}");
            let test_of_faiths: Vec<usize> = actors
                .iter()
                .enumerate()
                .filter(|(_, (_, actor))| *actor == P0)
                .map(|(position, _)| position)
                .collect();
            assert!(!test_of_faiths.is_empty(), "{actors:?}");
            assert!(
                test_of_faiths
                    .iter()
                    .all(|&position| position > phytohydras),
                "{actors:?}"
            );
        }
        assert_eq!(plus_counters(&runner, hydra), 4);
        assert_eq!(damage(&runner, hydra), 0);
        assert!(!runner.state().has_post_replacement_drain());
    }
}

/// Force Bubble: "The effect does not prevent damage, it replaces it so the damage is neither
/// dealt or prevented." Delaying Shield: "This means it can replace damage that 'can't be
/// prevented'." Crumbling Sanctuary: "The ability affects all players." (CR 615.13.)
#[test]
fn player_substitutions_replace_damage_preventable_or_not() {
    for (name, text, counter) in [
        ("Force Bubble", FORCE_BUBBLE, depletion()),
        ("Delaying Shield", DELAYING_SHIELD, delay()),
    ] {
        for (spell, amount) in [("Stomp", 2), ("Lightning Bolt", 3)] {
            let mut scenario = scenario();
            let id = scenario.add_enchantment_from_oracle(P1, name, text).id();
            let spell = if spell == "Stomp" {
                stomp(&mut scenario)
            } else {
                bolt(&mut scenario)
            };
            let mut runner = scenario.build();
            let events = cast(&mut runner, spell, vec![player_target(P1)]);

            assert_eq!(counters(&runner, id, &counter), amount, "{name}");
            assert_eq!(life(&runner, P1), 20);
            assert_eq!(prevented(&events), 0);
            assert_eq!(dealt(&events), 0);
        }
    }

    for (spell, amount) in [("Stomp", 2), ("Lightning Bolt", 3)] {
        let mut scenario = scenario();
        scenario.add_artifact_from_oracle(P0, "Crumbling Sanctuary", CRUMBLING_SANCTUARY);
        scenario.with_library_top(P1, &["One", "Two", "Three", "Four"]);
        let spell = if spell == "Stomp" {
            stomp(&mut scenario)
        } else {
            bolt(&mut scenario)
        };
        let mut runner = scenario.build();
        let events = cast(&mut runner, spell, vec![player_target(P1)]);

        let exiled = runner
            .state()
            .objects
            .values()
            .filter(|object| object.zone == Zone::Exile && object.owner == P1)
            .count();
        assert_eq!(exiled, amount);
        assert_eq!(life(&runner, P1), 20);
        assert_eq!(prevented(&events), 0);
        assert_eq!(dealt(&events), 0);
    }
}

/// Force Bubble: "If you take 6 damage at once, it will replace all 6 damage and put 6 counters
/// on it. Then the second ability will trigger."
#[test]
fn force_bubble_is_sacrificed_at_four_counters() {
    let mut scenario = scenario();
    let bubble = force_bubble(&mut scenario, P1);
    let (first, second) = (bolt(&mut scenario), bolt(&mut scenario));
    let mut runner = scenario.build();

    cast(&mut runner, first, vec![player_target(P1)]);
    assert_eq!(counters(&runner, bubble, &depletion()), 3);
    assert_eq!(zone(&runner, bubble), Zone::Battlefield);

    cast(&mut runner, second, vec![player_target(P1)]);
    assert_eq!(zone(&runner, bubble), Zone::Graveyard);
    assert_eq!(life(&runner, P1), 20);
    assert_at_rest(&runner);
}

/// Crumbling Sanctuary: "If you have no cards in your library, then damage done to you will have
/// no effect since it will get changed into a removal that can't happen." (CR 614.6.)
#[test]
fn crumbling_sanctuary_replaces_damage_with_an_empty_library() {
    let mut scenario = scenario();
    scenario.add_artifact_from_oracle(P0, "Crumbling Sanctuary", CRUMBLING_SANCTUARY);
    let spell = bolt(&mut scenario);
    let mut runner = scenario.build();
    assert!(player(&runner, P1).library.is_empty());
    let events = cast(&mut runner, spell, vec![player_target(P1)]);

    assert_eq!(life(&runner, P1), 20);
    assert_eq!(prevented(&events), 0);
    assert_at_rest(&runner);
}

/// Dralnu: "Dralnu may be one of the permanents you sacrifice while applying its replacement
/// effect." and "If Dralnu would be dealt more damage than the number of permanents you control,
/// instead you sacrifice all permanents you control." (CR 608.2d.)
#[test]
fn dralnu_sacrifices_the_permanents_its_controller_picks() {
    let mut scenario = scenario();
    let id = dralnu(&mut scenario, P1);
    let others: Vec<ObjectId> = (0..3)
        .map(|index| {
            scenario
                .add_creature(P1, &format!("Bear {index}"), 1, 1)
                .id()
        })
        .collect();
    let spell = bolt(&mut scenario);
    let mut runner = scenario.build();
    let mut events = cast(&mut runner, spell, vec![object_target(id)]);

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            count,
            cards,
            effect_kind,
            ..
        } => {
            assert_eq!(*effect_kind, EffectKind::Sacrifice);
            assert_eq!((*count, cards.len()), (3, 4));
            assert!(cards.contains(&id));
        }
        other => panic!("expected EffectZoneChoice, got {other:?}"),
    }
    answer(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![id, others[0], others[1]],
        },
        &mut events,
    );

    assert_eq!(zone(&runner, id), Zone::Graveyard);
    assert_eq!(zone(&runner, others[0]), Zone::Graveyard);
    assert_eq!(zone(&runner, others[1]), Zone::Graveyard);
    assert_eq!(zone(&runner, others[2]), Zone::Battlefield);
    assert_eq!(damage(&runner, id), 0);
    assert_at_rest(&runner);

    // Fewer permanents than damage: all of them, with no prompt.
    let mut scenario = self::scenario();
    let id = dralnu(&mut scenario, P1);
    let other = scenario.add_creature(P1, "Bear", 1, 1).id();
    let spell = bolt(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![object_target(id)]);
    assert_eq!(zone(&runner, id), Zone::Graveyard);
    assert_eq!(zone(&runner, other), Zone::Graveyard);
    assert_at_rest(&runner);

    // Damage that can't be prevented is still replaced.
    let mut scenario = self::scenario();
    let id = dralnu(&mut scenario, P1);
    lands(&mut scenario, P1, 2);
    let spell = stomp(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![object_target(id)]);
    assert_eq!(zone_prompt(&runner), (2, 3));
}

/// Phytohydra: "Phytohydra's controller chooses which effect to apply first." Bolt under Furnace
/// of Rath is 3 counters, or 6 with Furnace first (CR 616.1).
#[test]
fn a_substitution_is_ordered_against_a_doubler() {
    for hydra_first in [true, false] {
        let mut scenario = scenario();
        let hydra = phytohydra(&mut scenario, P1);
        furnace(&mut scenario);
        let spell = bolt(&mut scenario);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![object_target(hydra)]);
        assert_prompt_is_replacement_choice(&runner);
        if hydra_first {
            choose_source(&mut runner, &mut events, hydra);
        } else {
            choose_not_source(&mut runner, &mut events, hydra);
        }

        assert_eq!(
            plus_counters(&runner, hydra),
            if hydra_first { 3 } else { 6 }
        );
        assert_at_rest(&runner);
    }
}

/// Phytohydra: "If another effect would prevent damage from being dealt to Phytohydra or replace
/// it with something else, Phytohydra's controller chooses which effect to apply first."
#[test]
fn a_substitution_is_ordered_against_a_prevention_shield() {
    for hydra_first in [true, false] {
        let mut scenario = scenario();
        let hydra = phytohydra(&mut scenario, P0);
        let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
        let spell = bolt(&mut scenario);
        let mut runner = scenario.build();
        cast(&mut runner, hands, vec![object_target(hydra)]);
        let mut events = cast(&mut runner, spell, vec![object_target(hydra)]);
        assert_prompt_is_replacement_choice(&runner);
        if hydra_first {
            choose_not_shield(&mut runner, &mut events);
        } else {
            choose_shield(&mut runner, &mut events);
        }

        assert_eq!(
            plus_counters(&runner, hydra),
            if hydra_first { 3 } else { 0 }
        );
        assert_eq!(damage(&runner, hydra), 0);
        assert_at_rest(&runner);
    }
}

/// Force Bubble ("If damage would be dealt to you, put that many depletion counters on this
/// enchantment instead.") and Pariah ("All damage that would be dealt to you is dealt to
/// enchanted creature instead.") both apply to Lightning Bolt's 3 at P1, who orders them
/// (CR 616.1, CR 616.1e). Force Bubble first replaces the damage with 3 depletion counters, and
/// Pariah has no event left. Pariah first sends it to the enchanted creature, which "dealt to
/// you" no longer matches (CR 616.1f).
#[test]
fn a_substitution_and_a_redirect_ask_their_order() {
    for bubble_first in [true, false] {
        let mut scenario = scenario();
        let bubble = force_bubble(&mut scenario, P1);
        let creature = scenario.add_creature(P1, "Wall", 0, 8).id();
        let pariah = scenario
            .add_enchantment_from_oracle(P1, "Pariah", PARIAH)
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
            .id();
        let spell = bolt(&mut scenario);
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), pariah, creature);
        let mut events = cast(&mut runner, spell, vec![player_target(P1)]);
        let (chooser, sources) = replacement_prompt(&runner);
        assert_eq!(chooser, P1);
        assert!(
            sources.contains(&bubble) && sources.contains(&pariah),
            "{sources:?}"
        );
        choose_source(
            &mut runner,
            &mut events,
            if bubble_first { bubble } else { pariah },
        );

        if bubble_first {
            assert_eq!(counters(&runner, bubble, &depletion()), 3);
            assert_eq!(damage(&runner, creature), 0);
        } else {
            assert_eq!(counters(&runner, bubble, &depletion()), 0);
            assert_eq!(damage(&runner, creature), 3);
        }
        assert_eq!(life(&runner, P1), 20);
        assert_at_rest(&runner);
    }
}

/// The candidate for protection's prevention (CR 702.16e), offered beside the other replacements
/// that apply to the same damage (CR 616.1).
fn is_protection(candidate: &ReplacementCandidateSummary) -> bool {
    candidate
        .description
        .starts_with("Prevent damage with protection")
}

/// Pyroclasm at a Phytohydra with protection from everything. Protection's prevention
/// (CR 702.16e: "Any damage that would be dealt by sources that have the stated quality to a
/// permanent or player with protection is prevented.") and Phytohydra's substitution both apply,
/// so its controller chooses one (CR 616.1): protection first prevents the 2, Phytohydra first
/// turns it into two +1/+1 counters and leaves nothing to prevent.
#[test]
fn protection_and_a_substitution_are_ordered_by_the_affected_player() {
    for protection_first in [true, false] {
        let mut scenario = scenario();
        let hydra = scenario
            .add_creature_from_oracle(P0, "Phytohydra", 1, 1, PHYTOHYDRA)
            .with_keyword(Keyword::Protection(ProtectionTarget::Everything))
            .id();
        let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
        let mut runner = scenario.build();
        let mut events = cast(&mut runner, spell, vec![]);

        let (chooser, sources) = replacement_prompt(&runner);
        assert_eq!(chooser, P0);
        assert!(sources.contains(&hydra), "{sources:?}");
        if protection_first {
            choose(&mut runner, &mut events, is_protection);
        } else {
            choose_source(&mut runner, &mut events, hydra);
        }

        if protection_first {
            assert_eq!(prevented(&events), 1);
            assert_eq!(plus_counters(&runner, hydra), 0);
        } else {
            assert!(events.iter().any(|e| matches!(
                e,
                GameEvent::ReplacementApplied { source_id, .. } if *source_id == hydra
            )));
            assert_eq!(prevented(&events), 0);
            assert_eq!(plus_counters(&runner, hydra), 2);
        }
        assert_eq!(damage(&runner, hydra), 0);
        assert_at_rest(&runner);
    }
}

/// Pyroclasm (red, CR 202.2) at P1's Silver Knight ("First strike, protection from red") under
/// P0's Soul-Scar Mage. Protection's prevention (CR 702.16e) and Soul-Scar Mage's substitution
/// both apply, so P1 chooses one (CR 616.1, CR 616.1e): Soul-Scar Mage first puts two -1/-1
/// counters on the Knight (a 2/2, so it dies, CR 704.5f), protection first none. P1's Hill Giant,
/// with no protection, is the reach guard: Soul-Scar Mage applies to it alone in the same cast, and
/// it survives as a 1/1.
#[test]
fn protection_and_soul_scar_mage_are_ordered_by_the_affected_player() {
    for protection_first in [true, false] {
        let mut scenario = scenario();
        let mage = soul_scar_mage(&mut scenario, P0);
        let knight = scenario
            .add_creature(P1, "Silver Knight", 2, 2)
            .with_subtypes(vec!["Human", "Knight"])
            .from_oracle_text_with_keywords(&["First strike", "Protection"], SILVER_KNIGHT)
            .id();
        let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            })
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P0, ManaColor::Red);
        }
        let mut runner = scenario.build();
        assert!(runner.state().objects[&knight]
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            ))));
        let mut events = cast(&mut runner, spell, vec![]);

        let (chooser, sources) = replacement_prompt(&runner);
        assert_eq!(chooser, P1);
        assert!(sources.contains(&mage), "{sources:?}");
        if protection_first {
            choose(&mut runner, &mut events, is_protection);
        } else {
            choose_source(&mut runner, &mut events, mage);
        }

        assert_eq!(counters(&runner, giant, &CounterType::Minus1Minus1), 2);
        assert_eq!(zone(&runner, giant), Zone::Battlefield);
        if protection_first {
            assert!(events.iter().any(|e| matches!(
                e,
                GameEvent::DamagePrevented { target: TargetRef::Object(id), .. } if *id == knight
            )));
            assert_eq!(counters(&runner, knight, &CounterType::Minus1Minus1), 0);
            assert_eq!(zone(&runner, knight), Zone::Battlefield);
        } else {
            assert_eq!(zone(&runner, knight), Zone::Graveyard);
        }
        assert_at_rest(&runner);
    }
}

/// Szadek, Lord of Secrets attacks P1, who cast Teferi's Protection ("you gain protection from
/// everything"). Protection's prevention (CR 702.16e) and Szadek's substitution both apply to the
/// combat damage to P1, who chooses one (CR 616.1, CR 616.1e): Szadek first puts five +1/+1
/// counters on Szadek and mills five, protection first does neither.
#[test]
fn protection_and_szadek_are_ordered_by_the_affected_player() {
    for protection_first in [true, false] {
        let mut scenario = scenario();
        let szadek = szadek(&mut scenario);
        scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);
        let protection = scenario
            .add_spell_to_hand_from_oracle(P1, "Teferi's Protection", true, TEFERIS_PROTECTION)
            .id();
        let mut runner = scenario.build();
        // P0 passes in precombat main, and P1 casts Teferi's Protection with priority.
        runner.act(GameAction::PassPriority).expect("P0 passes");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ),
            "got {:?}",
            runner.state().waiting_for
        );
        cast(&mut runner, protection, vec![]);
        assert_eq!(runner.state().active_player, P0);

        let mut events = combat_events(&mut runner, vec![szadek], vec![]);

        let (chooser, sources) = replacement_prompt(&runner);
        assert_eq!(chooser, P1);
        assert!(sources.contains(&szadek), "{sources:?}");
        if protection_first {
            choose(&mut runner, &mut events, is_protection);
        } else {
            choose_source(&mut runner, &mut events, szadek);
        }

        if protection_first {
            assert!(events.iter().any(|e| matches!(
                e,
                GameEvent::DamagePrevented { target: TargetRef::Player(player), .. }
                    if *player == P1
            )));
            assert_eq!(plus_counters(&runner, szadek), 0);
            assert_eq!(library_and_graveyard(&runner, P1), (7, 0));
        } else {
            assert_eq!(plus_counters(&runner, szadek), 5);
            assert_eq!(library_and_graveyard(&runner, P1), (2, 5));
        }
        assert_eq!(life(&runner, P1), 20);
        assert!(runner.state().pending_combat_lifelink.is_none());
        assert_at_rest(&runner);
    }
}

/// "dealt to you" and "dealt to this creature" name only their recipient (CR 614.1a).
#[test]
fn a_substitution_covers_only_its_recipient() {
    let mut scenario = scenario();
    let bubble = force_bubble(&mut scenario, P0);
    let creature = scenario.add_creature(P0, "Bear", 2, 4).id();
    let (first, second) = (bolt(&mut scenario), bolt(&mut scenario));
    let mut runner = scenario.build();
    cast(&mut runner, first, vec![player_target(P1)]);
    cast(&mut runner, second, vec![object_target(creature)]);
    assert_eq!(life(&runner, P1), 17);
    assert_eq!(damage(&runner, creature), 3);
    assert_eq!(counters(&runner, bubble, &depletion()), 0);

    let mut scenario = self::scenario();
    phytohydra(&mut scenario, P1);
    let creature = scenario.add_creature(P1, "Bear", 2, 4).id();
    let spell = bolt(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![object_target(creature)]);
    assert_eq!(damage(&runner, creature), 3);

    let mut scenario = self::scenario();
    let hydra = phytohydra(&mut scenario, P0);
    let blocker = scenario.add_creature(P1, "Squire", 1, 1).id();
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![hydra], vec![(blocker, hydra)]);
    assert_eq!(plus_counters(&runner, hydra), 1);
    assert_eq!(zone(&runner, blocker), Zone::Graveyard);
}

/// Pyroclasm on Phytohydra and Lichenthrope: both events are replaced.
#[test]
fn two_recipients_are_each_replaced() {
    let mut scenario = scenario();
    let hydra = phytohydra(&mut scenario, P1);
    let lichenthrope = scenario
        .add_creature_from_oracle(P1, "Lichenthrope", 5, 5, LICHENTHROPE)
        .id();
    let spell = sorcery(&mut scenario, "Pyroclasm", PYROCLASM);
    let mut runner = scenario.build();
    let events = cast(&mut runner, spell, vec![]);

    assert_eq!(plus_counters(&runner, hydra), 2);
    assert_eq!(
        counters(&runner, lichenthrope, &CounterType::Minus1Minus1),
        2
    );
    assert_eq!(dealt(&events), 0);
    assert_at_rest(&runner);
}

/// CR 120.3f: lifelink gains for damage dealt, and a replaced event deals none (CR 614.6). The
/// unreplaced game is the reach guard: the same attacker gains 3 from the same 3 damage.
#[test]
fn lifelink_has_nothing_to_gain_from_replaced_damage() {
    for replaced in [true, false] {
        let mut scenario = scenario();
        let blocker = if replaced {
            phytohydra(&mut scenario, P1)
        } else {
            scenario.add_creature(P1, "Bear", 2, 9).id()
        };
        let attacker = scenario
            .add_creature(P0, "Aardvark Sloth", 3, 3)
            .from_oracle_text_with_keywords(&["Lifelink"], AARDVARK_SLOTH)
            .id();
        let mut runner = scenario.build();
        assert!(runner.state().objects[&attacker].has_keyword(&Keyword::Lifelink));
        run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);

        assert_eq!(
            plus_counters(&runner, blocker),
            if replaced { 3 } else { 0 }
        );
        assert_eq!(damage(&runner, blocker), if replaced { 0 } else { 3 });
        assert_eq!(life(&runner, P0), if replaced { 20 } else { 23 });
    }
}

/// Combat up to its first prompt (`combat_events`), then every `ReplacementChoice` answered with
/// the first source of `prefer` that it offers. Returns the events and each prompt's chooser and
/// candidate sources.
#[track_caller]
fn combat_answering(
    runner: &mut GameRunner,
    attackers: Vec<ObjectId>,
    blocks: Vec<(ObjectId, ObjectId)>,
    prefer: &[ObjectId],
) -> (Vec<GameEvent>, Vec<(PlayerId, Vec<ObjectId>)>) {
    let mut events = combat_events(runner, attackers, blocks);
    let mut prompts = Vec::new();
    while let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    {
        let sources: Vec<ObjectId> = candidates.iter().map(|c| c.source_id).collect();
        let pick = prefer
            .iter()
            .copied()
            .find(|source| sources.contains(source))
            .unwrap_or_else(|| panic!("an unexpected replacement prompt: {sources:?}"));
        prompts.push((*player, sources));
        choose_source(runner, &mut events, pick);
    }
    (events, prompts)
}

/// The candidate sources of one prompt, as a set.
fn source_set(sources: &[ObjectId]) -> std::collections::BTreeSet<ObjectId> {
    sources.iter().copied().collect()
}

/// Force Bubble ("If damage would be dealt to you, put that many depletion counters on this
/// enchantment instead. When there are four or more depletion counters on this enchantment,
/// sacrifice it.") under P0's Furnace of Rath in combat: P1, the player dealt damage, orders them
/// (CR 616.1), and the combat batch asks (CR 510.2 rules out priority, not the choice). An
/// unblocked 1/1: Force Bubble first puts 1 counter, Furnace first 2. A 3/3 after Stomp: Force
/// Bubble first puts 3 and stays, Furnace first puts 6 and it is sacrificed — a substitution is not
/// a prevention, so "can't be prevented" changes nothing (CR 615.12, CR 614.1a). P1 takes no
/// damage either way.
#[test]
fn combat_order_park_keeps_force_bubbles_damage() {
    for bubble_first in [true, false] {
        let mut scenario = scenario();
        let bubble = force_bubble(&mut scenario, P1);
        let furnace = furnace(&mut scenario);
        let attacker = scenario.add_creature(P0, "Bear", 1, 1).id();
        let mut runner = scenario.build();
        let first = if bubble_first { bubble } else { furnace };
        let (_, prompts) = combat_answering(&mut runner, vec![attacker], vec![], &[first]);

        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(prompts[0].0, P1);
        assert_eq!(source_set(&prompts[0].1), source_set(&[bubble, furnace]));
        assert_eq!(life(&runner, P1), 20);
        assert_eq!(
            counters(&runner, bubble, &depletion()),
            if bubble_first { 1 } else { 2 }
        );

        let mut scenario = self::scenario();
        let bubble = force_bubble(&mut scenario, P1);
        let furnace = self::furnace(&mut scenario);
        let attacker = scenario.add_creature(P0, "Ogre", 3, 3).id();
        let spell = stomp(&mut scenario);
        let mut runner = scenario.build();
        cast(&mut runner, spell, vec![player_target(P0)]);
        let first = if bubble_first { bubble } else { furnace };
        let (_, prompts) = combat_answering(&mut runner, vec![attacker], vec![], &[first]);

        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(life(&runner, P1), 20);
        if bubble_first {
            assert_eq!(zone(&runner, bubble), Zone::Battlefield);
            assert_eq!(counters(&runner, bubble, &depletion()), 3);
        } else {
            assert_eq!(zone(&runner, bubble), Zone::Graveyard);
        }
    }
}

/// Phytohydra ("If damage would be dealt to this creature, put that many +1/+1 counters on it
/// instead.") blocking a 3/3 under Furnace of Rath: its controller orders them (CR 616.1; ruling
/// S9). Phytohydra first puts 3 counters, Furnace first 6. It is dealt no damage.
#[test]
fn combat_order_park_keeps_phytohydras_damage() {
    for hydra_first in [true, false] {
        let mut scenario = scenario();
        let hydra = phytohydra(&mut scenario, P1);
        let furnace = furnace(&mut scenario);
        let attacker = scenario.add_creature(P0, "Ogre", 3, 3).id();
        let mut runner = scenario.build();
        let first = if hydra_first { hydra } else { furnace };
        let (_, prompts) = combat_answering(
            &mut runner,
            vec![attacker],
            vec![(hydra, attacker)],
            &[first],
        );

        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(prompts[0].0, P1);
        assert_eq!(
            plus_counters(&runner, hydra),
            if hydra_first { 3 } else { 6 }
        );
        assert_eq!(damage(&runner, hydra), 0);
    }
}

/// Polukranos, Unchained: "prevent that damage and remove that many +1/+1 counters from it."
/// Blocking a 2/2 under Furnace of Rath, its controller orders them (CR 616.1): Furnace first
/// prevents 4 and removes 4 (2 left), Polukranos first prevents 2 and removes 2 (4 left). The
/// prevention is one event (CR 615.5).
#[test]
fn combat_order_park_runs_a_prevention_rider_before_the_batch_drain() {
    for furnace_first in [true, false] {
        let mut scenario = scenario();
        let id = scenario
            .add_creature_from_oracle(P1, "Polukranos, Unchained", 0, 0, POLUKRANOS)
            .id();
        scenario.with_counter(id, CounterType::Plus1Plus1, 6);
        let furnace = furnace(&mut scenario);
        let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
        let mut runner = scenario.build();
        let first = if furnace_first { furnace } else { id };
        let (events, prompts) =
            combat_answering(&mut runner, vec![attacker], vec![(id, attacker)], &[first]);

        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(
            plus_counters(&runner, id),
            if furnace_first { 2 } else { 4 }
        );
        assert_eq!(damage(&runner, id), 0);
        assert_eq!(prevented(&events), 1);
    }
}

/// The prompts Dapper Shieldmate's shield counter and Furnace of Rath raise in one combat leg, as
/// measured: whether its controller is asked to order them (CR 616.1). Every order gives the same
/// result, so the rows below hold for each answer.
const SHIELDMATE_PROMPTS: usize = 1;

/// Dapper Shieldmate: "If a permanent with a shield counter is dealt unpreventable damage, that
/// damage will be dealt and a shield counter will still be removed." and "If a permanent that
/// would be dealt damage has more than one shield counter on it, that damage is prevented and
/// only one shield counter is removed." (CR 122.1c.) Under Furnace of Rath, every order its
/// controller could answer leaves one shield counter, and the damage is prevented (or, under
/// Stomp, dealt doubled: 4).
#[test]
fn combat_order_park_keeps_a_shield_counter_replacement() {
    for unpreventable in [false, true] {
        for furnace_first in [true, false] {
            let mut scenario = scenario();
            let id = scenario
                .add_creature_from_oracle(P1, "Dapper Shieldmate", 2, 6, SHIELDMATE)
                .id();
            scenario.with_counter(id, CounterType::Shield, 2);
            let furnace = furnace(&mut scenario);
            let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
            let spell = unpreventable.then(|| stomp(&mut scenario));
            let mut runner = scenario.build();
            if let Some(spell) = spell {
                cast(&mut runner, spell, vec![player_target(P1)]);
            }
            let first = if furnace_first { furnace } else { id };
            let (_, prompts) =
                combat_answering(&mut runner, vec![attacker], vec![(id, attacker)], &[first]);

            assert_eq!(prompts.len(), SHIELDMATE_PROMPTS, "{prompts:?}");
            assert_eq!(counters(&runner, id, &CounterType::Shield), 1);
            assert_eq!(damage(&runner, id), if unpreventable { 4 } else { 0 });
        }
    }
}

/// Torbran, Thane of Red Fell's ruling: "the player being dealt damage or the controller of the
/// permanent being dealt damage chooses an order in which to apply those effects." P1 is asked:
/// Torbran first (2+2)×2 = 8, Furnace of Rath first 2×2+2 = 6.
#[test]
fn combat_order_park_keeps_a_damage_modifiers_damage() {
    for torbran_first in [true, false] {
        let mut scenario = scenario();
        let torbran = scenario
            .add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN)
            .id();
        let furnace = furnace(&mut scenario);
        let attacker = scenario
            .add_creature(P0, "Goblin", 2, 2)
            .with_color(vec![ManaColor::Red])
            .id();
        let mut runner = scenario.build();
        let first = if torbran_first { torbran } else { furnace };
        let (_, prompts) = combat_answering(&mut runner, vec![attacker], vec![], &[first]);

        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(prompts[0].0, P1);
        assert_eq!(20 - life(&runner, P1), if torbran_first { 8 } else { 6 });
    }
}

/// CR 616.1f: "Once the chosen effect has been applied, this process is repeated". Torbran,
/// Furnace of Rath and Gratuitous Violence are asked until the event settles: Torbran first
/// ((2+2)×2)×2 = 16; one doubler, then Torbran, then the other ((2×2)+2)×2 = 12; both doublers
/// first (2×2×2)+2 = 10. With P1's Force Bubble in Gratuitous Violence's place, Force Bubble first
/// puts 2 counters on it; Torbran or Furnace first puts four or more, and it is sacrificed. P1
/// takes no damage on that board.
#[test]
fn combat_order_parks_repeat_until_the_event_settles() {
    for (order, lost) in [
        (["torbran", "furnace", "violence"], 16),
        (["furnace", "torbran", "violence"], 12),
        (["furnace", "violence", "torbran"], 10),
    ] {
        let mut scenario = scenario();
        let torbran = scenario
            .add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN)
            .id();
        let furnace = furnace(&mut scenario);
        let violence = scenario
            .add_enchantment_from_oracle(P0, "Gratuitous Violence", GRATUITOUS_VIOLENCE)
            .id();
        let attacker = scenario
            .add_creature(P0, "Goblin", 2, 2)
            .with_color(vec![ManaColor::Red])
            .id();
        let mut runner = scenario.build();
        let prefer: Vec<ObjectId> = order
            .iter()
            .map(|name| match *name {
                "torbran" => torbran,
                "furnace" => furnace,
                _ => violence,
            })
            .collect();
        let (_, prompts) = combat_answering(&mut runner, vec![attacker], vec![], &prefer);

        assert!(!prompts.is_empty(), "reach-guard: the order was asked");
        assert!(prompts.iter().all(|(player, _)| *player == P1));
        assert_eq!(20 - life(&runner, P1), lost, "{order:?}");
    }

    for first_name in ["bubble", "torbran", "furnace"] {
        let mut scenario = self::scenario();
        let torbran = scenario
            .add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN)
            .id();
        let furnace = furnace(&mut scenario);
        let bubble = force_bubble(&mut scenario, P1);
        let attacker = scenario
            .add_creature(P0, "Goblin", 2, 2)
            .with_color(vec![ManaColor::Red])
            .id();
        let mut runner = scenario.build();
        let prefer = match first_name {
            "bubble" => vec![bubble],
            "torbran" => vec![torbran, bubble],
            _ => vec![furnace, bubble],
        };
        let (_, prompts) = combat_answering(&mut runner, vec![attacker], vec![], &prefer);

        assert!(!prompts.is_empty(), "reach-guard: the order was asked");
        assert_eq!(life(&runner, P1), 20);
        if first_name == "bubble" {
            assert_eq!(counters(&runner, bubble, &depletion()), 2);
        } else {
            assert_eq!(zone(&runner, bubble), Zone::Graveyard, "{first_name} first");
        }
    }
}

/// Two 1/1 attackers, each blocked by its own Phytohydra, under Furnace of Rath: one prompt per
/// leg, each P1's (CR 616.1). Answering the first leg with its Phytohydra puts 1 counter on it;
/// answering the second leg with Furnace of Rath puts 2 on the other. Each answered effect's
/// substitution drains in its own leg (CR 616.1g). Then Phytohydra and Polukranos, Unchained
/// ("prevent that damage and remove that many +1/+1 counters from it") blocking a 1/1 and a
/// 2/2: Phytohydra for its leg and Furnace for Polukranos's gives 1 counter and 2 left on
/// Polukranos; Furnace for Phytohydra's leg and Polukranos for its own gives 2 counters and 4
/// left. No rider stays resident past its leg.
#[test]
fn combat_order_answer_drains_in_its_own_portion() {
    let mut scenario = scenario();
    let (first, second) = (phytohydra(&mut scenario, P1), phytohydra(&mut scenario, P1));
    let furnace = furnace(&mut scenario);
    let (one, two) = (
        scenario.add_creature(P0, "Squire", 1, 1).id(),
        scenario.add_creature(P0, "Squire", 1, 1).id(),
    );
    let mut runner = scenario.build();
    let (_, prompts) = combat_answering(
        &mut runner,
        vec![one, two],
        vec![(first, one), (second, two)],
        &[first, furnace],
    );

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts.iter().all(|(player, _)| *player == P1));
    assert_eq!(plus_counters(&runner, first), 1);
    assert_eq!(plus_counters(&runner, second), 2);

    for hydra_answer_is_hydra in [true, false] {
        let mut scenario = self::scenario();
        let hydra = phytohydra(&mut scenario, P1);
        let polukranos = scenario
            .add_creature_from_oracle(P1, "Polukranos, Unchained", 0, 0, POLUKRANOS)
            .id();
        scenario.with_counter(polukranos, CounterType::Plus1Plus1, 6);
        let furnace = self::furnace(&mut scenario);
        let (one, two) = (
            scenario.add_creature(P0, "Squire", 1, 1).id(),
            scenario.add_creature(P0, "Bear", 2, 2).id(),
        );
        let mut runner = scenario.build();
        // Each leg takes the first of `prefer` it offers: its own effect, or Furnace of Rath.
        let prefer = if hydra_answer_is_hydra {
            vec![hydra, furnace]
        } else {
            vec![polukranos, furnace]
        };
        let (_, prompts) = combat_answering(
            &mut runner,
            vec![one, two],
            vec![(hydra, one), (polukranos, two)],
            &prefer,
        );

        assert_eq!(prompts.len(), 2, "{prompts:?}");
        assert_eq!(
            (
                plus_counters(&runner, hydra),
                plus_counters(&runner, polukranos)
            ),
            if hydra_answer_is_hydra {
                (1, 2)
            } else {
                (2, 4)
            }
        );
        assert!(!runner.state().has_post_replacement_drain());
    }
}

/// Force Bubble: "You can't split the damage between them." Three unblocked 1/1s at P1, who
/// controls two Force Bubbles: P1 is asked once per source, and answering the same Force Bubble
/// each time puts all 3 counters on it.
///
/// strict-failure: the ruling says "damage from multiple sources that happens at one time (such
/// as combat damage) gives one choice, not one per source", and the engine asks three times
/// where the card gives one.
/// This gap is Force Bubble's own ruling. CR 700.1 ("The text of triggered abilities and
/// replacement effects defines the event they're looking for") keeps it from carrying over to a
/// source-qualified substitution, so no coverage marker claims it for the class, and none
/// records it for Force Bubble either: its coverage reports supported.
#[test]
fn two_force_bubbles_take_all_of_a_combat_batch_on_one() {
    let mut scenario = scenario();
    let (first, second) = (
        force_bubble(&mut scenario, P1),
        force_bubble(&mut scenario, P1),
    );
    let attackers: Vec<ObjectId> = (0..3)
        .map(|index| {
            scenario
                .add_creature(P0, &format!("Squire {index}"), 1, 1)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    let (_, prompts) = combat_answering(&mut runner, attackers, vec![], &[first]);

    assert_eq!(prompts.len(), 3, "{prompts:?}");
    assert!(prompts.iter().all(|(player, _)| *player == P1));
    assert_eq!(
        [
            counters(&runner, first, &depletion()),
            counters(&runner, second, &depletion())
        ],
        [3, 0]
    );
    assert_eq!(life(&runner, P1), 20);
}

/// Force Bubble: "You can't split the damage between them." Answering every prompt with one
/// Bubble is the legal path.
///
/// strict-failure: the ruling says "damage from multiple sources that happens at one time (such
/// as combat damage) gives one choice, not one per source", and the engine asks once per Dragon:
/// two prompts where the card gives one.
/// This gap is Force Bubble's own ruling. CR 700.1 ("The text of triggered abilities and
/// replacement effects defines the event they're looking for") keeps it from carrying over to a
/// source-qualified substitution, so no coverage marker claims it for the class, and none
/// records it for Force Bubble either: its coverage reports supported.
#[test]
fn a_repeated_answer_puts_a_noncombat_batch_on_one_force_bubble() {
    let mut scenario = scenario();
    let sarkhan = scenario
        .add_planeswalker_from_oracle(P0, "Sarkhan the Mad", "Sarkhan", 7, SARKHAN)
        .id();
    let (first, second) = (
        force_bubble(&mut scenario, P1),
        force_bubble(&mut scenario, P1),
    );
    scenario
        .add_creature(P0, "Whelp", 1, 1)
        .with_subtypes(vec!["Dragon"]);
    scenario
        .add_creature(P0, "Wyrm", 2, 2)
        .with_subtypes(vec!["Dragon"]);
    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&sarkhan].abilities.len() - 1;
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: sarkhan,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate")
        .events;
    settle(&mut runner, &mut vec![player_target(P1)], &mut events);

    assert_prompt_is_replacement_choice(&runner);
    let mut prompts = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        prompts += 1;
        choose_source(&mut runner, &mut events, first);
    }

    assert_eq!(prompts, 2);
    assert_eq!(counters(&runner, first, &depletion()), 3);
    assert_eq!(counters(&runner, second, &depletion()), 0);
    assert_eq!(life(&runner, P1), 20);
    assert_at_rest(&runner);
}

/// CR 615.7: "Each 1 damage that would be dealt to the "shielded" permanent or player is
/// prevented. Preventing 1 damage reduces the remaining shield by 1." Szadek, Lord of Secrets and
/// a 4/4 attack P1, who is under P0's Mending Hands ("Prevent the next 4 damage that would be
/// dealt to any target this turn."). Szadek's 5 meets both, in P1's order (CR 616.1). Szadek
/// first replaces all 5 (five +1/+1 counters, five milled) and prevents nothing, so the shield
/// isn't used up (CR 609.7b) and prevents the 4/4's 4. Mending Hands first is Szadek's ruling: "the
/// player can choose to first have Mending Hands prevent 4 of the damage and then apply Szadek's
/// ability so that it gets only one counter", and the 4/4's 4 is then dealt.
#[test]
fn szadeks_substitution_leaves_a_shared_shield_for_the_other_attacker() {
    for szadek_first in [true, false] {
        let mut scenario = scenario();
        let szadek = szadek(&mut scenario);
        let ogre = scenario.add_creature(P0, "Ogre", 4, 4).id();
        scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);
        let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
        let mut runner = scenario.build();
        cast(&mut runner, hands, vec![player_target(P1)]);
        let mut events = combat_events(&mut runner, vec![szadek, ogre], vec![]);

        // Reach: Szadek's damage meets the shield with all 4 left.
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("expected P1's order, got {:?}", runner.state().waiting_for);
        };
        assert_eq!(*player, P1);
        assert!(candidates.iter().any(|c| c.source_id == szadek));
        assert!(candidates.iter().any(|c| c.description == SHIELD));
        if szadek_first {
            choose_source(&mut runner, &mut events, szadek);
        } else {
            choose_shield(&mut runner, &mut events);
        }

        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "one order prompt"
        );
        let prevented_from: Vec<(ObjectId, u32)> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamagePrevented {
                    source_id,
                    target: TargetRef::Player(player),
                    amount,
                } if *player == P1 => Some((*source_id, *amount)),
                _ => None,
            })
            .collect();
        // CR 615.13: the shield's prevention publishes as one application per recipient, named
        // by the shield's host (`ObjectId(0)` for Mending Hands' floating shield), since one
        // application can span several damage sources (`combat_damage::push_prevented_events`).
        let shield_host = ObjectId(0);
        if szadek_first {
            assert_eq!(plus_counters(&runner, szadek), 5);
            assert_eq!(library_and_graveyard(&runner, P1), (2, 5));
            assert_eq!(prevented_from, vec![(shield_host, 4)]);
            assert_eq!(life(&runner, P1), 20);
        } else {
            assert_eq!(plus_counters(&runner, szadek), 1);
            assert_eq!(library_and_graveyard(&runner, P1), (6, 1));
            assert_eq!(prevented_from, vec![(shield_host, 4)]);
            assert_eq!(life(&runner, P1), 16);
        }
        assert_at_rest(&runner);
        assert!(runner.state().pending_combat_lifelink.is_none());
    }
}

/// Undead Alchemist: "If you control multiple Undead Alchemists, the multiple replacement
/// abilities will have no added effect. Combat damage dealt to a player by a Zombie you control
/// will be replaced only once with cards being put into that player's graveyard." Alchemist A
/// attacks P1 alone; both Alchemists apply to its 4, so P1 chooses one (CR 616.1). After it
/// applies the event is a mill (CR 614.6), which the other's "would deal combat damage" no longer
/// matches (CR 614.5, CR 616.1f): P1 mills 4, once. The milled cards are not creature cards, so
/// neither Alchemist's trigger creates a token.
#[test]
fn two_undead_alchemists_replace_a_zombies_damage_once() {
    for a_first in [true, false] {
        let mut scenario = scenario();
        let a = undead_alchemist(&mut scenario);
        let b = undead_alchemist(&mut scenario);
        scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);
        let mut runner = scenario.build();
        let first = if a_first { a } else { b };
        let (events, prompts) = combat_answering(&mut runner, vec![a], vec![], &[first]);

        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(prompts[0].0, P1);
        assert_eq!(source_set(&prompts[0].1), BTreeSet::from([a, b]));
        let applied: Vec<ObjectId> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ReplacementApplied { source_id, .. }
                    if *source_id == a || *source_id == b =>
                {
                    Some(*source_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(applied, vec![first]);
        assert_eq!(library_and_graveyard(&runner, P1), (3, 4));
        assert_eq!(life(&runner, P1), 20);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::TokenCreated { .. })));
        assert_at_rest(&runner);
        assert!(runner.state().pending_combat_lifelink.is_none());
    }
}

/// Battletide Alchemist: "If a source would deal damage to a player, you may prevent X of that
/// damage, where X is the number of Clerics you control." The "may" is P1's to answer, and the
/// combat batch asks it (CR 616.1, CR 615.5): accepting prevents 1 of the Ogre's 3 (the
/// Alchemist is the one Cleric) and P1 goes to 18; declining leaves all 3, and P1 goes to 17.
#[test]
fn a_parked_may_is_asked_in_combat() {
    // Reach: the same board offers the "may" in a noncombat event.
    let mut scenario = scenario();
    scenario
        .add_creature_from_oracle(P1, "Battletide Alchemist", 3, 4, ALCHEMIST)
        .with_subtypes(vec!["Kithkin", "Cleric"]);
    let spell = bolt(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![player_target(P1)]);
    assert_prompt_is_replacement_choice(&runner);
    assert!(runner
        .state()
        .pending_replacement
        .as_ref()
        .is_some_and(|pending| pending.is_optional));

    for accept in [true, false] {
        let mut scenario = self::scenario();
        scenario
            .add_creature_from_oracle(P1, "Battletide Alchemist", 3, 4, ALCHEMIST)
            .with_subtypes(vec!["Kithkin", "Cleric"]);
        let attacker = scenario.add_creature(P0, "Ogre", 3, 3).id();
        let mut runner = scenario.build();
        let mut events = combat_events(&mut runner, vec![attacker], vec![]);

        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected the Alchemist's may, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        assert!(runner
            .state()
            .pending_replacement
            .as_ref()
            .is_some_and(|pending| pending.is_optional));
        // CR 616.1: the Alchemist's own candidate accepts the "may"; "Decline" declines it.
        let index = candidates
            .iter()
            .position(|candidate| (candidate.description == "Decline") != accept)
            .unwrap_or_else(|| panic!("the may offers both answers: {candidates:?}"));
        answer(
            &mut runner,
            GameAction::ChooseReplacement { index },
            &mut events,
        );

        assert_eq!(life(&runner, P1), if accept { 18 } else { 17 });
        assert_eq!(prevented(&events), usize::from(accept));
        assert!(runner.state().pending_replacement.is_none());
    }
}

/// CR 701.14a + CR 615.5: the reverse hit's prevention rider finishes before Fight.
#[test]
fn reverse_fight_rider_holds_fight_completion_until_its_answer() {
    let mut scenario = scenario();
    let fighter = scenario.add_creature(P0, "Siege Mastodon", 3, 5).id();
    let opponent = scenario.add_creature(P1, "Eager Cadet", 1, 1).id();
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    // Verbatim Oracle text also used by unpreventable_damage_prevention_effects::PIT_FIGHT.
    let fight = instant(&mut scenario, "Pit Fight", "Target creature you control fights another target creature. (Each deals damage equal to its power to the other.)");
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(fighter)]);
    assert_at_rest(&runner);
    let mut events = cast(
        &mut runner,
        fight,
        vec![object_target(fighter), object_target(opponent)],
    );
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected counter order, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(completions(&events, EffectKind::Fight, fight).len(), 0);
    assert_eq!(plus_counters(&runner, fighter), 0);
    choose_source(&mut runner, &mut events, scales);
    // One prevented damage puts (1 + 1) * 2 counters, choosing Scales before Season.
    assert_eq!(plus_counters(&runner, fighter), 4);
    assert_eq!(completions(&events, EffectKind::Fight, fight).len(), 1);
    assert_eq!(damage(&runner, fighter), 0);
    assert_at_rest(&runner);
}

/// CR 616.1 + CR 101.4: every automatic redirect is found before anyone is asked. Rakdos Charm,
/// four players, P0 active. P1's sole Pariah sends P1's 1 to P2's Wall, and P3's sole Pariah
/// sends P3's 1 to P0's Wall; each Wall is under Test of Faith and Mending Hands, an order its
/// controller chooses. The outstanding choices are P0's and P2's, asked P0 first, and the batch
/// completes once.
#[test]
fn every_automatic_redirect_is_found_before_a_choice_is_asked() {
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.add_creature(p3, "Hill Giant", 3, 3);
    let p0_wall = scenario.add_creature(P0, "P0 Wall", 0, 20).id();
    let p2_wall = scenario.add_creature(P2, "P2 Wall", 0, 20).id();
    let pariah = |scenario: &mut GameScenario, owner| {
        scenario
            .add_enchantment_from_oracle(owner, "Pariah", PARIAH)
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
            .id()
    };
    let p1_pariah = pariah(&mut scenario, P1);
    let p3_pariah = pariah(&mut scenario, p3);
    let spells: Vec<ObjectId> = [TEST_OF_FAITH, MENDING_HANDS, TEST_OF_FAITH, MENDING_HANDS]
        .iter()
        .map(|text| instant(&mut scenario, "Shield", text))
        .collect();
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), p1_pariah, p2_wall);
    attach_to(runner.state_mut(), p3_pariah, p0_wall);
    for (spell, wall) in spells.iter().zip([p2_wall, p2_wall, p0_wall, p0_wall]) {
        cast(&mut runner, *spell, vec![object_target(wall)]);
    }
    assert_at_rest(&runner);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);

    let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| None);

    assert_eq!(prompts, vec![("order", P0, 0), ("order", P2, 0)]);
    assert_eq!((damage(&runner, p0_wall), damage(&runner, p2_wall)), (0, 0));
    assert_eq!((life(&runner, P1), life(&runner, p3)), (20, 20));
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// CR 616.1 + CR 101.4 at first discovery: Rakdos Charm, four players. P1's Pariah ("All damage
/// that would be dealt to you is dealt to enchanted creature instead.") enchants P3's Wall and is
/// the only replacement on P1's 1, so it redirects without a choice. The Wall's 1 then meets Test
/// of Faith and Mending Hands, an order P3 chooses; P2's own 1 meets Force Bubble and Delaying
/// Shield, an order P2 chooses. Both choices are outstanding at once, so P2 is asked before P3, and
/// nothing is dealt before the last replacement answer (CR 120.4b).
#[test]
fn an_automatic_redirect_waits_for_an_earlier_players_choice() {
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.add_creature(P2, "Hill Giant", 3, 3);
    let wall = scenario.add_creature(p3, "Wall", 0, 20).id();
    let pariah = scenario
        .add_enchantment_from_oracle(P1, "Pariah", PARIAH)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
        .id();
    let bubble = force_bubble(&mut scenario, P2);
    scenario.add_enchantment_from_oracle(P2, "Delaying Shield", DELAYING_SHIELD);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), pariah, wall);
    cast(&mut runner, faith, vec![object_target(wall)]);
    cast(&mut runner, hands, vec![object_target(wall)]);
    assert_at_rest(&runner);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);

    let prompts = answer_batch_prompts(&mut runner, &mut events, |player, _| {
        (player == P2).then_some(bubble)
    });

    assert_eq!(prompts, vec![("order", P2, 0), ("order", p3, 0)]);
    assert_eq!(counters(&runner, bubble, &depletion()), 1);
    assert_eq!(damage(&runner, wall), 0, "the Wall's 1 was prevented");
    assert_eq!(life(&runner, P1), 20, "P1's 1 went to the Wall");
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// CR 701.14a + CR 608.2f: Pit Fight ("Target creature you control fights another target
/// creature.") deals both directions as one simultaneous event. Furnace of Rath doubles each, and
/// each fighter is under its own Test of Faith, so each direction's damage meets two replacements,
/// an order its fighter's controller chooses (CR 616.1). The active player P0 is asked first, then
/// P1 (CR 101.4). Nothing is dealt before both answers (CR 120.4b), and the fight completes once.
#[test]
fn a_fights_two_directions_ask_their_choices_in_apnap_order() {
    let mut scenario = scenario();
    let ours = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let theirs = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    furnace(&mut scenario);
    let faith_ours = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let faith_theirs = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let pit_fight = instant(&mut scenario, "Pit Fight", PIT_FIGHT);
    let mut runner = scenario.build();
    cast(&mut runner, faith_ours, vec![object_target(ours)]);
    cast(&mut runner, faith_theirs, vec![object_target(theirs)]);
    let mut events = cast(
        &mut runner,
        pit_fight,
        vec![object_target(ours), object_target(theirs)],
    );

    let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| None);

    let order: Vec<(&str, PlayerId)> = prompts
        .iter()
        .take(2)
        .map(|(kind, player, _)| (*kind, *player))
        .collect();
    assert_eq!(order, vec![("order", P0), ("order", P1)], "{prompts:?}");
    assert!(
        prompts.iter().take(2).all(|(_, _, dealt)| *dealt == 0),
        "{prompts:?}"
    );
    assert_completes_once_after_its_damage(&events, EffectKind::Fight, pit_fight);
    assert_at_rest(&runner);
}

/// CR 616.1 + CR 615.5: a fight's original replacement choices are all made before any
/// prevention after-effect. Pit Fight, P0 active. P0's 1/1 fighter is under Test of Faith and
/// Mending Hands (two prevention shields on the reverse hit, so P0 orders them). P1's fighter is
/// under one Test of Faith, with Hardened Scales and Doubling Season, so the counters it puts
/// meet two counter replacements and P1 orders those. P0's shield order comes before P1's counter
/// order (CR 101.4), and the fight completes once.
#[test]
fn a_fight_settles_original_choices_before_a_counter_rider() {
    let mut scenario = scenario();
    let ours = scenario.add_creature(P0, "Goblin", 1, 1).id();
    let theirs = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let faith_ours = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
    let faith_theirs = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    scenario.add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON);
    let pit_fight = instant(&mut scenario, "Pit Fight", PIT_FIGHT);
    let mut runner = scenario.build();
    cast(&mut runner, faith_ours, vec![object_target(ours)]);
    cast(&mut runner, hands, vec![object_target(ours)]);
    cast(&mut runner, faith_theirs, vec![object_target(theirs)]);
    let mut events = cast(
        &mut runner,
        pit_fight,
        vec![object_target(ours), object_target(theirs)],
    );

    let prompts = answer_batch_prompts(&mut runner, &mut events, |_, _| None);

    let players: Vec<PlayerId> = prompts
        .iter()
        .filter(|(kind, _, _)| *kind == "order")
        .map(|(_, player, _)| *player)
        .collect();
    let p0_last = players.iter().rposition(|p| *p == P0);
    let p1_first = players.iter().position(|p| *p == P1);
    assert!(
        matches!((p0_last, p1_first), (Some(last), Some(first)) if last < first),
        "P0's shield order before P1's counter order: {prompts:?}"
    );
    assert_completes_once_after_its_damage(&events, EffectKind::Fight, pit_fight);
    assert_at_rest(&runner);
}

/// The F2 board with the seats reversed and counter replacements on both sides: P0's fighter has
/// one Test of Faith, P1's has Test of Faith and Mending Hands, and each player controls Hardened
/// Scales and Doubling Season. P0's shield applies on its own and its counters wait; P1 orders
/// its shields (choosing Test of Faith), and that answer's counters wait too (CR 616.1: it is one
/// of the fight's original choices). After the damage, the counters come in APNAP order
/// (CR 615.5 + CR 101.4): P0 orders its counter replacements before P1 does. Before the hold,
/// P1's counters were asked at P1's answer, ahead of P0's.
fn reversed_seat_fight() -> (GameRunner, Vec<GameEvent>, [ObjectId; 3]) {
    let mut scenario = scenario();
    let ours = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Goblin", 1, 1).id();
    let faith_ours = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let faith_theirs = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
    for player in [P0, P1] {
        scenario.add_enchantment_from_oracle(player, "Hardened Scales", HARDENED_SCALES);
        scenario.add_enchantment_from_oracle(player, "Doubling Season", DOUBLING_SEASON);
    }
    let pit_fight = instant(&mut scenario, "Pit Fight", PIT_FIGHT);
    let mut runner = scenario.build();
    cast(&mut runner, faith_ours, vec![object_target(ours)]);
    cast(&mut runner, faith_theirs, vec![object_target(theirs)]);
    cast(&mut runner, hands, vec![object_target(theirs)]);
    let events = cast(
        &mut runner,
        pit_fight,
        vec![object_target(ours), object_target(theirs)],
    );
    (runner, events, [ours, theirs, pit_fight])
}

#[test]
fn a_fight_holds_an_answered_shields_rider_for_apnap_order() {
    let (mut runner, mut events, [ours, theirs, pit_fight]) = reversed_seat_fight();

    // Each first candidate: P1's Test of Faith (cast before Mending Hands, CR 616.1).
    let players: Vec<PlayerId> = answer_batch_prompts(&mut runner, &mut events, |_, _| None)
        .into_iter()
        .filter(|(kind, _, _)| *kind == "order")
        .map(|(_, player, _)| player)
        .collect();

    // P1's shield order first; then every counter order of P0's before any of P1's.
    assert_eq!(players.first(), Some(&P1), "got {players:?}");
    let counter_orders = &players[1..];
    let p0_last = counter_orders.iter().rposition(|p| *p == P0);
    let p1_first = counter_orders.iter().position(|p| *p == P1);
    assert!(
        matches!((p0_last, p1_first), (Some(last), Some(first)) if last < first),
        "P0's counters before P1's: {players:?}"
    );
    // P1's 1 to P0's Bears and P0's 2 to P1's Goblin are prevented; each 1 counter becomes 3 or
    // 4 under Hardened Scales and Doubling Season, depending on the order.
    assert_eq!(damage(&runner, ours), 0);
    assert_eq!(damage(&runner, theirs), 0);
    assert!(
        [3, 4].contains(&plus_counters(&runner, ours)),
        "got {}",
        plus_counters(&runner, ours)
    );
    assert!(
        [5, 6].contains(&plus_counters(&runner, theirs)),
        "got {}",
        plus_counters(&runner, theirs)
    );
    assert_completes_once_after_its_damage(&events, EffectKind::Fight, pit_fight);
    assert_at_rest(&runner);
}

/// CR 104.1 + CR 800.4a: a fight parked on a held rider's prompt ends with the game. On the
/// reversed-seat board, P1 concedes at P0's counter prompt: the game is over and nothing of the
/// batch is left to resume.
#[test]
fn a_fight_parked_on_a_held_rider_ends_with_the_game() {
    let (mut runner, mut events, _) = reversed_seat_fight();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P1, .. }
        ),
        "reach: P1 orders its shields, got {:?}",
        runner.state().waiting_for
    );
    // The first candidate: Test of Faith, cast before Mending Hands.
    answer(
        &mut runner,
        GameAction::ChooseReplacement { index: 0 },
        &mut events,
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ),
        "reach: the batch is parked on P0's counter order, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");

    assert!(runner.state().game_end.is_some());
    assert_eq!(
        runner.state().resolution_stack.len(),
        0,
        "no damage batch is left to resume"
    );
}

/// Soul-Scar Mage's ruling: "If an effect causes a creature you control to fight a creature an
/// opponent controls, the damage is dealt simultaneously. For instance, if your 2/2 creature
/// fights an opponent's 4/4 creature, your creature will be dealt 4 damage and your opponent's
/// creature will have two -1/-1 counters put on it." (CR 701.14a.) The same fight without
/// Soul-Scar Mage is the reach guard: the 4/4 is dealt 2.
#[test]
fn soul_scar_mage_fight_replaces_one_direction() {
    for with_mage in [true, false] {
        let mut scenario = scenario();
        if with_mage {
            soul_scar_mage(&mut scenario, P0);
        }
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let theirs = scenario.add_creature(P1, "Vanilla 4/4", 4, 4).id();
        let pit_fight = instant(&mut scenario, "Pit Fight", PIT_FIGHT);
        let mut runner = scenario.build();
        let events = cast(
            &mut runner,
            pit_fight,
            vec![object_target(bears), object_target(theirs)],
        );

        assert_completes_once_after_its_damage(&events, EffectKind::Fight, pit_fight);
        assert_eq!(
            zone(&runner, bears),
            Zone::Graveyard,
            "the Bears are dealt 4"
        );
        if with_mage {
            assert_eq!(counters(&runner, theirs, &CounterType::Minus1Minus1), 2);
            assert_eq!(damage(&runner, theirs), 0);
        } else {
            assert_eq!(counters(&runner, theirs, &CounterType::Minus1Minus1), 0);
            assert_eq!(damage(&runner, theirs), 2);
        }
        assert_at_rest(&runner);
    }
}

/// Pit Fight deals both directions at once (CR 701.14a). P0's Bears, under P0's Test of Faith,
/// fight P1's 4/4 under P0's Soul-Scar Mage. The Bears' 2 becomes "that many -1/-1 counters" on
/// the 4/4 (CR 614.6), the modified event; the 4/4's 4 meets Test of Faith, which prevents 3, and
/// its "put a +1/+1 counter" is the rest of its effect, immediately afterward (CR 615.5). So the
/// substitute's work comes before the rider's, though P0 is active: Phase D's APNAP rank
/// (CR 101.4) orders held work only within one timing.
///
/// P1's Doubling Season and Vizier of Remedies both apply to the 2 counters on P1's creature, in
/// P1's order (CR 616.1): Season first 2 × 2 − 1 = 3, Vizier first (2 − 1) × 2 = 2. P0's Hardened
/// Scales and Doubling Season apply to Test of Faith's counters on P0's Bears, in P0's order. Each
/// player's Doubling Season reads "a permanent you control", so neither appears in the other's
/// prompt.
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174), so
/// P0 is asked three times, (1+1) × 2 = 4 or 1 × 2 + 1 = 3 each: 12 or 9, and the prompts are P1,
/// P0, P0, P0. The card puts its three counters in one event: (3+1) × 2 = 8 or 3 × 2 + 1 = 7, and
/// the prompts are P1, P0.
#[test]
fn a_soul_scar_fight_substitution_comes_with_the_event() {
    for (p1_season_first, p0_scales_first, minus, each, total) in
        [(true, true, 3, 4, 12), (false, false, 2, 3, 9)]
    {
        let mut scenario = scenario();
        soul_scar_mage(&mut scenario, P0);
        let scales = scenario
            .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
            .id();
        let season_p0 = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON)
            .id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let theirs = scenario.add_creature(P1, "Vanilla 4/4", 4, 4).id();
        let season_p1 = scenario
            .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
            .id();
        let vizier = vizier_of_remedies(&mut scenario, P1);
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let pit_fight = instant(&mut scenario, "Pit Fight", PIT_FIGHT);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(bears)]);
        let mut events = cast(
            &mut runner,
            pit_fight,
            vec![object_target(bears), object_target(theirs)],
        );

        let mut prompts = Vec::new();
        while matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            let (chooser, sources) = replacement_prompt(&runner);
            prompts.push((chooser, sources, plus_counters(&runner, bears)));
            let pick = match (chooser == P1, p1_season_first, p0_scales_first) {
                (true, true, _) => season_p1,
                (true, false, _) => vizier,
                (false, _, true) => scales,
                (false, _, false) => season_p0,
            };
            choose_source(&mut runner, &mut events, pick);
        }

        assert_eq!(prompts.len(), 4, "{prompts:?}");
        let choosers: Vec<PlayerId> = prompts.iter().map(|(chooser, _, _)| *chooser).collect();
        assert_eq!(choosers, vec![P1, P0, P0, P0], "{prompts:?}");
        assert_eq!(prompts[0].1, BTreeSet::from([season_p1, vizier]));
        assert!(
            prompts[1..]
                .iter()
                .all(|(_, sources, _)| *sources == BTreeSet::from([scales, season_p0])),
            "{prompts:?}"
        );
        let seen: Vec<u32> = prompts.iter().map(|(_, _, plus)| *plus).collect();
        assert_eq!(seen, vec![0, 0, each, 2 * each], "{prompts:?}");
        assert_eq!(counters(&runner, theirs, &CounterType::Minus1Minus1), minus);
        assert_eq!(damage(&runner, theirs), 0);
        assert_eq!(plus_counters(&runner, bears), total);
        assert_eq!(damage(&runner, bears), 1);
        assert_completes_once_after_its_damage(&events, EffectKind::Fight, pit_fight);
        assert_at_rest(&runner);
    }
}

const AWE_STRIKE: &str = "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.";

/// CR 615.7 + CR 616.1: the player chooses which damage a shield prevents. Mending Hands ("Prevent
/// the next 4 damage that would be dealt to any target this turn.") is on P1's Wall of Stone and
/// Awe Strike on P0's Hill Giant; Allies at Last has the Giant and Rumbling Baloth deal damage
/// to the Wall, with either selected first. The Giant's 3 meets both shields, P1's choice; the Baloth's 4 meets only
/// Mending Hands, and waits for that choice instead of spending the shield first. Mending Hands
/// for the Giant leaves 1 of it for the Baloth: the Wall takes 3 and P0 gains nothing. Awe Strike
/// for the Giant leaves all 4 for the Baloth: the Wall takes none and P0 gains 3.
/// STRICT FAILURE (CR 615.7, fenced by the `shared_shield_split_unoffered` coverage gap): P1 may
/// also split Mending Hands, 2 on each source (the Wall takes 2 and P0 gains 1). That split is not
/// offered; the shield meets one source's damage in full.
#[test]
fn a_batch_keeps_a_shared_shield_for_the_players_choice() {
    for (baloth_first, pick_hands) in [(false, true), (false, false), (true, true), (true, false)] {
        let mut scenario = scenario();
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let baloth = scenario.add_creature(P0, "Rumbling Baloth", 4, 4).id();
        let wall = scenario.add_creature(P1, "Wall of Stone", 0, 8).id();
        let hands = instant(&mut scenario, "Mending Hands", MENDING_HANDS);
        let awe = instant(&mut scenario, "Awe Strike", AWE_STRIKE);
        let allies = sorcery(&mut scenario, "Allies at Last", ALLIES_AT_LAST);
        let mut runner = scenario.build();
        cast(&mut runner, hands, vec![object_target(wall)]);
        cast(&mut runner, awe, vec![object_target(giant)]);
        let mut events = cast(
            &mut runner,
            allies,
            // The batch replaces the sources in their selected order (CR 115.1).
            if baloth_first {
                vec![
                    object_target(baloth),
                    object_target(giant),
                    object_target(wall),
                ]
            } else {
                vec![
                    object_target(giant),
                    object_target(baloth),
                    object_target(wall),
                ]
            },
        );

        let mut prompts = 0;
        while let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = runner.state().waiting_for.clone()
        {
            assert_eq!(player, P1, "the Wall's controller chooses");
            assert_eq!(
                candidates.len(),
                2,
                "both shields are still offered: {candidates:?}"
            );
            prompts += 1;
            // Mending Hands was cast first, so it is the first candidate.
            let index = if pick_hands { 0 } else { 1 };
            answer(
                &mut runner,
                GameAction::ChooseReplacement { index },
                &mut events,
            );
        }

        assert_eq!(
            prompts, 1,
            "baloth_first {baloth_first}, pick_hands {pick_hands}"
        );
        assert_at_rest(&runner);
        if pick_hands {
            assert_eq!(damage(&runner, wall), 3);
            assert_eq!(life(&runner, P0), 20);
        } else {
            assert_eq!(damage(&runner, wall), 0);
            assert_eq!(life(&runner, P0), 23);
        }
        assert_completes_once_after_its_damage(
            &events,
            EffectKind::EachDealsDamageEqualToPower,
            allies,
        );
    }
}

/// CR 616.1 + CR 101.4: P2's outstanding order precedes a new P3 order after redirect.
#[test]
fn redirected_damage_choices_follow_four_player_apnap_order() {
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.add_creature(P2, "Hill Giant", 3, 3);
    let wall = scenario.add_creature(p3, "Wall", 0, 20).id();
    let pariah = scenario
        .add_enchantment_from_oracle(P1, "Pariah", PARIAH)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
        .id();
    furnace(&mut scenario);
    let bubble = force_bubble(&mut scenario, P2);
    scenario.add_enchantment_from_oracle(P2, "Delaying Shield", DELAYING_SHIELD);
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), pariah, wall);
    assert!(runner.state().objects[&wall].attachments.contains(&pariah));
    assert_eq!(runner.state().objects[&pariah].controller, P1);
    assert_eq!(runner.state().objects[&wall].controller, p3);
    cast(&mut runner, faith, vec![object_target(wall)]);
    assert_at_rest(&runner);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    let prompts = answer_batch_prompts(&mut runner, &mut events, |player, _| {
        if player == P1 {
            Some(pariah)
        } else if player == P2 {
            Some(bubble)
        } else {
            Some(wall)
        }
    });
    let order: Vec<_> = prompts
        .iter()
        .map(|(kind, player, _)| (*kind, *player))
        .collect();
    eprintln!("four-player redirect prompt order: {order:?}");
    assert_eq!(order, vec![("order", P1), ("order", P2), ("order", p3)]);
    assert_eq!(counters(&runner, bubble, &depletion()), 1);
    assert_eq!(plus_counters(&runner, wall), 1);
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// CR 616.1 + CR 614.6 + CR 104.1: a terminal substitution chosen by an ANSWER waits for the
/// batch's other original choices and happens with its damage. P1 orders Nefarious Lich over
/// Force Bubble for each Bear; the loss comes after the second answer, with P0's Cadet's 1, and
/// the batch never completes.
#[test]
fn a_terminal_substitution_answer_waits_for_the_batch() {
    let mut scenario = scenario();
    let cached = scenario.add_creature(P0, "Eager Cadet", 1, 1).id();
    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let later = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let lich = nefarious_lich(&mut scenario, 0);
    force_bubble(&mut scenario, P1);
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected damage order, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    assert!(runner.state().game_end.is_none());
    assert!(self::player(&runner, P1).graveyard.is_empty());
    assert_eq!(dealt(&events), 0);
    choose_source(&mut runner, &mut events, lich);
    assert!(
        runner.state().game_end.is_none(),
        "the first answer's loss waits for the batch"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    choose_source(&mut runner, &mut events, lich);
    assert_game_over(&runner, P0);
    assert!(events.iter().any(
        |e| matches!(e, GameEvent::ReplacementApplied { source_id, .. } if *source_id == lich)
    ));
    assert!(!events.iter().any(|e| matches!(e, GameEvent::DamageDealt { source_id, .. } if *source_id == first || *source_id == later)));
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::DamageDealt { source_id, .. } if *source_id == cached)));
    assert_eq!(life(&runner, P0), 19);
    assert!(completions(&events, EffectKind::EachSourceDealsDamage, charm).is_empty());
}

/// Two Pariahs let P1 redirect its two sources to different later players.
fn two_redirect_fixture(
    lifelink: bool,
    first_controller: PlayerId,
) -> (
    GameRunner,
    ObjectId,
    [ObjectId; 2],
    [ObjectId; 2],
    Vec<ObjectId>,
) {
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 11);
    scenario.at_phase(Phase::PreCombatMain);
    if lifelink {
        scenario
            .add_creature(P1, "Aardvark Sloth", 3, 3)
            .from_oracle_text_with_keywords(&["Lifelink"], AARDVARK_SLOTH);
    } else {
        scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    }
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    let walls = [
        scenario
            .add_creature(first_controller, "Wall of Stone", 0, 8)
            .id(),
        scenario.add_creature(p3, "Wall of Stone", 0, 8).id(),
    ];
    let pariahs = std::array::from_fn(|_| {
        scenario
            .add_enchantment_from_oracle(P1, "Pariah", PARIAH)
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(&["Enchant"], PARIAH)
            .id()
    });
    let mut furnaces = vec![furnace(&mut scenario)];
    if lifelink {
        furnaces.push(furnace(&mut scenario));
    }
    let faiths = [
        instant(&mut scenario, "Test of Faith", TEST_OF_FAITH),
        instant(&mut scenario, "Test of Faith", TEST_OF_FAITH),
    ];
    let charm = instant(&mut scenario, "Rakdos Charm", RAKDOS_CHARM);
    let mut runner = scenario.build();
    for (aura, wall) in pariahs.into_iter().zip(walls) {
        attach_to(runner.state_mut(), aura, wall);
    }
    for (faith, wall) in faiths.into_iter().zip(walls) {
        cast(&mut runner, faith, vec![object_target(wall)]);
        assert_at_rest(&runner);
    }
    (runner, charm, pariahs, walls, furnaces)
}

#[track_caller]
fn assert_damage_chooser(runner: &GameRunner, expected: PlayerId) {
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected replacement choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, expected);
}

/// CR 101.4: both redirected portions remain outstanding, ordered by their new choosers.
#[test]
fn two_redirected_outstanding_choices_follow_apnap_and_complete_once() {
    let (mut runner, charm, pariahs, walls, _) = two_redirect_fixture(false, P2);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    assert_damage_chooser(&runner, P1);
    choose_source(&mut runner, &mut events, pariahs[0]);
    assert_damage_chooser(&runner, P1);
    choose_source(&mut runner, &mut events, pariahs[1]);
    let prompts = answer_batch_prompts(&mut runner, &mut events, |player, _| {
        Some(walls[if player == P2 { 0 } else { 1 }])
    });
    assert_eq!(
        prompts.iter().map(|(_, p, _)| *p).collect::<Vec<_>>(),
        vec![P2, PlayerId(3)]
    );
    assert_eq!(plus_counters(&runner, walls[0]), 1);
    assert_eq!(plus_counters(&runner, walls[1]), 1);
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// CR 101.4d: redirecting P1's choice to P0 restarts ahead of the outstanding P1 choice.
#[test]
fn redirected_damage_restarts_apnap_for_an_earlier_player() {
    let (mut runner, charm, pariahs, walls, _) = two_redirect_fixture(false, P0);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    assert_damage_chooser(&runner, P1);
    choose_source(&mut runner, &mut events, pariahs[0]);
    assert_damage_chooser(&runner, P0);
    choose_source(&mut runner, &mut events, walls[0]);
    assert_damage_chooser(&runner, P1);
    choose_source(&mut runner, &mut events, pariahs[1]);
    assert_damage_chooser(&runner, PlayerId(3));
    choose_source(&mut runner, &mut events, walls[1]);
    assert_eq!(plus_counters(&runner, walls[0]), 1);
    assert_eq!(plus_counters(&runner, walls[1]), 1);
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

/// CR 101.4: a parked later-player choice survives a full GameState save and restore.
#[test]
fn deferred_damage_queue_survives_game_state_serde_round_trip() {
    let (mut runner, charm, pariahs, walls, _) = two_redirect_fixture(false, P2);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    choose_source(&mut runner, &mut events, pariahs[0]);
    assert_damage_chooser(&runner, P1);
    assert_eq!(
        runner
            .state()
            .active_ability_continuation()
            .unwrap()
            .chain
            .context
            .parked_damage_batch
            .as_ref()
            .map_or(0, |parked| parked.deferred.len()),
        1,
        "reach: a choice is deferred"
    );
    let mut uninterrupted = GameRunner::from_state(runner.state().clone());
    // Same full-state round trip as yunas_whistle::round_trip.
    let json = serde_json::to_string(runner.state()).expect("serialize GameState");
    *runner.state_mut() = serde_json::from_str(&json).expect("deserialize GameState");
    let mut uninterrupted_events = events.clone();
    for (r, e) in [
        (&mut runner, &mut events),
        (&mut uninterrupted, &mut uninterrupted_events),
    ] {
        choose_source(r, e, pariahs[1]);
        let prompts = answer_batch_prompts(r, e, |player, _| {
            Some(walls[if player == P2 { 0 } else { 1 }])
        });
        assert_eq!(
            prompts.iter().map(|(_, p, _)| *p).collect::<Vec<_>>(),
            vec![P2, PlayerId(3)]
        );
        assert_completes_once_after_its_damage(e, EffectKind::EachSourceDealsDamage, charm);
        assert_at_rest(r);
    }
    assert_eq!(
        serde_json::to_value(runner.state()).unwrap(),
        serde_json::to_value(uninterrupted.state()).unwrap()
    );
    assert_eq!(events, uninterrupted_events);
}

/// CR 120.3f: a deferred lifelink source gains once for its damage remaining after prevention.
#[test]
fn deferred_damage_source_keeps_lifelink_exactly_once() {
    let (mut runner, charm, pariahs, walls, furnaces) = two_redirect_fixture(true, P2);
    let mut events = cast_rakdos_charm_damage_mode(&mut runner, charm);
    choose_source(&mut runner, &mut events, pariahs[0]);
    assert_damage_chooser(&runner, P1);
    choose_source(&mut runner, &mut events, pariahs[1]);
    let prompts = answer_batch_prompts(&mut runner, &mut events, |player, candidates| {
        furnaces
            .iter()
            .copied()
            .find(|id| candidates.iter().any(|c| c.source_id == *id))
            .or(Some(walls[if player == P2 { 0 } else { 1 }]))
    });
    assert_eq!(
        prompts.iter().map(|(_, p, _)| *p).collect::<Vec<_>>(),
        vec![P2, P2, PlayerId(3), PlayerId(3)]
    );
    // Both Furnaces turn 1 into 4; Test of Faith prevents 3, leaving 1 damage.
    assert_eq!(damage(&runner, walls[0]), 1);
    assert_eq!(life(&runner, P1), 21);
    assert_completes_once_after_its_damage(&events, EffectKind::EachSourceDealsDamage, charm);
    assert_at_rest(&runner);
}

const FATEFUL_SHOWDOWN: &str = "Fateful Showdown deals damage to any target equal to the number of cards in your hand. Discard all the cards in your hand, then draw that many cards.";
const PRIZEFIGHT: &str = "Target creature you control fights target creature you don't control. (Each deals damage equal to its power to the other.)\nCreate a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";

/// The damage events dealt to `recipient`, in order.
fn dealt_to(events: &[GameEvent], recipient: ObjectId) -> Vec<u32> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                target: TargetRef::Object(id),
                amount,
                ..
            } if *id == recipient => Some(*amount),
            _ => None,
        })
        .collect()
}

/// CR 615.5 + CR 608.2c: a prevention rider's amount belongs to the rider alone. Fateful
/// Showdown ("deals damage to any target equal to the number of cards in your hand. Discard all
/// the cards in your hand, then draw that many cards.") with five cards in hand at P1's 3/3
/// under Test of Faith ("For each 1 damage prevented this way, put a +1/+1 counter on that
/// creature."): 3 of the 5 damage is prevented and its counters are put on the 3/3, which is
/// dealt 2. The spell's next instructions follow: P0 discards five and draws "that many", the
/// five discarded, never the rider's 3. With P1's Hardened Scales and Doubling Season the rider
/// pauses on its counter orders, and the spell's tail waits behind them, still drawing five.
#[test]
fn a_riders_amount_does_not_reach_the_spells_tail() {
    for paused in [false, true] {
        let mut scenario = self::scenario();
        let creature = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        let season = paused.then(|| {
            scenario.add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES);
            scenario
                .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
                .id()
        });
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let showdown = instant(&mut scenario, "Fateful Showdown", FATEFUL_SHOWDOWN);
        for index in 0..5 {
            scenario.add_card_to_hand(P0, &format!("Held {index}"));
        }
        scenario.with_library_top(
            P0,
            &[
                "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight",
            ],
        );
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(creature)]);
        let graveyard_before = player(&runner, P0).graveyard.len();
        let mut events = cast(&mut runner, showdown, vec![object_target(creature)]);
        let mut orders = 0;
        while matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            orders += 1;
            choose_source(
                &mut runner,
                &mut events,
                season.expect("only the paused variant asks"),
            );
        }
        assert_eq!(
            orders > 0,
            paused,
            "the rider pauses only under the counter replacements"
        );

        // Reach-guard: the prevention applied and its rider put counters on the 3/3.
        let counter_events: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                matches!(event, GameEvent::CounterAdded { object_id, .. } if *object_id == creature)
            })
            .map(|(index, _)| index)
            .collect();
        assert!(
            !counter_events.is_empty(),
            "Test of Faith's counters were put"
        );
        assert_eq!(damage(&runner, creature), 2);

        let drawn: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P0)
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            drawn.len(),
            5,
            "P0 draws the five discarded, not the rider's 3"
        );
        // The five discarded, then Fateful Showdown itself (Test of Faith was already there).
        assert_eq!(player(&runner, P0).graveyard.len(), graveyard_before + 6);
        if paused {
            let last_counter = *counter_events.last().expect("counters were put");
            assert!(
                drawn.iter().all(|&index| index > last_counter),
                "the spell's tail follows the rider's paused counters"
            );
        }
        assert_at_rest(&runner);
        assert!(runner.state().pending_replacement.is_none());
    }
}

/// CR 701.14a + CR 608.2c + CR 616.1: a fight's tail is placed once. Prizefight ("Target
/// creature you control fights target creature you don't control. … Create a Treasure token.")
/// between P0's red 5/5 and P1's 2/2, under P0's Torbran, Thane of Red Fell ("it deals that much
/// damage plus 2 instead") and Furnace of Rath ("it deals double that damage … instead"). The
/// two directions are one batch. The 5/5's 5 to the 2/2 meets both, and P1, the controller of
/// the creature dealt damage, orders them: Torbran first (5+2)×2 = 14, Furnace first 5×2+2 = 12.
/// The 2/2's 2 to the 5/5 meets only Furnace: 4. Then the next instruction runs once: P0
/// creates exactly one Treasure (CR 111.10a).
#[test]
fn a_paused_fights_tail_runs_once() {
    for torbran_first in [true, false] {
        let mut scenario = self::scenario();
        let torbran = scenario
            .add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN)
            .id();
        let furnace = furnace(&mut scenario);
        let ours = scenario
            .add_creature(P0, "Red Brute", 5, 5)
            .with_color(vec![ManaColor::Red])
            .id();
        let theirs = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let prizefight = instant(&mut scenario, "Prizefight", PRIZEFIGHT);
        let mut runner = scenario.build();
        let treasures = |runner: &GameRunner| {
            runner
                .state()
                .battlefield
                .iter()
                .filter(|&&id| {
                    let object = &runner.state().objects[&id];
                    object.controller == P0
                        && object.card_types.subtypes.iter().any(|t| t == "Treasure")
                })
                .count()
        };
        let mut events = cast(
            &mut runner,
            prizefight,
            vec![object_target(ours), object_target(theirs)],
        );

        // Reach-guard: the fight parked on P1's order of Torbran and Furnace of Rath.
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected ReplacementChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        let mut offered: Vec<ObjectId> = candidates.iter().map(|c| c.source_id).collect();
        offered.sort();
        let mut expected = vec![torbran, furnace];
        expected.sort();
        assert_eq!(offered, expected);
        assert_eq!(treasures(&runner), 0, "the tail waits for the fight");

        choose_source(
            &mut runner,
            &mut events,
            if torbran_first { torbran } else { furnace },
        );

        assert_eq!(treasures(&runner), 1, "the fight's tail ran exactly once");
        assert_eq!(
            dealt_to(&events, theirs),
            vec![if torbran_first { 14 } else { 12 }]
        );
        assert_eq!(dealt_to(&events, ours), vec![4]);
        assert_eq!(zone(&runner, theirs), Zone::Graveyard);
        assert_completes_once_after_its_damage(&events, EffectKind::Fight, prizefight);
        assert_at_rest(&runner);
        assert!(runner.state().pending_replacement.is_none());
        assert!(runner.state().pending_combat_lifelink.is_none());
    }
}
