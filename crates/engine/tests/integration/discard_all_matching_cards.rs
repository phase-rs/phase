//! CR 701.9a + CR 608.2c: "target player reveals their hand and discards all <filter>
//! cards" makes the declared player discard every card in THEIR hand that matches the
//! filter. The count is the census of those cards in the discarding player's hand, so
//! the discard is forced (no `DiscardChoice` prompt), and a hand with no matching card
//! discards nothing and resolves cleanly.
//!
//! Driving cards (verbatim Oracle text): **Amnesia** (nonland), **Trapfinder's Trick**
//! (the Trap spell type, CR 205.3k), **Shatter Assumptions** (colorless nonland and
//! multicolored, CR 105.2c / CR 105.2b), **Cabal Therapy** and **Cabal Therapist**
//! (the name chosen earlier in the resolution, CR 201.2a; Cabal Therapist's reflexive
//! "When you do", CR 603.12) and **Tsabo's Decree** (the creature type chosen earlier,
//! CR 205.3m, with changeling, CR 702.73a). **Nicol Bolas** pins the same count
//! binding for an existing whole-hand discard by a triggering player.
//!
//! Every board gives the caster (and, where present, a third player) cards that match
//! the filter, so a count or pool read against the wrong player's hand either prompts
//! for a choice (the scenario driver stops at an unanswered `DiscardChoice`, which
//! `assert_resolved` rejects) or moves the wrong cards.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const AMNESIA: &str = "Target player reveals their hand and discards all nonland cards.";

const TRAPFINDERS_TRICK: &str = "Target player reveals their hand and discards all Trap cards.";

const SHATTER_ASSUMPTIONS: &str = "Choose one —\n• Target opponent reveals their hand and \
discards all colorless nonland cards.\n• Target opponent reveals their hand and discards all \
multicolored cards.";

const CABAL_THERAPY: &str = "Choose a nonland card name. Target player reveals their hand and \
discards all cards with that name.\nFlashback—Sacrifice a creature. (You may cast this card from \
your graveyard for its flashback cost. Then exile it.)";

const CABAL_THERAPIST: &str = "Menace\nAt the beginning of your first main phase, you may \
sacrifice a creature. When you do, choose a nonland card name, then target player reveals their \
hand and discards all cards with that name.";

const TSABOS_DECREE: &str = "Choose a creature type. Target player reveals their hand and \
discards all creature cards of that type. Then destroy all creatures of that type that player \
controls. They can't be regenerated.";

const NICOL_BOLAS: &str = "Flying\nAt the beginning of your upkeep, sacrifice Nicol Bolas unless \
you pay {U}{B}{R}.\nWhenever Nicol Bolas deals damage to an opponent, that player discards their \
hand.";

const MILLSTONE: &str = "{2}, {T}: Target player mills two cards.";

fn free() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![],
    }
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn assert_zones(runner: &GameRunner, expected: &[(&str, ObjectId, Zone)]) {
    for (label, id, zone_expected) in expected {
        assert_eq!(zone(runner, *id), *zone_expected, "{label}");
    }
}

/// The spell or ability finished resolving: no unanswered prompt (in particular no
/// `DiscardChoice`) and nothing left on the stack.
fn assert_resolved(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the resolution must finish without a prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the stack must be empty");
}

/// Every card a `Discarded` event names, in event order.
fn discarded(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::Discarded { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

/// Sorted copy, for order-insensitive set comparisons.
fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// The discard instruction itself resolved (not a gap): its completion marker was
/// published.
fn discard_resolved(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Discard,
                ..
            }
        )
    })
}

fn add_sorcery(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, false, text)
        .with_mana_cost(free())
        .id()
}

fn with_card_names(runner: &mut GameRunner) {
    runner.state_mut().all_card_names = ["Lightning Bolt", "Shock", "Opt"].map(String::from).into();
}

