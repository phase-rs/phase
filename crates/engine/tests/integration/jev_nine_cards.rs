//! Production-pipeline regressions for nine Oracle-to-runtime repairs: each
//! card is parsed from its verbatim Oracle text and exercised through casting,
//! triggers, and resolution prompts.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SAILORS_BANE: &str = "This spell costs {1} less to cast for each card you own in exile and in your graveyard that's an instant card, a sorcery card, or a card that has an Adventure.\nWard {4}";
const MACHINISTS_DISMISSAL: &str = "This spell costs {1} less to cast for each artifact you control and each artifact card in your hand.\nCounter target noncreature spell.";
const CRATEROUS_STOMP: &str = "Craterous Stomp deals 3 damage to target creature an opponent controls. Each other creature that player controls becomes a Coward in addition to its other types and gains \"This creature can't block Giants or Warriors.\"";
const BROKEN_VISAGE: &str = "Destroy target nonartifact attacking creature. It can't be regenerated. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness. Sacrifice the token at the beginning of the next end step.";
const GAME_PRESERVE: &str = "At the beginning of your upkeep, each player reveals the top card of their library. If all cards revealed this way are creature cards, put those cards onto the battlefield under their owners' control.";
const IMMARD: &str = "Whenever Immard, the Stormcleaver enters or attacks, put a charge counter on it or remove one from it. When you remove a counter this way, choose one \u{2014}\n\u{2022} Immard deals 4 damage to any target.\n\u{2022} Immard gains lifelink and indestructible until end of turn.";
const ASHLING: &str = "As this enchantment enters, choose odd or even. (Zero is even.)\nEach creature with mana value of the chosen quality has haste.\nEach creature without mana value of the chosen quality enters tapped.";
const LOCKET: &str = "Spells you cast cost {1} less to cast for each card with the same name as that spell in your graveyard.";
const CIRCADIAN: &str = "Vivid \u{2014} Seek X cards that each share a color with one or more permanents you control, where X is the number of colors among permanents you control. For each color among permanents you control, those cards perpetually gain \"This spell costs {1} less to cast.\"";

fn pool(units: &[ManaType]) -> Vec<ManaUnit> {
    units
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn colorless(n: usize) -> Vec<ManaType> {
    vec![ManaType::Colorless; n]
}

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn creature_card_on_library_top(
    scenario: &mut GameScenario,
    player: engine::types::player::PlayerId,
    name: &str,
) -> ObjectId {
    scenario
        .add_spell_to_library_top(player, name, false)
        .as_creature()
        .id()
}

/// Answer the prompts a resolution raises with `answer`, then pass priority
/// until the stack is empty. Panics on a prompt `answer` declines.
fn settle(runner: &mut GameRunner, mut answer: impl FnMut(&WaitingFor) -> Option<GameAction>) {
    for _ in 0..60 {
        let waiting = runner.state().waiting_for.clone();
        match &waiting {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass is accepted");
            }
            other => {
                let action =
                    answer(other).unwrap_or_else(|| panic!("unanswered prompt: {other:?}"));
                runner.act(action).expect("prompt answer is accepted");
            }
        }
    }
    panic!("resolution did not settle");
}

/// CR 601.2f + CR 108.3 + CR 715.2a: owned instants, sorceries, and Adventure
/// cards in exile AND in the graveyard each reduce Sailors' Bane by {1}; an
/// opponent's card and a non-qualifying card do not.
#[test]
fn sailors_bane_counts_owned_qualifying_cards_across_exile_and_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bane = scenario
        .add_creature_to_hand_from_oracle(P0, "Sailors' Bane", 7, 7, SAILORS_BANE)
        .with_mana_cost(cost(7, vec![ManaCostShard::Blue, ManaCostShard::Blue]))
        .id();
    scenario.add_spell_to_exile(P0, "Exiled Instant", true);
    scenario.add_spell_to_graveyard(P0, "Graveyard Sorcery", false);
    scenario.add_creature_to_graveyard(P0, "Graveyard Creature", 2, 2);
    scenario.add_spell_to_exile(P1, "Opponent's Exiled Instant", true);
    let mut units = vec![ManaType::Blue, ManaType::Blue];
    units.extend(colorless(6));
    scenario.with_mana_pool(P0, pool(&units));
    let mut runner = scenario.build();

    let outcome = runner.cast(bane).resolve();
    outcome.assert_zone(&[bane], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        1,
        "two owned qualifying cards reduce {{7}}{{U}}{{U}} to {{5}}{{U}}{{U}}"
    );
}

