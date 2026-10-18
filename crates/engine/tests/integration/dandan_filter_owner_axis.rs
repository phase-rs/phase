//! Dandân shared pile: a filter that names the graveyard or library is
//! satisfied for every seat that reads the pile, and a filter that does not
//! name it keeps its single player-axis comparison.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

pub(super) fn plenty_of_mana() -> Vec<ManaUnit> {
    [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ]
    .into_iter()
    .flat_map(|color| (0..6).map(move |_| ManaUnit::new(color, ObjectId(0), false, vec![])))
    .collect()
}

pub(super) fn scenario(format: FormatConfig) -> GameScenario {
    let mut scenario = GameScenario::new_with_format(format, 2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

pub(super) fn stage(
    scenario: &mut GameScenario,
    db: &CardDatabase,
    zone: Zone,
    cards: &[(PlayerId, &str)],
) -> Vec<ObjectId> {
    cards
        .iter()
        .map(|&(owner, name)| scenario.add_real_card(owner, name, zone, db))
        .collect()
}

/// Build the runner with `actor` holding priority in their precombat main phase.
pub(super) fn start(mut scenario: GameScenario, actor: PlayerId) -> GameRunner {
    scenario.with_mana_pool(actor, plenty_of_mana());
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = actor;
    state.priority_player = actor;
    state.waiting_for = WaitingFor::Priority { player: actor };
    runner
}

pub(super) fn dandan() -> FormatConfig {
    FormatConfig::dandan()
}

fn gy(runner: &GameRunner, seat: PlayerId) -> Vec<ObjectId> {
    runner.state().graveyard_of(seat).iter().copied().collect()
}

/// Pass priority until the ETB trigger's targets are known: the legal targets of an open prompt,
/// or the lone legal target the engine chose automatically.
fn pass_to_trigger_targets(runner: &mut GameRunner) -> Vec<TargetRef> {
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => return target_slots[selection.current_slot].legal_targets.clone(),
            _ => {
                if let Some(entry) = runner.state().stack.last() {
                    if matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }) {
                        let ability = entry.ability().expect("a triggered ability");
                        return ability.targets.clone();
                    }
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    panic!("the trigger never reached the stack");
}

fn sacrifice_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| format!("{:?}", ability.cost).contains("Sacrifice"))
        .expect("a sacrifice ability")
}

// ---------------------------------------------------------------------------
// V1: FilterProp::Owned (Haunted Fengraf)
// ---------------------------------------------------------------------------

/// P1 activates Haunted Fengraf over a graveyard holding `victims`; returns the runner,
/// the Fengraf and the staged victims.
fn fengraf_run(
    format: FormatConfig,
    victims: &[(PlayerId, &str)],
    controller_override: Option<PlayerId>,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    let fengraf = sc.add_real_card(P1, "Haunted Fengraf", Zone::Battlefield, db);
    let cards = stage(&mut sc, db, Zone::Graveyard, victims);
    let mut runner = start(sc, P1);
    runner.state_mut().objects.get_mut(&fengraf).unwrap().tapped = false;
    if let Some(controller) = controller_override {
        for id in &cards {
            runner.state_mut().objects.get_mut(id).unwrap().controller = controller;
        }
    }
    let index = sacrifice_ability_index(&runner, fengraf);
    runner.activate(fengraf, index).resolve();
    (runner, fengraf, cards)
}

#[test]
fn v1_fengraf_returns_the_other_seats_creature_from_the_shared_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    // Reach: the cost was paid, so Fengraf itself sits in the pile.
    let (runner, fengraf, cards) = fengraf_run(dandan(), &[(P0, "Grizzly Bears")], None);
    assert_eq!(runner.state().objects[&fengraf].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&cards[0]].zone,
        Zone::Hand,
        "a P0-owned creature in the shared graveyard satisfies P1's 'your graveyard'"
    );

    // Paired: a P1-owned creature leaves the pile as well.
    let (runner, _, cards) = fengraf_run(dandan(), &[(P1, "Grizzly Bears")], None);
    assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Hand);

    // Hostile: a pile card whose stored controller is P1 is still returned.
    let (runner, _, cards) = fengraf_run(dandan(), &[(P0, "Grizzly Bears")], Some(P1));
    assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Hand);
}

