//! "If damage would be dealt to X, <Y> that many … instead" is a replacement effect, not a
//! prevention effect (CR 614.1a, CR 615.1a), and it is order-material against every other
//! damage replacement (CR 616.1).
//!
//! The seven lines are Phytohydra, Lichenthrope, Force Bubble, Delaying Shield, Crumbling
//! Sanctuary, Dralnu, Lich Lord and Nefarious Lich. Phytohydra's ruling states the class:
//! "Phytohydra's ability doesn't prevent damage. Damage that can't be prevented will still be
//! replaced by +1/+1 counters being put on Phytohydra."
//!
//! Three seams meet here, and each row below goes through the production pipeline:
//!
//! - A rider that picks from a population acts on what the player picks, and the spell's next
//!   instruction follows all of it (CR 608.2c, CR 608.2d): Lightning Helix's "and you gain 3
//!   life" under Nefarious Lich.
//! - Combat damage can't pause for a CR 616.1 order choice yet (Command Zone #140), so each
//!   order park inside a batch takes one legal order instead of dropping the damage (CR 510.2).
//!   Candidate order is scan order, so those rows assert the legal set, never one member.
//! - A spell's later instruction runs once, behind its remaining recipients, whichever
//!   replacement answer reaches it.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, EffectKind, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, ReplacementCandidateSummary, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
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

/// Pyroclasm: "deals 2 damage to each creature." A rider's prompt doesn't stop the other
/// recipients, whichever is dealt damage first (CR 120.3, CR 608.2d).
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
        assert_eq!(damage(&runner, bear), 2);
        assert_eq!(damage(&runner, id), 0);
        take_offered(&mut runner, &mut events);
        assert_eq!(damage(&runner, bear), 2);
        assert_at_rest(&runner);
    }

    let mut scenario = scenario();
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

/// Test of Faith's ruling: "If a 1/1 creature would be dealt 6 damage, 3 damage is prevented and
/// three +1/+1 counters are put on the creature." The last damage is then replaced, and each
/// effect's "that many" reads its own amount (CR 614.6, CR 615.5).
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

    // Dralnu: the shield's rider runs ahead of the sacrifice prompt.
    for shield_first in [true, false] {
        let mut scenario = self::scenario();
        let id = dralnu(&mut scenario, P0);
        lands(&mut scenario, P0, 4);
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(id)]);
        let mut events = cast(&mut runner, slash, vec![object_target(id)]);
        if shield_first {
            choose_shield(&mut runner, &mut events);
            assert_eq!(plus_counters(&runner, id), 3);
            assert_eq!(zone_prompt(&runner), (1, 5));
        } else {
            choose_not_shield(&mut runner, &mut events);
            assert_eq!(plus_counters(&runner, id), 0);
            assert_eq!(zone_prompt(&runner), (4, 5));
        }
        take_offered(&mut runner, &mut events);
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