/// CR 601.2f + CR 109.4: artifacts you control and artifact cards in your hand
/// both reduce Machinist's Dismissal; an opponent's artifact and a nonartifact
/// card in hand do not. The counter still resolves.
#[test]
fn machinists_dismissal_counts_controlled_artifacts_and_artifact_cards_in_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dismissal = scenario
        .add_spell_to_hand_from_oracle(P0, "Machinist's Dismissal", true, MACHINISTS_DISMISSAL)
        .with_mana_cost(cost(4, vec![ManaCostShard::Blue]))
        .id();
    scenario
        .add_creature(P0, "Controlled Artifact", 1, 1)
        .as_artifact();
    scenario.add_artifact_to_hand_from_oracle(P0, "Artifact In Hand", "");
    scenario
        .add_creature(P1, "Opponent Artifact", 1, 1)
        .as_artifact();
    scenario.add_card_to_hand(P0, "Nonartifact In Hand");
    let target = scenario
        .add_spell_to_hand(P0, "Target Sorcery", true)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut units = vec![ManaType::Blue];
    units.extend(colorless(3));
    scenario.with_mana_pool(P0, pool(&units));
    let mut runner = scenario.build();

    runner.cast(target).commit();
    let outcome = runner.cast(dismissal).target_objects(&[target]).resolve();
    outcome.assert_zone(&[target, dismissal], Zone::Graveyard);
    assert_eq!(
        outcome.mana_pool_total(P0),
        1,
        "two artifacts reduce {{4}}{{U}} to {{2}}{{U}}"
    );
}

/// CR 608.2c + CR 109.4 + CR 205.1b: "that player" is the target's controller
/// and "each other creature" excludes the target, not the spell.
#[test]
fn craterous_stomp_grants_coward_to_the_target_controllers_other_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stomp = scenario
        .add_spell_to_hand_from_oracle(P0, "Craterous Stomp", true, CRATEROUS_STOMP)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let target = scenario.add_creature(P1, "Target", 5, 5).id();
    let teammate = scenario.add_creature(P1, "Teammate", 2, 2).id();
    let own = scenario.add_creature(P0, "Own Creature", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(stomp).target_objects(&[target]).resolve();
    let state = outcome.state();
    assert_eq!(state.objects[&target].damage_marked, 3);
    let is_coward = |id: ObjectId| {
        state.objects[&id]
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Coward")
    };
    assert!(
        is_coward(teammate),
        "the target's controller's other creature becomes a Coward"
    );
    assert!(
        !is_coward(target),
        "the damaged target is not one of the other creatures"
    );
    assert!(!is_coward(own), "the caster's creature is not affected");
}

/// CR 111.3 + CR 701.8a + CR 603.7: the destroyed attacker's power and
/// toughness define the black Spirit token, which is sacrificed at the next
/// end step.
#[test]
fn broken_visage_copies_the_destroyed_attackers_power_and_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 5, 7).id();
    let visage = scenario
        .add_spell_to_hand_from_oracle(P0, "Broken Visage", true, BROKEN_VISAGE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("attack declares");

    let outcome = runner.cast(visage).target_objects(&[attacker]).resolve();
    outcome.assert_zone(&[attacker], Zone::Graveyard);
    let spirit = outcome
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| outcome.state().objects[id].name == "Spirit")
        .expect("a Spirit token is created");
    let token = &outcome.state().objects[&spirit];
    assert_eq!((token.power, token.toughness), (Some(5), Some(7)));
    assert_eq!(token.color, vec![ManaColor::Black]);
    assert_eq!(token.controller, P0);

    // Pass through blockers and damage into the end step, resolving whatever
    // triggers on the way.
    for _ in 0..40 {
        let state = runner.state();
        if state.phase == Phase::End
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.delayed_triggers.is_empty()
        {
            break;
        }
        match state.waiting_for {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass is accepted");
            }
        }
    }
    assert!(
        !runner.state().objects.contains_key(&spirit)
            || runner.state().objects[&spirit].zone != Zone::Battlefield,
        "the Spirit token is sacrificed at the beginning of the next end step"
    );
}

fn game_preserve_upkeep(p1_top_is_creature: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_enchantment_from_oracle(P0, "Game Preserve", GAME_PRESERVE);
    let mine = creature_card_on_library_top(&mut scenario, P0, "My Creature Card");
    let theirs = if p1_top_is_creature {
        creature_card_on_library_top(&mut scenario, P1, "Their Creature Card")
    } else {
        scenario
            .add_spell_to_library_top(P1, "Their Sorcery", false)
            .id()
    };
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::Upkeep);
    settle(&mut runner, |_| None);
    (runner, mine, theirs)
}