#[test]
fn v1_fengraf_in_standard_returns_only_the_activators_own_graveyard_card() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, fengraf, cards) = fengraf_run(
        FormatConfig::standard(),
        &[(P0, "Grizzly Bears"), (P1, "Grizzly Bears")],
        None,
    );
    assert_eq!(runner.state().objects[&fengraf].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&cards[1]].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&cards[0]].zone,
        Zone::Graveyard,
        "the other seat's own graveyard card stays"
    );
    assert!(gy(&runner, P0).contains(&cards[0]));
}

// ---------------------------------------------------------------------------
// V2: typed-filter controller axis (Mystic Sanctuary)
// ---------------------------------------------------------------------------

/// P1 plays Mystic Sanctuary (enters untapped) over a graveyard holding a P0-owned instant and
/// creature plus a P1-owned instant; returns the runner, staged cards and the trigger's targets.
fn sanctuary_run(format: FormatConfig) -> (GameRunner, Vec<ObjectId>, Vec<TargetRef>) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    for _ in 0..3 {
        sc.add_real_card(P1, "Island", Zone::Battlefield, db);
    }
    let sanctuary = sc.add_real_card(P1, "Mystic Sanctuary", Zone::Hand, db);
    let cards = stage(
        &mut sc,
        db,
        Zone::Graveyard,
        &[(P0, "Lightning Bolt"), (P0, "Grizzly Bears"), (P1, "Shock")],
    );
    let mut runner = start(sc, P1);
    let card_id = runner.state().objects[&sanctuary].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: sanctuary,
            card_id,
        })
        .expect("Mystic Sanctuary is playable");
    assert!(
        !runner.state().objects[&sanctuary].tapped,
        "reach: three other Islands keep it untapped, so the trigger is on the stack"
    );
    let targets = pass_to_trigger_targets(&mut runner);
    (runner, cards, targets)
}

#[test]
fn v2_mystic_sanctuary_targets_the_other_seats_instant_in_the_shared_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, cards, targets) = sanctuary_run(dandan());
    let (bolt, bears, shock) = (cards[0], cards[1], cards[2]);
    assert!(targets.contains(&TargetRef::Object(shock)), "paired: own");
    assert!(targets.contains(&TargetRef::Object(bolt)), "other seat's");
    assert!(
        !targets.contains(&TargetRef::Object(bears)),
        "a creature is neither an instant nor a sorcery"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bolt)),
        })
        .expect("target chosen");
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the may");
            }
            _ => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("resolve");
            }
        }
    }
    assert_eq!(
        runner.state().library_of(P0).front().copied(),
        Some(bolt),
        "the instant sits on top of the shared library"
    );
}

#[test]
fn v2_mystic_sanctuary_in_standard_cannot_reach_the_other_seats_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (_, cards, targets) = sanctuary_run(FormatConfig::standard());
    assert!(targets.contains(&TargetRef::Object(cards[2])), "own: reach");
    assert!(!targets.contains(&TargetRef::Object(cards[0])));
}

// ---------------------------------------------------------------------------
// V3: Owned{Opponent} (Disposal Mummy)
// ---------------------------------------------------------------------------

fn mummy_targets(format: FormatConfig) -> (Vec<ObjectId>, Vec<TargetRef>) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    let mummy = sc.add_real_card(P1, "Disposal Mummy", Zone::Hand, db);
    let cards = stage(
        &mut sc,
        db,
        Zone::Graveyard,
        &[(P0, "Grizzly Bears"), (P1, "Shock")],
    );
    let mut runner = start(sc, P1);
    runner.cast(mummy).commit();
    let targets = pass_to_trigger_targets(&mut runner);
    (cards, targets)
}