/// R1. CR 701.9a + CR 608.2c: Amnesia's target discards every nonland card in their
/// own hand; the count is bound to the target's hand. Fails at base (the clause is a
/// gap, nothing moves) and with the count read against the caster's hand (one nonland
/// card there, three in the target's, so a `DiscardChoice` prompt stops resolution).
#[test]
fn amnesia_target_discards_every_nonland_card() {
    let mut scenario = GameScenario::new_n_player(3, 41);
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario
        .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
        .id();
    let instant = scenario.add_spell_to_hand(P1, "Opt", true).id();
    let artifact = scenario
        .add_artifact_to_hand_from_oracle(P1, "Millstone", MILLSTONE)
        .id();
    let land = scenario.add_land_to_hand(P1, "Forest").id();
    let casters_card = scenario.add_creature_to_hand(P0, "Hill Giant", 3, 3).id();
    let third_card = scenario
        .add_creature_to_hand(P2, "Grizzly Bears", 2, 2)
        .id();
    let spell = add_sorcery(&mut scenario, "Amnesia", AMNESIA);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("P1 creature card", creature, Zone::Graveyard),
            ("P1 instant card", instant, Zone::Graveyard),
            ("P1 artifact card", artifact, Zone::Graveyard),
            ("P1 land card", land, Zone::Hand),
            ("caster's nonland card", casters_card, Zone::Hand),
            ("third player's nonland card", third_card, Zone::Hand),
        ],
    );
    assert_eq!(
        sorted(discarded(outcome.events())),
        sorted(vec![creature, instant, artifact]),
        "exactly the target's nonland cards are discarded"
    );
}

/// R2. CR 701.9a: a hand with no nonland card discards nothing and the spell resolves
/// without a prompt. Reach-guards: the reveal happened and the discard instruction
/// itself resolved (at base the clause is a gap, so no discard completes).
#[test]
fn amnesia_with_only_lands_discards_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let forest = scenario.add_land_to_hand(P1, "Forest").id();
    let swamp = scenario.add_land_to_hand(P1, "Swamp").id();
    let casters_card = scenario.add_creature_to_hand(P0, "Hill Giant", 3, 3).id();
    let spell = add_sorcery(&mut scenario, "Amnesia", AMNESIA);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    assert_resolved(&runner);
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P1 && card_ids.contains(&forest) && card_ids.contains(&swamp)
        )),
        "reach-guard: P1 revealed their hand"
    );
    assert!(
        discard_resolved(outcome.events()),
        "reach-guard: the discard instruction resolved"
    );
    assert!(
        discarded(outcome.events()).is_empty(),
        "nothing is discarded"
    );
    assert_zones(
        &runner,
        &[
            ("P1 Forest", forest, Zone::Hand),
            ("P1 Swamp", swamp, Zone::Hand),
            ("caster's nonland card", casters_card, Zone::Hand),
        ],
    );
}

/// R4. Amnesia ruling: a player may target themselves. The caster discards their own
/// nonland cards; Amnesia itself is on the stack, not in the hand, so it goes to the
/// graveyard as a resolved sorcery and is never discarded.
#[test]
fn amnesia_targeting_yourself_discards_your_nonland_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let own_creature = scenario.add_creature_to_hand(P0, "Hill Giant", 3, 3).id();
    let own_land = scenario.add_land_to_hand(P0, "Island").id();
    let opponents_card = scenario
        .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
        .id();
    let spell = add_sorcery(&mut scenario, "Amnesia", AMNESIA);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P0).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("caster's nonland card", own_creature, Zone::Graveyard),
            ("caster's land card", own_land, Zone::Hand),
            ("Amnesia", spell, Zone::Graveyard),
            ("opponent's nonland card", opponents_card, Zone::Hand),
        ],
    );
    assert_eq!(
        discarded(outcome.events()),
        vec![own_creature],
        "only the caster's nonland hand card is discarded, never Amnesia itself"
    );
}