/// Test of Faith: "For each 1 damage prevented this way, put a +1/+1 counter on that
/// creature." Dralnu, Lich Lord: "If damage would be dealt to Dralnu, sacrifice that many
/// permanents instead." With Hardened Scales and Doubling Season, every counter event is
/// ordered (CR 616.1), and the sacrifice follows all of them (CR 608.2c, CR 615.5): sacrificing
/// Hardened Scales cannot change a counter event that already happened.
///
/// strict-failure: Test of Faith's counters parse as one counter at a time (Command Zone #174),
/// so the engine asks three orders and puts 12 (Scales first) or 9 (Season first). The card puts
/// three counters in one event: 8 or 7, with one order.
#[test]
fn an_earlier_riders_paused_counters_finish_before_the_substitution() {
    for (scales_first, each, total) in [(true, 4, 12), (false, 3, 9)] {
        let mut scenario = self::scenario();
        let id = dralnu(&mut scenario, P0);
        let scales = scenario
            .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES)
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON)
            .id();
        lands(&mut scenario, P0, 2);
        let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
        let slash = sorcery(&mut scenario, "Flame Slash", FLAME_SLASH);
        let mut runner = scenario.build();
        cast(&mut runner, faith, vec![object_target(id)]);
        let mut events = cast(&mut runner, slash, vec![object_target(id)]);
        choose_shield(&mut runner, &mut events);

        let first = if scales_first { scales } else { season };
        let seen = answer_counter_orders(&mut runner, &mut events, id, first);
        assert_eq!(seen, vec![0, each, 2 * each]);
        assert_eq!(plus_counters(&runner, id), total);
        assert_eq!(zone_prompt(&runner), (1, 5));
        answer(
            &mut runner,
            GameAction::SelectCards {
                cards: vec![scales],
            },
            &mut events,
        );

        assert_eq!(zone(&runner, scales), Zone::Graveyard);
        assert_eq!(zone(&runner, id), Zone::Battlefield);
        assert_eq!(plus_counters(&runner, id), total);
        assert_eq!(damage(&runner, id), 0);
        assert_at_rest(&runner);
    }

    // Phytohydra: "put that many +1/+1 counters on it instead" is one more ordered counter
    // event, of 1, after Test of Faith's counters. strict-failure: Test of Faith's three counters
    // are three ordered events (Command Zone #174), so 12 + 4; the card puts them in one event,
    // (3+1)×2 = 8, then 4: 12, with two orders.
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
    // next instruction, after the counters and the queued sacrifice (CR 608.2c).
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
    let mut lives = Vec::new();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        lives.push((plus_counters(&runner, id), life(&runner, P0)));
        choose_source(&mut runner, &mut events, scales);
    }
    // strict-failure: three counter events (Command Zone #174); the card puts one, for 8.
    assert_eq!(lives, vec![(0, 20), (4, 20), (8, 20)]);
    assert_eq!(zone_prompt(&runner), (1, 7));
    assert_eq!(life(&runner, P0), 20);
    answer(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![scales],
        },
        &mut events,
    );
    assert_eq!(life(&runner, P0), 24);
    assert_eq!(plus_counters(&runner, id), 12);
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
        // The order between Dralnu's substitution and the Bear's counters is not asserted: by
        // CR 614.6 + CR 615.5 the substitution comes first, which the engine doesn't yet do in
        // every creation order (Command Zone #172).
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

const MASTER_OF_THE_WILD_HUNT: &str = "At the beginning of your upkeep, create a 2/2 green Wolf creature token.\n{T}: Tap all untapped Wolf creatures you control. Each Wolf tapped this way deals damage equal to its power to target creature. That creature deals damage equal to its power divided as its controller chooses among any number of those Wolves.";

/// Master of the Wild Hunt: "That creature deals damage equal to its power divided as its
/// controller chooses among any number of those Wolves." The division is chosen while the
/// ability resolves (CR 608.2d), and the chosen portions are one batch. Wolf A and Wolf B are
/// each under their own Test of Faith, with Hardened Scales and Doubling Season: Wolf A's
/// counters ask an order (CR 616.1) and leave the prompt standing while the division goes on to
/// Wolf B and Wolf C; Wolf B's counters wait behind Wolf A's (CR 608.2c, CR 615.5). One
/// prevented damage is one counter event either way, so each Wolf gets (1 + 1) × 2 = 4.
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
        damage(&runner, c),
        1,
        "the division went on past the prompt"
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
fn temper(scenario: &mut GameScenario) -> ObjectId {
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

fn cast_temper_at_one(runner: &mut GameRunner, temper: ObjectId, target: ObjectId) {
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
/// and Pyroclasm; CR 615.5). In combat the batch's drain takes the same path.
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

    let mut scenario = self::scenario();
    let hydra = phytohydra(&mut scenario, P1);
    let attacker = scenario.add_creature(P0, "Ogre", 4, 4).id();
    let faith = instant(&mut scenario, "Test of Faith", TEST_OF_FAITH);
    let mut runner = scenario.build();
    cast(&mut runner, faith, vec![object_target(hydra)]);
    run_combat(&mut runner, vec![attacker], vec![(hydra, attacker)]);

    assert_eq!(plus_counters(&runner, hydra), 4);
    assert_eq!(damage(&runner, hydra), 0);
    assert!(!runner.state().has_post_replacement_drain());
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

/// Force Bubble under Furnace of Rath in combat takes one legal order (CR 510.2, CR 616.1),
/// under ordinary and unpreventable damage alike.
#[test]
fn combat_order_park_keeps_force_bubbles_damage() {
    let mut scenario = scenario();
    let bubble = force_bubble(&mut scenario, P1);
    furnace(&mut scenario);
    let attacker = scenario.add_creature(P0, "Bear", 1, 1).id();
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![]);

    assert_eq!(life(&runner, P1), 20);
    let on_bubble = counters(&runner, bubble, &depletion());
    assert!([1, 2].contains(&on_bubble), "got {on_bubble}");

    // A 3/3 after Stomp: Bubble first is 3, Furnace first is 6 and it is sacrificed.
    let mut scenario = self::scenario();
    let bubble = force_bubble(&mut scenario, P1);
    furnace(&mut scenario);
    let attacker = scenario.add_creature(P0, "Ogre", 3, 3).id();
    let spell = stomp(&mut scenario);
    let mut runner = scenario.build();
    cast(&mut runner, spell, vec![player_target(P0)]);
    run_combat(&mut runner, vec![attacker], vec![]);

    assert_eq!(life(&runner, P1), 20);
    assert!(
        zone(&runner, bubble) == Zone::Graveyard || counters(&runner, bubble, &depletion()) == 3,
        "Force Bubble: {:?}",
        runner.state().objects[&bubble].counters
    );
}

#[test]
fn combat_order_park_keeps_phytohydras_damage() {
    let mut scenario = scenario();
    let hydra = phytohydra(&mut scenario, P1);
    furnace(&mut scenario);
    let attacker = scenario.add_creature(P0, "Ogre", 3, 3).id();
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![(hydra, attacker)]);

    let on_hydra = plus_counters(&runner, hydra);
    assert!([3, 6].contains(&on_hydra), "got {on_hydra}");
    assert_eq!(damage(&runner, hydra), 0);
}