#[test]
fn v3_disposal_mummy_treats_the_shared_graveyard_as_an_opponents() {
    if shared_card_db().is_none() {
        return;
    }
    let (cards, targets) = mummy_targets(dandan());
    assert!(targets.contains(&TargetRef::Object(cards[0])), "reach");
    assert!(
        targets.contains(&TargetRef::Object(cards[1])),
        "the pile is an opponent's graveyard for every seat"
    );
}

#[test]
fn v3_disposal_mummy_in_standard_excludes_the_casters_own_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (cards, targets) = mummy_targets(FormatConfig::standard());
    assert!(targets.contains(&TargetRef::Object(cards[0])), "reach");
    assert!(!targets.contains(&TargetRef::Object(cards[1])));
}

// ---------------------------------------------------------------------------
// V14 / V16: observers whose filter does not name the graveyard keep one comparison
// ---------------------------------------------------------------------------

/// P0 controls `observer`; `sacrificer` controls Viscera Seer and a Grizzly Bears and sacrifices
/// the Bears, which lands in the (shared) graveyard.
fn sacrifice_run(
    format: FormatConfig,
    observer: &str,
    sacrificer: PlayerId,
) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    let watcher = sc.add_real_card(P0, observer, Zone::Battlefield, db);
    let seer = sc.add_real_card(sacrificer, "Viscera Seer", Zone::Battlefield, db);
    let victim = sc.add_real_card(sacrificer, "Grizzly Bears", Zone::Battlefield, db);
    stage(
        &mut sc,
        db,
        Zone::Library,
        &[(P0, "Island"), (P0, "Island")],
    );
    let mut runner = start(sc, sacrificer);
    runner
        .act(GameAction::ActivateAbility {
            source_id: seer,
            ability_index: 0,
        })
        .expect("Viscera Seer activates");
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("sacrifice the Bears");
            }
            WaitingFor::ScryChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("scry");
            }
            _ if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("resolve");
            }
            _ => break,
        }
    }
    assert!(
        gy(&runner, P0).contains(&victim),
        "reach: the sacrificed card is in the shared pile"
    );
    (runner, watcher)
}

fn counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn v14_a_sacrifice_observer_ignores_the_other_seats_sacrifice_into_the_shared_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, watcher) = sacrifice_run(dandan(), "Bloodbriar", P0);
    assert_eq!(
        counters(&runner, watcher),
        1,
        "reach: its own sacrifice counts"
    );

    let (runner, watcher) = sacrifice_run(dandan(), "Bloodbriar", P1);
    assert_eq!(
        counters(&runner, watcher),
        0,
        "'whenever you sacrifice' never sees an opponent's sacrifice"
    );
}

#[test]
fn v16_a_dies_observer_ignores_the_other_seats_creature_in_the_shared_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, _) = sacrifice_run(dandan(), "Zulaport Cutthroat", P0);
    assert_eq!(runner.life(P1), 19, "reach: its own creature dying drains");
    assert_eq!(runner.life(P0), 21);

    let (runner, _) = sacrifice_run(dandan(), "Zulaport Cutthroat", P1);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P0), 20);
}

// ---------------------------------------------------------------------------
// V17: zone named by the effect's origin, not the filter (Release to Memory)
// ---------------------------------------------------------------------------

/// P0 casts Release to Memory over a graveyard holding a P0-owned and a P1-owned creature;
/// returns the runner and the staged `(mine, theirs)`.
fn release_to_memory_run(format: FormatConfig) -> (GameRunner, ObjectId, ObjectId) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    let release = sc.add_real_card(P0, "Release to Memory", Zone::Hand, db);
    let cards = stage(
        &mut sc,
        db,
        Zone::Graveyard,
        &[(P0, "Grizzly Bears"), (P1, "Grizzly Bears")],
    );
    let mut runner = start(sc, P0);
    runner.cast(release).resolve();
    (runner, cards[0], cards[1])
}