/// R5. CR 205.3k: Trapfinder's Trick discards only cards with the Trap spell type.
#[test]
fn trapfinders_trick_discards_only_trap_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let trap = scenario
        .add_spell_to_hand(P1, "Mindbreak Trap", true)
        .with_subtypes(vec!["Trap"])
        .id();
    let arcane = scenario
        .add_spell_to_hand(P1, "Glacial Ray", true)
        .with_subtypes(vec!["Arcane"])
        .id();
    let plain_instant = scenario.add_spell_to_hand(P1, "Opt", true).id();
    let land = scenario.add_land_to_hand(P1, "Island").id();
    let casters_trap = scenario
        .add_spell_to_hand(P0, "Mindbreak Trap", true)
        .with_subtypes(vec!["Trap"])
        .id();
    let spell = add_sorcery(&mut scenario, "Trapfinder's Trick", TRAPFINDERS_TRICK);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("P1 Trap card", trap, Zone::Graveyard),
            ("P1 Arcane card", arcane, Zone::Hand),
            ("P1 plain instant", plain_instant, Zone::Hand),
            ("P1 land", land, Zone::Hand),
            ("caster's Trap card", casters_trap, Zone::Hand),
        ],
    );
    assert_eq!(discarded(outcome.events()), vec![trap]);
}

/// R5b. CR 105.2c: Shatter Assumptions's first mode discards colorless nonland cards
/// only — a colorless land and a colored card stay.
#[test]
fn shatter_assumptions_colorless_mode() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let colorless_artifact = scenario
        .add_artifact_to_hand_from_oracle(P1, "Millstone", MILLSTONE)
        .id();
    let wastes = scenario.add_land_to_hand(P1, "Wastes").id();
    let green_creature = scenario
        .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let casters_artifact = scenario
        .add_artifact_to_hand_from_oracle(P0, "Millstone", MILLSTONE)
        .id();
    let spell = add_sorcery(&mut scenario, "Shatter Assumptions", SHATTER_ASSUMPTIONS);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).modes(&[0]).target_player(P1).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            (
                "P1 colorless artifact card",
                colorless_artifact,
                Zone::Graveyard,
            ),
            ("P1 Wastes", wastes, Zone::Hand),
            ("P1 green creature card", green_creature, Zone::Hand),
            (
                "caster's colorless artifact card",
                casters_artifact,
                Zone::Hand,
            ),
        ],
    );
    assert_eq!(discarded(outcome.events()), vec![colorless_artifact]);
}

/// R5c. CR 105.2b: Shatter Assumptions's second mode discards multicolored cards only.
#[test]
fn shatter_assumptions_multicolored_mode() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let two_color = scenario
        .add_creature_to_hand(P1, "Azorius Guildmage", 2, 2)
        .with_color(vec![ManaColor::White, ManaColor::Blue])
        .id();
    let mono = scenario
        .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let colorless = scenario
        .add_artifact_to_hand_from_oracle(P1, "Millstone", MILLSTONE)
        .id();
    let spell = add_sorcery(&mut scenario, "Shatter Assumptions", SHATTER_ASSUMPTIONS);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).modes(&[1]).target_player(P1).resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("P1 two-color creature card", two_color, Zone::Graveyard),
            ("P1 monocolored creature card", mono, Zone::Hand),
            ("P1 colorless artifact card", colorless, Zone::Hand),
        ],
    );
    assert_eq!(discarded(outcome.events()), vec![two_color]);
}

fn add_cabal_therapy(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Cabal Therapy", false)
        .from_oracle_text_with_keywords(&["Flashback"], CABAL_THERAPY)
        .with_mana_cost(free())
        .id()
}