/// CR 701.20b + CR 108.3 + CR 608.2c: when every revealed card is a creature
/// card, each enters under its owner's control.
#[test]
fn game_preserve_puts_all_revealed_creatures_onto_the_battlefield_under_owners() {
    let (runner, mine, theirs) = game_preserve_upkeep(true);
    let state = runner.state();
    assert_eq!(state.objects[&mine].zone, Zone::Battlefield);
    assert_eq!(state.objects[&theirs].zone, Zone::Battlefield);
    assert_eq!(state.objects[&mine].controller, P0);
    assert_eq!(state.objects[&theirs].controller, P1);
}

/// CR 608.2c: one noncreature card among the revealed cards moves none.
#[test]
fn game_preserve_moves_nothing_when_a_revealed_card_is_not_a_creature() {
    let (runner, mine, theirs) = game_preserve_upkeep(false);
    let state = runner.state();
    assert_eq!(state.objects[&mine].zone, Zone::Library);
    assert_eq!(state.objects[&theirs].zone, Zone::Library);
}

fn immard_attack(chosen_branch: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let immard = scenario
        .add_creature_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, IMMARD)
        .id();
    scenario.with_counter(immard, CounterType::Generic("charge".to_string()), 1);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(immard, AttackTarget::Player(P1))])
        .expect("attack declares");
    let branch = chosen_branch.to_string();
    settle(&mut runner, |waiting| match waiting {
        WaitingFor::ChooseOneOfBranch {
            branch_descriptions,
            ..
        } => {
            assert_eq!(
                branch_descriptions.len(),
                2,
                "with a charge counter present, both adding and removing are options"
            );
            let index = branch_descriptions
                .iter()
                .position(|description| description.contains(&branch))
                .expect("branch offered");
            Some(GameAction::ChooseBranch { index })
        }
        WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
            Some(GameAction::SelectModes { indices: vec![0] })
        }
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
            Some(GameAction::SelectTargets {
                targets: vec![engine::types::ability::TargetRef::Player(P1)],
            })
        }
        _ => None,
    });
    (runner, immard)
}

/// CR 603.12 + CR 122.1: removing a charge counter creates the reflexive modal
/// trigger; Immard's first mode deals 4 damage.
#[test]
fn immard_removing_a_charge_counter_triggers_the_reflexive_mode() {
    let (runner, immard) = immard_attack("remove");
    let charge = CounterType::Generic("charge".to_string());
    assert_eq!(
        runner.state().objects[&immard]
            .counters
            .get(&charge)
            .copied()
            .unwrap_or(0),
        0
    );
    assert_eq!(
        runner.life(P1),
        16,
        "the reflexive mode deals 4 damage to the chosen player"
    );
}

/// CR 603.12: adding a counter is not "remove a counter this way".
#[test]
fn immard_adding_a_charge_counter_creates_no_reflexive_trigger() {
    let (runner, immard) = immard_attack("put");
    let charge = CounterType::Generic("charge".to_string());
    assert_eq!(
        runner.state().objects[&immard]
            .counters
            .get(&charge)
            .copied(),
        Some(2)
    );
    assert_eq!(runner.life(P1), 20);
}

/// CR 608.2d + CR 122.1: an Immard with no charge counter can only add one.
#[test]
fn immard_without_a_charge_counter_offers_only_adding_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let immard = scenario
        .add_creature_from_oracle(P0, "Immard, the Stormcleaver", 4, 4, IMMARD)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(immard, AttackTarget::Player(P1))])
        .expect("attack declares");
    let mut offered = None;
    settle(&mut runner, |waiting| match waiting {
        WaitingFor::ChooseOneOfBranch {
            branch_descriptions,
            ..
        } => {
            offered = Some(branch_descriptions.clone());
            Some(GameAction::ChooseBranch { index: 0 })
        }
        _ => None,
    });
    let charge = CounterType::Generic("charge".to_string());
    if let Some(offered) = offered {
        assert!(
            offered
                .iter()
                .all(|description| !description.contains("remove")),
            "removal is impossible with no charge counter: {offered:?}"
        );
    }
    assert_eq!(
        runner.state().objects[&immard]
            .counters
            .get(&charge)
            .copied(),
        Some(1)
    );
    assert_eq!(runner.life(P1), 20);
}