#[test]
fn v17_exile_all_opponents_graveyards_takes_the_whole_shared_pile() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, mine, theirs) = release_to_memory_run(dandan());
    assert_eq!(
        runner.state().objects[&theirs].zone,
        Zone::Exile,
        "reach: the opponent-owned pile card is exiled"
    );
    assert_eq!(
        runner.state().objects[&mine].zone,
        Zone::Exile,
        "every pile card is in an opponent's graveyard"
    );
}

#[test]
fn v17_exile_all_opponents_graveyards_in_standard_leaves_the_casters_own() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, mine, theirs) = release_to_memory_run(FormatConfig::standard());
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Exile, "reach");
    assert_eq!(runner.state().objects[&mine].zone, Zone::Graveyard);
}

/// P0's Cogwork Progenitor reaches its end-step "artifact card in your graveyard" choice over a
/// graveyard holding two P0-owned artifacts (one would be chosen without a prompt) and a
/// P1-owned one; returns the offered cards and the staged `(mine, theirs)`.
fn cogwork_offer(format: FormatConfig) -> (Vec<ObjectId>, ObjectId, ObjectId) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    sc.add_real_card(P0, "Cogwork Progenitor", Zone::Battlefield, db);
    let cards = stage(
        &mut sc,
        db,
        Zone::Graveyard,
        &[(P0, "Mind Stone"), (P0, "Sol Ring"), (P1, "Arcane Signet")],
    );
    let mut runner = start(sc, P0);
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the may");
            }
            WaitingFor::EffectZoneChoice { cards: offered, .. } => {
                return (offered, cards[0], cards[2]);
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    panic!("the end-step choice never surfaced");
}

#[test]
fn v17_an_artifact_card_in_your_graveyard_offers_the_whole_shared_pile() {
    if shared_card_db().is_none() {
        return;
    }
    let (offered, mine, theirs) = cogwork_offer(dandan());
    assert!(offered.contains(&mine), "reach: P0's own artifact");
    assert!(offered.contains(&theirs), "the pile is P0's graveyard");
}

#[test]
fn v17_an_artifact_card_in_your_graveyard_in_standard_offers_only_your_own() {
    if shared_card_db().is_none() {
        return;
    }
    let (offered, mine, theirs) = cogwork_offer(FormatConfig::standard());
    assert!(offered.contains(&mine), "reach");
    assert!(!offered.contains(&theirs));
}

// ---------------------------------------------------------------------------
// V18: zone named by the quantity's `from`, not the filter (Relic Retriever)
// ---------------------------------------------------------------------------

/// P0 controls `watcher` and casts `spell` over a graveyard holding one Grizzly Bears owned by
/// `pile_owner`, then plays to the end step; returns the runner and the Bears. `targeted`
/// spells are aimed at the Bears.
fn pile_run(
    format: FormatConfig,
    watcher: &str,
    spell: &str,
    targeted: bool,
    pile_owner: PlayerId,
) -> (GameRunner, ObjectId) {
    pile_run_by(format, watcher, spell, targeted, pile_owner, P0)
}

/// `pile_run` with the spell held, and the turn played, by `caster`.
fn pile_run_by(
    format: FormatConfig,
    watcher: &str,
    spell: &str,
    targeted: bool,
    pile_owner: PlayerId,
    caster: PlayerId,
) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    sc.add_real_card(P0, watcher, Zone::Battlefield, db);
    let spell = sc.add_real_card(caster, spell, Zone::Hand, db);
    let bears = stage(
        &mut sc,
        db,
        Zone::Graveyard,
        &[(pile_owner, "Grizzly Bears")],
    )[0];
    let mut runner = start(sc, caster);
    if targeted {
        runner.cast(spell).target_object(bears).resolve();
    } else {
        runner.cast(spell).resolve();
    }
    play_to_end_step(&mut runner);
    (runner, bears)
}

fn play_to_end_step(runner: &mut GameRunner) {
    for _ in 0..60 {
        let state = runner.state();
        if state.phase == Phase::End && state.stack.is_empty() {
            break;
        }
        match state.waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
}

fn battlefield_named(runner: &GameRunner, name: &str) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name == name)
        .count()
}