/// R6. CR 201.2a + CR 608.2c: Cabal Therapy names a card as it resolves, and the target
/// discards every copy of it — two copies, no prompt. The caster's own copy stays.
/// With the count read against the caster's hand (one copy) a `DiscardChoice` stops
/// resolution.
#[test]
fn cabal_therapy_discards_every_copy_of_the_named_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bolt_a = scenario.add_bolt_to_hand(P1);
    let bolt_b = scenario.add_bolt_to_hand(P1);
    let shock = scenario.add_spell_to_hand(P1, "Shock", true).id();
    let forest = scenario.add_land_to_hand(P1, "Forest").id();
    let casters_bolt = scenario.add_bolt_to_hand(P0);
    let spell = add_cabal_therapy(&mut scenario);
    let mut runner = scenario.build();
    with_card_names(&mut runner);

    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .choose_option("Lightning Bolt")
        .resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("P1 first Lightning Bolt", bolt_a, Zone::Graveyard),
            ("P1 second Lightning Bolt", bolt_b, Zone::Graveyard),
            ("P1 Shock", shock, Zone::Hand),
            ("P1 Forest", forest, Zone::Hand),
            ("caster's Lightning Bolt", casters_bolt, Zone::Hand),
        ],
    );
    assert_eq!(
        sorted(discarded(outcome.events())),
        sorted(vec![bolt_a, bolt_b])
    );
}

/// R7. CR 701.9a: naming a card the target doesn't hold discards nothing and resolves
/// cleanly. Reach-guards: the name was chosen and the discard instruction resolved.
#[test]
fn cabal_therapy_naming_an_absent_card_discards_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bolt = scenario.add_bolt_to_hand(P1);
    let shock = scenario.add_spell_to_hand(P1, "Shock", true).id();
    let spell = add_cabal_therapy(&mut scenario);
    let mut runner = scenario.build();
    with_card_names(&mut runner);

    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .choose_option("Opt")
        .resolve();

    assert_resolved(&runner);
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::CardNamed { player_id, name, .. } if *player_id == P0 && name == "Opt"
        )),
        "reach-guard: the name was chosen"
    );
    assert!(
        discard_resolved(outcome.events()),
        "reach-guard: the discard instruction resolved"
    );
    assert!(discarded(outcome.events()).is_empty());
    assert_zones(
        &runner,
        &[
            ("P1 Lightning Bolt", bolt, Zone::Hand),
            ("P1 Shock", shock, Zone::Hand),
        ],
    );
}

/// What the Cabal Therapist trigger driver answers.
struct TherapistAnswers {
    accept_sacrifice: bool,
    fodder: ObjectId,
    name: &'static str,
}

/// Drives the Cabal Therapist first-main trigger from the moment it is put on the
/// stack until the stack is empty, answering each prompt through the public action
/// path. Returns every prompt kind seen and every event raised. Any prompt outside
/// the expected set (a `DiscardChoice` in particular) fails the test.
fn drive_therapist(
    runner: &mut GameRunner,
    answers: &TherapistAnswers,
) -> (Vec<&'static str>, Vec<GameEvent>) {
    let mut seen = Vec::new();
    let mut events = Vec::new();
    for _ in 0..64 {
        seen.push(runner.waiting_for_kind());
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return (seen, events);
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => GameAction::DecideOptionalEffect {
                accept: answers.accept_sacrifice,
            },
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&answers.fodder),
                    "the sacrifice offers the fodder creature: {cards:?}"
                );
                GameAction::SelectCards {
                    cards: vec![answers.fodder],
                }
            }
            WaitingFor::NamedChoice { .. } => GameAction::ChooseOption {
                choice: answers.name.to_string(),
            },
            other => panic!("unexpected prompt while resolving Cabal Therapist: {other:?}"),
        };
        events.extend(runner.act(action).expect("the prompt is answered").events);
    }
    panic!("the Cabal Therapist trigger never finished resolving");
}

struct TherapistBoard {
    runner: GameRunner,
    therapist: ObjectId,
    fodder: ObjectId,
    bolts: [ObjectId; 2],
    shock: ObjectId,
    casters_bolt: ObjectId,
}