/// Polukranos, Unchained: "prevent that damage and remove that many +1/+1 counters from it."
/// Furnace of Rath first removes 4, Polukranos first removes 2.
#[test]
fn combat_order_park_runs_a_prevention_rider_before_the_batch_drain() {
    let mut scenario = scenario();
    let id = scenario
        .add_creature_from_oracle(P1, "Polukranos, Unchained", 0, 0, POLUKRANOS)
        .id();
    scenario.with_counter(id, CounterType::Plus1Plus1, 6);
    furnace(&mut scenario);
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    let events = combat_events(&mut runner, vec![attacker], vec![(id, attacker)]);

    let left = plus_counters(&runner, id);
    assert!([4, 2].contains(&left), "got {left}");
    assert_eq!(damage(&runner, id), 0);
    assert_eq!(prevented(&events), 1);
}

/// Dapper Shieldmate: "If a permanent with a shield counter is dealt unpreventable damage, that
/// damage will be dealt and a shield counter will still be removed." and "If a permanent that
/// would be dealt damage has more than one shield counter on it, that damage is prevented and
/// only one shield counter is removed." (CR 122.1c.)
#[test]
fn combat_order_park_keeps_a_shield_counter_replacement() {
    for unpreventable in [false, true] {
        let mut scenario = scenario();
        let id = scenario
            .add_creature_from_oracle(P1, "Dapper Shieldmate", 2, 6, SHIELDMATE)
            .id();
        scenario.with_counter(id, CounterType::Shield, 2);
        furnace(&mut scenario);
        let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
        let spell = unpreventable.then(|| stomp(&mut scenario));
        let mut runner = scenario.build();
        if let Some(spell) = spell {
            cast(&mut runner, spell, vec![player_target(P1)]);
        }
        run_combat(&mut runner, vec![attacker], vec![(id, attacker)]);

        assert_eq!(counters(&runner, id, &CounterType::Shield), 1);
        assert_eq!(damage(&runner, id), if unpreventable { 4 } else { 0 });
    }
}

/// Torbran, Thane of Red Fell's ruling: "the player being dealt damage or the controller of the
/// permanent being dealt damage chooses an order in which to apply those effects." (2+2)×2 or
/// 2×2+2.
#[test]
fn combat_order_park_keeps_a_damage_modifiers_damage() {
    let mut scenario = scenario();
    scenario.add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN);
    furnace(&mut scenario);
    let attacker = scenario
        .add_creature(P0, "Goblin", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![]);

    let lost = 20 - life(&runner, P1);
    assert!([6, 8].contains(&lost), "got {lost}");
}

/// CR 616.1f: "Once the chosen effect has been applied, this process is repeated". Torbran,
/// Furnace of Rath and Gratuitous Violence park again after the first answer: (2×2×2)+2,
/// ((2×2)+2)×2 or ((2+2)×2)×2.
#[test]
fn combat_order_parks_repeat_until_the_event_settles() {
    let mut scenario = scenario();
    scenario.add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN);
    furnace(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Gratuitous Violence", GRATUITOUS_VIOLENCE);
    let attacker = scenario
        .add_creature(P0, "Goblin", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![]);

    let lost = 20 - life(&runner, P1);
    assert!([10, 12, 16].contains(&lost), "got {lost}");

    // Force Bubble in Gratuitous Violence's place: whichever comes first, it is reached.
    let mut scenario = self::scenario();
    scenario.add_creature_from_oracle(P0, "Torbran, Thane of Red Fell", 2, 4, TORBRAN);
    furnace(&mut scenario);
    let bubble = force_bubble(&mut scenario, P1);
    let attacker = scenario
        .add_creature(P0, "Goblin", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![]);

    assert_eq!(life(&runner, P1), 20);
    assert!(
        zone(&runner, bubble) == Zone::Graveyard || counters(&runner, bubble, &depletion()) == 2,
        "Force Bubble: {:?}",
        runner.state().objects[&bubble].counters
    );
}