fn retriever_run(
    format: FormatConfig,
    spell: &str,
    targeted: bool,
    pile_owner: PlayerId,
) -> (GameRunner, ObjectId, usize) {
    let (runner, bears) = pile_run(format, "Relic Retriever", spell, targeted, pile_owner);
    let treasures = battlefield_named(&runner, "Treasure");
    (runner, bears, treasures)
}

#[test]
fn v18_a_card_leaving_the_shared_graveyard_left_your_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, bears, treasures) = retriever_run(dandan(), "Regrowth", true, P1);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Hand,
        "reach: the other seat's card left the pile"
    );
    assert_eq!(
        treasures, 1,
        "it left every seat's graveyard, P0's included"
    );

    let (_, _, treasures) = retriever_run(dandan(), "Regrowth", true, P0);
    assert_eq!(treasures, 1, "paired: P0's own card");
}

#[test]
fn v18_in_standard_only_your_own_graveyard_counts() {
    if shared_card_db().is_none() {
        return;
    }
    let (_, _, treasures) = retriever_run(FormatConfig::standard(), "Regrowth", true, P0);
    assert_eq!(treasures, 1, "reach: the trigger fires for P0's own card");

    let (runner, bears, treasures) =
        retriever_run(FormatConfig::standard(), "Release to Memory", false, P1);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Exile,
        "reach: the opponent's card left the opponent's graveyard"
    );
    assert_eq!(treasures, 0);
}

// ---------------------------------------------------------------------------
// V19: trigger whose zone is the trigger's origin, not the filter (Chalk Outline)
// ---------------------------------------------------------------------------

fn detectives(format: FormatConfig, spell: &str, targeted: bool, pile_owner: PlayerId) -> usize {
    let (runner, bears) = pile_run(format, "Chalk Outline", spell, targeted, pile_owner);
    assert_eq!(
        runner.state().objects[&bears].zone,
        match spell {
            "Regrowth" => Zone::Hand,
            "Reanimate" => Zone::Battlefield,
            _ => Zone::Exile,
        },
        "reach: the pile card left the graveyard"
    );
    battlefield_named(&runner, "Detective")
}

#[test]
fn v19_a_creature_card_leaving_the_shared_graveyard_left_your_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        detectives(dandan(), "Regrowth", true, P0),
        1,
        "paired: P0's own card"
    );
    assert_eq!(
        detectives(dandan(), "Regrowth", true, P1),
        1,
        "the pile is every seat's graveyard, P0's included"
    );
}

#[test]
fn v19_in_standard_only_your_own_graveyard_triggers() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        detectives(FormatConfig::standard(), "Regrowth", true, P0),
        1,
        "reach: the trigger fires for P0's own card"
    );
    assert_eq!(
        detectives(FormatConfig::standard(), "Release to Memory", false, P1),
        0
    );
}

#[test]
fn v19_a_card_reanimated_from_the_shared_graveyard_left_your_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        detectives(dandan(), "Reanimate", true, P0),
        1,
        "paired: P0's own card"
    );
    assert_eq!(
        detectives(dandan(), "Reanimate", true, P1),
        1,
        "the pile is every seat's graveyard, P0's included"
    );
}

#[test]
fn v19_in_standard_only_your_own_graveyard_triggers_on_reanimation() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        detectives(FormatConfig::standard(), "Reanimate", true, P0),
        1,
        "reach: the trigger fires for P0's own card"
    );
    assert_eq!(
        detectives(FormatConfig::standard(), "Reanimate", true, P1),
        0
    );
}

// ---------------------------------------------------------------------------
// V20: the controller axis of a record read through the trigger's origin
// ---------------------------------------------------------------------------

fn goblin_armies(format: FormatConfig, spell: &str, targeted: bool, pile_owner: PlayerId) -> usize {
    let (runner, bears) = pile_run(format, "Along the Crooked Way", spell, targeted, pile_owner);
    assert_ne!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "reach: the pile card left the graveyard"
    );
    battlefield_named(&runner, "Goblin Army")
}