/// CR 607.2d + CR 614.1c: Ashling's persisted odd/even choice decides both its
/// haste grant and which entering creatures enter tapped.
#[test]
fn ashlings_prerogative_reads_its_own_chosen_parity() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ashling = scenario
        .add_spell_to_hand(P0, "Ashling's Prerogative", false)
        .as_enchantment()
        .from_oracle_text(ASHLING)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let odd = scenario
        .add_creature_to_hand(P0, "Odd Creature", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let even = scenario
        .add_creature_to_hand(P0, "Even Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(P0, pool(&colorless(3)));
    let mut runner = scenario.build();

    runner.cast(ashling).commit();
    settle(&mut runner, |waiting| match waiting {
        WaitingFor::NamedChoice { .. } => Some(GameAction::ChooseOption {
            choice: "Odd".to_string(),
        }),
        _ => None,
    });
    assert_eq!(runner.state().objects[&ashling].zone, Zone::Battlefield);

    runner.cast(odd).resolve();
    runner.cast(even).resolve();
    let state = runner.state();
    assert!(
        !state.objects[&odd].tapped,
        "an odd creature enters untapped"
    );
    assert!(
        state.objects[&odd].has_keyword(&Keyword::Haste),
        "an odd creature has haste"
    );
    assert!(
        state.objects[&even].tapped,
        "an even creature enters tapped"
    );
    assert!(!state.objects[&even].has_keyword(&Keyword::Haste));
}

/// Digital-only Alchemy seek + perpetual: with white and blue permanents, two
/// cards sharing those colors are sought and each perpetually costs {2} less.
#[test]
fn circadian_struggle_seeks_color_sharing_cards_and_reduces_each_by_color_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let struggle = scenario
        .add_spell_to_hand_from_oracle(P0, "Circadian Struggle", true, CIRCADIAN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario
        .add_creature(P0, "White Permanent", 1, 1)
        .with_color(vec![ManaColor::White]);
    scenario
        .add_creature(P0, "Blue Permanent", 1, 1)
        .with_color(vec![ManaColor::Blue]);
    let white = scenario
        .add_spell_to_library_top(P0, "White Card", true)
        .with_color(vec![ManaColor::White])
        .with_mana_cost(cost(3, vec![ManaCostShard::White]))
        .id();
    let blue = scenario
        .add_spell_to_library_top(P0, "Blue Card", true)
        .with_color(vec![ManaColor::Blue])
        .with_mana_cost(cost(3, vec![ManaCostShard::Blue]))
        .id();
    let red = scenario
        .add_spell_to_library_top(P0, "Red Card", true)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(struggle).resolve();
    outcome.assert_zone(&[white, blue], Zone::Hand);
    outcome.assert_zone(&[red], Zone::Library);

    // {3}{W} perpetually reduced by {2} costs {1}{W}.
    runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
    );
    runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    );
    let outcome = runner.cast(white).resolve();
    outcome.assert_zone(&[white], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);

    // Every sought card carries the grant, not only the first: {3}{U} → {1}{U}.
    runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
    );
    runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    );
    let outcome = runner.cast(blue).resolve();
    outcome.assert_zone(&[blue], Zone::Graveyard);
    assert_eq!(outcome.mana_pool_total(P0), 0);
}

/// CR 601.2f + CR 201.2a: Locket reduces each spell by {1} per card with that
/// spell's own name in its controller's graveyard; an opponent's same-name card,
/// a differently named card, and the Locket's own name do not count.
#[test]
fn locket_of_yesterdays_counts_graveyard_cards_named_like_the_spell_being_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Locket of Yesterdays", LOCKET)
        .with_mana_cost(ManaCost::generic(1));
    let shock = scenario
        .add_spell_to_hand(P0, "Shock", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let other = scenario
        .add_spell_to_hand(P0, "Other Spell", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.add_spell_to_graveyard(P0, "Shock", true);
    scenario.add_spell_to_graveyard(P0, "Shock", true);
    scenario.add_spell_to_graveyard(P1, "Shock", true);
    scenario.add_spell_to_graveyard(P0, "Locket of Yesterdays", false);
    scenario.with_mana_pool(P0, pool(&colorless(5)));
    let mut runner = scenario.build();

    let outcome = runner.cast(shock).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        4,
        "two Shocks in your graveyard reduce {{3}} to {{1}}"
    );
    let outcome = runner.cast(other).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        1,
        "no card shares Other Spell's name, so it costs the full {{3}}"
    );
}