/// A board in P0's upkeep, advanced into the first main phase so Cabal Therapist's
/// trigger is put on the stack by the real turn structure.
fn therapist_board() -> TherapistBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario.with_library_top(P0, &["Top One", "Top Two"]);
    scenario.with_library_top(P1, &["Top One", "Top Two"]);

    let therapist = scenario
        .add_creature(P0, "Cabal Therapist", 1, 1)
        .from_oracle_text_with_keywords(&["Menace"], CABAL_THERAPIST)
        .id();
    let fodder = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let bolts = [scenario.add_bolt_to_hand(P1), scenario.add_bolt_to_hand(P1)];
    let shock = scenario.add_spell_to_hand(P1, "Shock", true).id();
    let casters_bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    with_card_names(&mut runner);

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        !runner.state().stack.is_empty()
            || matches!(
                runner.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. }
            ),
        "reach-guard: the first-main trigger is being put on the stack, got {:?}",
        runner.state().waiting_for
    );

    TherapistBoard {
        runner,
        therapist,
        fodder,
        bolts,
        shock,
        casters_bolt,
    }
}

/// R8. CR 603.12 + CR 201.2a: accepting Cabal Therapist's sacrifice runs the reflexive
/// "When you do": the name is chosen, and the target discards every copy of it.
#[test]
fn cabal_therapist_when_you_do_discards_the_named_cards() {
    let mut board = therapist_board();
    let (seen, events) = drive_therapist(
        &mut board.runner,
        &TherapistAnswers {
            accept_sacrifice: true,
            fodder: board.fodder,
            name: "Lightning Bolt",
        },
    );

    assert_resolved(&board.runner);
    assert!(
        seen.contains(&"NamedChoice"),
        "the name was chosen: {seen:?}"
    );
    assert_zones(
        &board.runner,
        &[
            ("sacrificed fodder", board.fodder, Zone::Graveyard),
            ("Cabal Therapist", board.therapist, Zone::Battlefield),
            ("P1 first Lightning Bolt", board.bolts[0], Zone::Graveyard),
            ("P1 second Lightning Bolt", board.bolts[1], Zone::Graveyard),
            ("P1 Shock", board.shock, Zone::Hand),
            ("caster's Lightning Bolt", board.casters_bolt, Zone::Hand),
        ],
    );
    assert_eq!(sorted(discarded(&events)), sorted(board.bolts.to_vec()));
}

/// R8b. CR 603.12: declining the sacrifice means "When you do" never happens — no name
/// is chosen and nothing is discarded. Reach-guard: the optional sacrifice was offered.
#[test]
fn cabal_therapist_declined_sacrifice_discards_nothing() {
    let mut board = therapist_board();
    let (seen, events) = drive_therapist(
        &mut board.runner,
        &TherapistAnswers {
            accept_sacrifice: false,
            fodder: board.fodder,
            name: "Lightning Bolt",
        },
    );

    assert_resolved(&board.runner);
    assert!(
        seen.contains(&"OptionalEffectChoice"),
        "reach-guard: the sacrifice was offered: {seen:?}"
    );
    assert!(
        !seen.contains(&"NamedChoice"),
        "no name is chosen: {seen:?}"
    );
    assert!(discarded(&events).is_empty(), "nothing is discarded");
    assert_zones(
        &board.runner,
        &[
            ("fodder", board.fodder, Zone::Battlefield),
            ("P1 first Lightning Bolt", board.bolts[0], Zone::Hand),
            ("P1 second Lightning Bolt", board.bolts[1], Zone::Hand),
            ("P1 Shock", board.shock, Zone::Hand),
        ],
    );
}