#[test]
fn v20_a_controller_form_origin_trigger_reads_the_whole_shared_pile() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        goblin_armies(dandan(), "Regrowth", true, P0),
        1,
        "paired: P0's own card"
    );
    assert_eq!(
        goblin_armies(dandan(), "Regrowth", true, P1),
        1,
        "the pile is every seat's graveyard, P0's included"
    );
}

#[test]
fn v20_in_standard_only_your_own_graveyard_triggers_the_controller_form() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        goblin_armies(FormatConfig::standard(), "Regrowth", true, P0),
        1,
        "reach: the trigger fires for P0's own card"
    );
    assert_eq!(
        goblin_armies(FormatConfig::standard(), "Release to Memory", false, P1),
        0
    );
}

/// P0 watches with Along the Crooked Way while `caster` reanimates a pile Bears owned by
/// `pile_owner`.
fn goblin_armies_by_caster(format: FormatConfig, pile_owner: PlayerId, caster: PlayerId) -> usize {
    let (runner, bears) = pile_run_by(
        format,
        "Along the Crooked Way",
        "Reanimate",
        true,
        pile_owner,
        caster,
    );
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "reach: the pile card was reanimated"
    );
    battlefield_named(&runner, "Goblin Army")
}

#[test]
fn v20_a_reanimation_by_either_seat_leaves_the_shared_graveyard_of_the_watcher() {
    if shared_card_db().is_none() {
        return;
    }
    for pile_owner in [P0, P1] {
        assert_eq!(
            goblin_armies_by_caster(dandan(), pile_owner, P1),
            1,
            "P1 reanimates a card owned by {pile_owner:?}: it left P0's graveyard, the pile"
        );
    }
    assert_eq!(
        goblin_armies_by_caster(dandan(), P0, P0),
        1,
        "paired: the watcher reanimates its own card"
    );
}

/// P0 controls `watcher` and casts `spell` aimed at `target` over a library of four
/// `library_card`s owned by `pile_owner`.
fn mill_run(
    format: FormatConfig,
    watcher: &str,
    library_card: &str,
    pile_owner: PlayerId,
    target: PlayerId,
) -> GameRunner {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    sc.add_real_card(P0, watcher, Zone::Battlefield, db);
    let spell = sc.add_real_card(P0, "Thought Scour", Zone::Hand, db);
    stage(&mut sc, db, Zone::Library, &[(pile_owner, library_card); 4]);
    let mut runner = start(sc, P0);
    runner.cast(spell).target_player(target).resolve();
    runner
}

fn in_graveyard(runner: &GameRunner, name: &str) -> usize {
    let state = runner.state();
    state
        .objects
        .values()
        .filter(|object| object.zone == Zone::Graveyard && object.name == name)
        .count()
}

fn grave_reaver_bears(format: FormatConfig, pile_owner: PlayerId, target: PlayerId) -> usize {
    let runner = mill_run(
        format,
        "Colossal Grave-Reaver",
        "Grizzly Bears",
        pile_owner,
        target,
    );
    let on_battlefield = battlefield_named(&runner, "Grizzly Bears");
    assert_eq!(
        in_graveyard(&runner, "Grizzly Bears") + on_battlefield,
        2,
        "reach: two cards milled"
    );
    on_battlefield
}

#[test]
fn v20_a_library_origin_controller_form_reads_the_whole_shared_library() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        grave_reaver_bears(dandan(), P0, P0),
        1,
        "paired: P0's own cards"
    );
    assert_eq!(
        grave_reaver_bears(dandan(), P1, P0),
        1,
        "the library is every seat's, P0's included"
    );
}

#[test]
fn v20_in_standard_only_your_own_library_triggers_the_library_origin() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        grave_reaver_bears(FormatConfig::standard(), P0, P0),
        1,
        "reach: the trigger fires for P0's own library"
    );
    assert_eq!(grave_reaver_bears(FormatConfig::standard(), P1, P1), 0);
}