/// Two 1/1 attackers, each blocked by its own Phytohydra, under Furnace of Rath. Each answered
/// effect's rider drains in its own portion, ahead of the next portion's stamp.
#[test]
fn combat_order_answer_drains_in_its_own_portion() {
    let mut scenario = scenario();
    let (first, second) = (phytohydra(&mut scenario, P1), phytohydra(&mut scenario, P1));
    furnace(&mut scenario);
    let (one, two) = (
        scenario.add_creature(P0, "Squire", 1, 1).id(),
        scenario.add_creature(P0, "Squire", 1, 1).id(),
    );
    let mut runner = scenario.build();
    run_combat(
        &mut runner,
        vec![one, two],
        vec![(first, one), (second, two)],
    );

    assert!(plus_counters(&runner, first) >= 1);
    assert!(plus_counters(&runner, second) >= 1);

    // Polukranos's rider has no substitution behind it to drain the first portion's rider, so
    // a rider left resident past its own portion costs Polukranos its "remove that many +1/+1
    // counters from it".
    let mut scenario = self::scenario();
    let hydra = phytohydra(&mut scenario, P1);
    let polukranos = scenario
        .add_creature_from_oracle(P1, "Polukranos, Unchained", 0, 0, POLUKRANOS)
        .id();
    scenario.with_counter(polukranos, CounterType::Plus1Plus1, 6);
    furnace(&mut scenario);
    let (one, two) = (
        scenario.add_creature(P0, "Squire", 1, 1).id(),
        scenario.add_creature(P0, "Bear", 2, 2).id(),
    );
    let mut runner = scenario.build();
    run_combat(
        &mut runner,
        vec![one, two],
        vec![(hydra, one), (polukranos, two)],
    );

    assert!(plus_counters(&runner, hydra) >= 1);
    let left = plus_counters(&runner, polukranos);
    assert!([4, 2].contains(&left), "got {left}");
    assert!(!runner.state().has_post_replacement_drain());
}

/// Force Bubble: "Note that damage from multiple sources that happens at one time (such as combat
/// damage) gives one choice, not one per source."
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
    run_combat(&mut runner, attackers, vec![]);

    let mut on_bubbles = [
        counters(&runner, first, &depletion()),
        counters(&runner, second, &depletion()),
    ];
    on_bubbles.sort_unstable();
    assert_eq!(on_bubbles, [0, 3]);
    assert_eq!(life(&runner, P1), 20);
}

/// Force Bubble: "You can't split the damage between them." Answering every prompt with one
/// Bubble is the legal path. The prompt count is deliberately unasserted: the ruling wants one,
/// and the engine offers one per source (Command Zone #140).
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
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        choose_source(&mut runner, &mut events, first);
    }

    assert_eq!(counters(&runner, first, &depletion()), 3);
    assert_eq!(counters(&runner, second, &depletion()), 0);
    assert_eq!(life(&runner, P1), 20);
    assert_at_rest(&runner);
}

/// Battletide Alchemist: "you may prevent X of that damage". A "may" is not an order, so combat
/// gives it no automatic yes. Its life total is deliberately unasserted: the card wants a prompt
/// combat can't offer yet (Command Zone #140).
#[test]
fn a_parked_may_gets_no_automatic_yes_in_combat() {
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

    let mut scenario = self::scenario();
    scenario
        .add_creature_from_oracle(P1, "Battletide Alchemist", 3, 4, ALCHEMIST)
        .with_subtypes(vec!["Kithkin", "Cleric"]);
    let attacker = scenario.add_creature(P0, "Ogre", 3, 3).id();
    let mut runner = scenario.build();
    let events = combat_events(&mut runner, vec![attacker], vec![]);

    assert!(matches!(
        runner.state().phase,
        Phase::EndCombat | Phase::PostCombatMain
    ));
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(prevented(&events), 0);
}