/// R9. CR 205.3m + CR 702.73a: Tsabo's Decree discards the target's creature cards of
/// the chosen type, a changeling card included, and keeps other types, noncreature
/// cards and the caster's cards. Its later "destroy all creatures of that type that
/// player controls" still binds the target player.
#[test]
fn tsabos_decree_discards_creature_cards_of_the_chosen_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let goblin_card = scenario
        .add_creature_to_hand(P1, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin", "Warrior"])
        .id();
    let changeling_card = scenario
        .add_creature_to_hand(P1, "Changeling Outcast", 1, 1)
        .with_subtypes(vec!["Shapeshifter"])
        .with_keyword(Keyword::Changeling)
        .id();
    let elf_card = scenario
        .add_creature_to_hand(P1, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf", "Druid"])
        .id();
    let shock = scenario.add_spell_to_hand(P1, "Shock", true).id();
    let casters_goblin_card = scenario
        .add_creature_to_hand(P0, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin", "Warrior"])
        .id();
    let targets_goblin = scenario
        .add_creature(P1, "Goblin Guide", 2, 2)
        .with_subtypes(vec!["Goblin", "Scout"])
        .id();
    let casters_goblin = scenario
        .add_creature(P0, "Goblin Guide", 2, 2)
        .with_subtypes(vec!["Goblin", "Scout"])
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tsabo's Decree", true, TSABOS_DECREE)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = ["Elf", "Goblin", "Druid", "Warrior", "Scout"]
        .iter()
        .map(|t| t.to_string())
        .collect();

    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .choose_option("Goblin")
        .resolve();

    assert_resolved(&runner);
    assert_zones(
        &runner,
        &[
            ("P1 Goblin creature card", goblin_card, Zone::Graveyard),
            (
                "P1 changeling creature card",
                changeling_card,
                Zone::Graveyard,
            ),
            ("P1 Elf creature card", elf_card, Zone::Hand),
            ("P1 noncreature card", shock, Zone::Hand),
            (
                "caster's Goblin creature card",
                casters_goblin_card,
                Zone::Hand,
            ),
            (
                "P1's Goblin on the battlefield",
                targets_goblin,
                Zone::Graveyard,
            ),
            (
                "caster's Goblin on the battlefield",
                casters_goblin,
                Zone::Battlefield,
            ),
        ],
    );
    assert_eq!(
        sorted(discarded(outcome.events())),
        sorted(vec![goblin_card, changeling_card])
    );
}

/// Passes priority (answering an empty declare-blockers window) until `phase`.
fn advance_through_combat_to(runner: &mut GameRunner, phase: Phase) {
    for _ in 0..24 {
        if runner.state().phase == phase {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("declaring no blockers should succeed");
            }
            other => panic!("unexpected window while advancing to {phase:?}: {other:?}"),
        }
    }
    panic!("never reached {phase:?}; at {:?}", runner.state().phase);
}

/// R10. CR 701.9a + CR 608.2c: Nicol Bolas's "that player discards their hand" — the
/// damaged player discards every card in their own hand, not a count of the
/// controller's hand. P0 holds no cards, so a count read against the controller would
/// discard nothing.
#[test]
fn nicol_bolas_damaged_player_discards_their_whole_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bolas = scenario
        .add_creature(P0, "Nicol Bolas", 7, 7)
        .from_oracle_text_with_keywords(&["Flying"], NICOL_BOLAS)
        .id();
    let hand = [
        scenario.add_card_to_hand(P1, "Opt"),
        scenario.add_card_to_hand(P1, "Shock"),
        scenario.add_land_to_hand(P1, "Forest").id(),
    ];
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "precondition: the controller holds no cards"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bolas, AttackTarget::Player(P1))])
        .expect("declare attackers");
    advance_through_combat_to(&mut runner, Phase::PostCombatMain);

    assert_resolved(&runner);
    assert_eq!(
        runner.life(P1),
        13,
        "reach-guard: Nicol Bolas dealt combat damage"
    );
    for id in hand {
        assert_eq!(
            zone(&runner, id),
            Zone::Graveyard,
            "P1's whole hand is discarded"
        );
    }
}