fn desert_warfare_triggers(format: FormatConfig, pile_owner: PlayerId, target: PlayerId) -> usize {
    let runner = mill_run(format, "Desert Warfare", "Arid Archway", pile_owner, target);
    assert_eq!(
        in_graveyard(&runner, "Arid Archway"),
        2,
        "reach: two Deserts milled"
    );
    runner.state().delayed_triggers.len()
}

#[test]
fn v20_a_one_of_origin_trigger_reads_the_whole_shared_library() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        desert_warfare_triggers(dandan(), P0, P0),
        2,
        "paired: P0's own Deserts"
    );
    assert_eq!(
        desert_warfare_triggers(dandan(), P1, P0),
        2,
        "each Desert put into the shared graveyard from the shared library is P0's"
    );
}

#[test]
fn v20_in_standard_only_your_own_library_triggers_the_one_of_origin() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        desert_warfare_triggers(FormatConfig::standard(), P0, P0),
        2,
        "reach: the trigger fires for P0's own Deserts"
    );
    assert_eq!(desert_warfare_triggers(FormatConfig::standard(), P1, P1), 0);
}

// ---------------------------------------------------------------------------
// V22: a live entrant with no named origin is the permanent (Impact Tremors)
// ---------------------------------------------------------------------------

/// P0 watches with Impact Tremors while `caster` reanimates a P0-owned pile Bears; returns the
/// life totals (P0, P1).
fn tremors_life(format: FormatConfig, caster: PlayerId) -> (i32, i32) {
    let (runner, bears) = pile_run_by(format, "Impact Tremors", "Reanimate", true, P0, caster);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "reach: the pile card was reanimated"
    );
    (runner.life(P0), runner.life(P1))
}

#[test]
fn v22_a_creature_you_control_entering_is_judged_as_the_permanent() {
    if shared_card_db().is_none() {
        return;
    }
    for format in [dandan(), FormatConfig::standard()] {
        assert_eq!(
            tremors_life(format.clone(), P0),
            (18, 19),
            "reach: P0's reanimated creature pings P1"
        );
        assert_eq!(
            tremors_life(format, P1),
            (20, 18),
            "P1's creature is not a creature P0 controls"
        );
    }
}

// ---------------------------------------------------------------------------
// Origin-less record door (Aetherworks Marvel) and entered-from-your-graveyard
// conditions (Archfiend's Vessel, Prized Amalgam, Grist, Voracious Larva)
// ---------------------------------------------------------------------------

fn energy(runner: &GameRunner, seat: PlayerId) -> u32 {
    runner.state().players[seat.0 as usize].energy
}

/// P0 watches with Aetherworks Marvel while `target` mills two of `library_owner`'s Bears.
fn marvel_energy_after_mill(
    format: FormatConfig,
    library_owner: PlayerId,
    target: PlayerId,
) -> u32 {
    let runner = mill_run(
        format,
        "Aetherworks Marvel",
        "Grizzly Bears",
        library_owner,
        target,
    );
    assert_eq!(
        in_graveyard(&runner, "Grizzly Bears"),
        2,
        "reach: two cards milled"
    );
    energy(&runner, P0)
}

/// P0 watches with Aetherworks Marvel and destroys its own Grizzly Bears.
fn marvel_energy_after_own_destroy(format: FormatConfig) -> u32 {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    sc.add_real_card(P0, "Aetherworks Marvel", Zone::Battlefield, db);
    let bears = sc.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let spell = sc.add_real_card(P0, "Doom Blade", Zone::Hand, db);
    let mut runner = start(sc, P0);
    runner.cast(spell).target_object(bears).resolve();
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "reach: the permanent died"
    );
    energy(&runner, P0)
}

#[test]
fn a_card_milled_from_a_library_is_not_a_permanent_you_control() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        marvel_energy_after_own_destroy(dandan()),
        1,
        "paired: P0's own permanent dies"
    );
    assert_eq!(
        marvel_energy_after_mill(dandan(), P1, P0),
        0,
        "the other seat's library cards are nobody's permanents"
    );
}

#[test]
fn in_standard_a_milled_card_is_not_a_permanent_you_control() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        marvel_energy_after_own_destroy(FormatConfig::standard()),
        1,
        "reach: the trigger fires for P0's own permanent"
    );
    assert_eq!(
        marvel_energy_after_mill(FormatConfig::standard(), P1, P1),
        0
    );
}

/// P0 reanimates `entrant` (owned by `entrant_owner`) from the pile, with `staged` cards placed
/// first; returns the runner after the turn is played to the end step.
fn reanimate_run(
    format: FormatConfig,
    staged: &[(PlayerId, &str, Zone)],
    entrant: &str,
    entrant_owner: PlayerId,
) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    for &(owner, name, zone) in staged {
        sc.add_real_card(owner, name, zone, db);
    }
    let spell = sc.add_real_card(P0, "Reanimate", Zone::Hand, db);
    let card = stage(&mut sc, db, Zone::Graveyard, &[(entrant_owner, entrant)])[0];
    let mut runner = start(sc, P0);
    runner
        .cast(spell)
        .target_object(card)
        .accept_optional()
        .resolve();
    assert_ne!(
        runner.state().objects[&card].zone,
        Zone::Graveyard,
        "reach: the pile card was reanimated"
    );
    play_to_end_step(&mut runner);
    (runner, card)
}

fn vessel_demons(format: FormatConfig, owner: PlayerId) -> usize {
    let (runner, _) = reanimate_run(format, &[], "Archfiend's Vessel", owner);
    battlefield_named(&runner, "Demon")
}

#[test]
fn an_entrant_from_the_shared_pile_entered_from_your_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(vessel_demons(dandan(), P0), 1, "paired: P0's own card");
    assert_eq!(
        vessel_demons(dandan(), P1),
        1,
        "the pile is P0's graveyard whoever owns the card"
    );
}

#[test]
fn in_standard_an_entrant_from_the_other_seats_graveyard_did_not_enter_from_yours() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        vessel_demons(FormatConfig::standard(), P0),
        1,
        "reach: P0's own graveyard card"
    );
    assert_eq!(vessel_demons(FormatConfig::standard(), P1), 0);
}

fn amalgam_return_triggers(format: FormatConfig, owner: PlayerId) -> usize {
    let (runner, _) = reanimate_run(
        format,
        &[(P0, "Prized Amalgam", Zone::Graveyard)],
        "Grizzly Bears",
        owner,
    );
    battlefield_named(&runner, "Prized Amalgam")
}

#[test]
fn a_creature_from_the_shared_pile_entered_from_your_graveyard_for_a_graveyard_watcher() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        amalgam_return_triggers(dandan(), P0),
        1,
        "paired: P0's own card"
    );
    assert_eq!(amalgam_return_triggers(dandan(), P1), 1);
}

#[test]
fn in_standard_a_creature_from_the_other_seats_graveyard_does_not_wake_the_watcher() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(
        amalgam_return_triggers(FormatConfig::standard(), P0),
        1,
        "reach: P0's own graveyard card"
    );
    assert_eq!(amalgam_return_triggers(FormatConfig::standard(), P1), 0);
}

fn grist_transforms(format: FormatConfig, owner: PlayerId) -> bool {
    let (runner, grist) = reanimate_run(format, &[], "Grist, Voracious Larva", owner);
    runner.state().objects[&grist].transformed
}

#[test]
fn a_creature_you_control_from_the_shared_pile_transforms_a_graveyard_grist() {
    if shared_card_db().is_none() {
        return;
    }
    assert!(grist_transforms(dandan(), P0), "paired: P0's own card");
    assert!(grist_transforms(dandan(), P1));
}

#[test]
fn in_standard_a_card_from_the_other_seats_graveyard_does_not_transform_grist() {
    if shared_card_db().is_none() {
        return;
    }
    assert!(
        grist_transforms(FormatConfig::standard(), P0),
        "reach: P0's own graveyard card"
    );
    assert!(!grist_transforms(FormatConfig::standard(), P1));
}
