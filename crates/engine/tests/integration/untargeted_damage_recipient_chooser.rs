//! Resolution-time recipient chooser for an untargeted described damage recipient.
//!
//! "~ deals N damage to a creature or player" names its recipient by description,
//! not by the word "target", so nothing is announced when the ability goes on the
//! stack (CR 115.10a); the controller chooses the recipient while the instruction
//! is applied (CR 115.10 + CR 608.2d). The prompt is a choice, not a target, so
//! hexproof, shroud and protection never restrict it (CR 115.10a).
//!
//! The toy planeswalkers below isolate the recipient chooser from Comet, Stellar
//! Pup's die table (covered end to end in `comet_stellar_pup.rs`); their rows run
//! through the real activation pipeline. Wicked Guardian is the other printed class
//! member, exercised through its own enters trigger.

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, EffectKind, FilterProp,
    QuantityExpr, ResolvedAbility, TargetChoiceTiming, TargetFilter, TargetRef,
    TargetSelectionMode, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Comet's face-4/5 body, with the prose removal clause standing in for the
/// `[−2]` symbol (the symbol form is covered in `comet_stellar_pup.rs`).
const PLAIN_WALKER: &str = "[0]: Probe Walker deals damage equal to the number of loyalty counters on him to a creature or player, then remove two loyalty counters from Probe Walker.";

/// The same body as a full-range die row, so no RNG pinning is needed: the
/// instruction resolves inside the die-result branch frame whatever is rolled.
const DIE_WALKER: &str = "{T}: Roll a six-sided die.\n1\u{2014}6 | Probe Walker deals damage equal to the number of loyalty counters on him to a creature or player, then remove two loyalty counters from Probe Walker.";

/// A row whose damage reads the die result after the recipient is chosen.
const DIE_RESULT_WALKER: &str = "{T}: Roll a six-sided die.\n1\u{2014}6 | Probe Walker deals damage equal to the result to a creature or player.";

/// An object-only recipient: the controller's own creatures.
const OBJECT_RELIC: &str =
    "{T}: Probe Relic deals 2 damage to a creature you control, then you gain 1 life.";

/// A battlefield-object population that is not creature-only.
const POPULATION_RELIC: &str = "{T}: Probe Relic deals 2 damage to a creature or planeswalker.";

/// The full class shape without a planeswalker: objects and players.
const RECIPIENT_RELIC: &str = "{T}: Probe Relic deals 3 damage to a creature or player.";

/// The targeted form of the same class: "target" announces the recipient.
const TARGETED_WALKER: &str = "[0]: Probe Walker deals 2 damage to target creature or player.";

/// Verbatim Oracle text of the printed class member.
const WICKED_GUARDIAN: &str = "When this creature enters, you may have it deal 2 damage to another creature you control. If you do, draw a card.";

const FATAL_ATTRACTION: &str = "Enchant creature\nWhen this Aura enters, it deals 2 damage to enchanted creature.\nAt the beginning of your upkeep, this Aura deals 4 damage to enchanted creature.";

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    let mut next = Some(def);
    while let Some(d) = next {
        if matches!(&*d.effect, Effect::Unimplemented { .. }) {
            return true;
        }
        next = d.sub_ability.as_deref();
    }
    false
}

/// The index of the first parsed ability of `id` satisfying `pred`, read from the
/// parsed abilities and never hard-coded.
fn ability_index(
    runner: &GameRunner,
    id: ObjectId,
    pred: impl Fn(&AbilityDefinition) -> bool,
) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(pred)
        .expect("the fixture carries the expected ability")
}

fn loyalty_index(runner: &GameRunner, id: ObjectId) -> usize {
    ability_index(runner, id, |a| {
        matches!(a.cost, Some(AbilityCost::Loyalty { .. }))
    })
}

fn tap_index(runner: &GameRunner, id: ObjectId) -> usize {
    ability_index(runner, id, |a| matches!(a.cost, Some(AbilityCost::Tap)))
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn marked_damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

fn loyalty_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn activate(runner: &mut GameRunner, source: ObjectId, index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap_or_else(|error| panic!("the activation must be accepted: {error:?}"));
}

fn pick(runner: &mut GameRunner, target: TargetRef) -> Vec<GameEvent> {
    runner
        .act(GameAction::SelectTargets {
            targets: vec![target],
        })
        .unwrap_or_else(|error| panic!("the pick must be accepted: {error:?}"))
        .events
}

fn is_priority_for(runner: &GameRunner, player: PlayerId) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: holder } if holder == player
    )
}

fn assert_priority_for(runner: &GameRunner, player: PlayerId) {
    assert!(
        is_priority_for(runner, player),
        "expected {player:?} to hold priority, got {}",
        short(&runner.state().waiting_for)
    );
}

/// The recipient prompt the engine is waiting on.
struct Prompt {
    player: PlayerId,
    eligible: Vec<TargetRef>,
    min: u32,
    max: Option<u32>,
}

/// A readable head of a wait state: a full `Debug` dump of a pending cast is
/// thousands of characters.
fn short(waiting_for: &WaitingFor) -> String {
    format!("{waiting_for:?}").chars().take(90).collect()
}

fn prompt(runner: &GameRunner) -> Prompt {
    match &runner.state().waiting_for {
        WaitingFor::ChooseObjectsSelection {
            player,
            eligible,
            min,
            max,
            ..
        } => Prompt {
            player: *player,
            eligible: eligible.clone(),
            min: *min,
            max: *max,
        },
        other => panic!("expected the recipient prompt, got {}", short(other)),
    }
}

fn die_face(events: &[GameEvent]) -> Option<u8> {
    events.iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: 6,
            result: Some(result),
            ..
        } => Some(*result),
        _ => None,
    })
}

fn tracked_set_event(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChooseObjectsIntoTrackedSet,
                ..
            }
        )
    })
}

/// Every object and player an untargeted creature-or-player instruction may hit:
/// the two creatures the opponent controls (one hexproof), the controller's own
/// creature, and both players. The artifact, the opposing planeswalker and the
/// source planeswalker are not in the population.
struct Board {
    runner: GameRunner,
    walker: ObjectId,
    own_creature: ObjectId,
    own_artifact: ObjectId,
    opposing_creature: ObjectId,
    hexproof_creature: ObjectId,
    opposing_walker: ObjectId,
}

fn board(walker_oracle: &str) -> Board {
    let mut scenario = scenario();
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, walker_oracle)
        .id();
    let own_creature = scenario.add_creature(P0, "Own Bear", 2, 9).id();
    let own_artifact = scenario.add_artifact_from_oracle(P0, "Own Idol", "").id();
    let opposing_creature = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    let hexproof_creature = scenario
        .add_creature(P1, "Hexproof Giant", 3, 9)
        .hexproof()
        .id();
    let opposing_walker = scenario
        .add_planeswalker_from_oracle(P1, "Opposing Walker", "Probe", 4, "[+1]: You gain 1 life.")
        .id();
    Board {
        runner: scenario.build(),
        walker,
        own_creature,
        own_artifact,
        opposing_creature,
        hexproof_creature,
        opposing_walker,
    }
}

/// Activate the board's walker and resolve its ability up to the recipient prompt.
fn board_at_prompt(walker_oracle: &str) -> Board {
    let mut b = board(walker_oracle);
    let index = loyalty_index(&b.runner, b.walker);
    activate(&mut b.runner, b.walker, index);
    b.runner.advance_until_stack_empty();
    b
}

/// The reach-guard every runtime row starts from: the walker's ability parsed to
/// a described creature-or-player recipient chosen at resolution, with its
/// removal clause intact.
fn assert_recipient_ability(runner: &GameRunner, walker: ObjectId, index: usize) {
    let def = &runner.state().objects[&walker].abilities[index];
    assert!(!chain_has_unimplemented(def), "{def:#?}");
    assert!(
        matches!(
            &*def.effect,
            Effect::DealDamage {
                target: TargetFilter::Or { .. },
                damage_source: None,
                ..
            }
        ),
        "{def:#?}"
    );
    assert_eq!(
        def.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "an untargeted described recipient is chosen at resolution: {def:#?}"
    );
    assert!(
        def.sub_ability
            .as_deref()
            .is_some_and(|sub| matches!(&*sub.effect, Effect::RemoveCounter { .. })),
        "{def:#?}"
    );
}

fn parse_head(text: &str) -> AbilityDefinition {
    parse_effect_chain(text, AbilityKind::Spell)
}

fn is_damage(def: &AbilityDefinition) -> bool {
    matches!(&*def.effect, Effect::DealDamage { .. })
}

/// CR 115.10a + CR 608.2d: the class — a described recipient with no "target" —
/// is chosen at resolution. Each positive is paired with the unchanged forms
/// below it, so the classifier is neither inert nor a blanket re-timing.
#[test]
fn described_damage_recipients_are_chosen_at_resolution() {
    for text in [
        "~ deals 3 damage to a creature or player.",
        "~ deals 3 damage to a creature you control.",
        "~ deals 3 damage to a creature or planeswalker.",
        "~ deals damage equal to the number of loyalty counters on him to a creature or player.",
    ] {
        let def = parse_head(text);
        assert!(!chain_has_unimplemented(&def), "{text}: {def:#?}");
        assert!(is_damage(&def), "{text}: {def:#?}");
        assert_eq!(
            def.target_choice_timing,
            TargetChoiceTiming::Resolution,
            "{text}"
        );
    }
}

/// Comet's face-4/5 body in prose form: the recipient is chosen at resolution and
/// the removal clause that follows it parses to a loyalty-counter removal on the
/// source, so the amount is read before the counters are removed (CR 608.2h).
#[test]
fn the_face_four_five_body_parses_with_its_removal_clause() {
    let def = parse_head(
        "~ deals damage equal to the number of loyalty counters on him to a creature or player, then remove two loyalty counters from ~.",
    );
    assert!(!chain_has_unimplemented(&def), "{def:#?}");
    assert!(is_damage(&def), "{def:#?}");
    assert_eq!(def.target_choice_timing, TargetChoiceTiming::Resolution);
    let sub = def.sub_ability.as_deref().expect("the removal clause");
    assert!(
        matches!(
            &*sub.effect,
            Effect::RemoveCounter {
                counter_type: Some(CounterType::Loyalty),
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            }
        ),
        "{sub:#?}"
    );
}

/// The forms that name their recipient by "target", by anaphor or by a mass scope
/// keep the reading they had before: stack-time announcement, or no choice at all.
#[test]
fn recipients_that_are_not_a_resolution_choice_keep_their_reading() {
    for text in [
        "~ deals 3 damage to target creature or player.",
        "~ deals 3 damage to any target.",
        "~ deals 4 damage divided as you choose among any number of targets.",
    ] {
        let def = parse_head(text);
        assert!(!chain_has_unimplemented(&def), "{text}: {def:#?}");
        assert!(is_damage(&def), "{text}: {def:#?}");
        assert_eq!(
            def.target_choice_timing,
            TargetChoiceTiming::Stack,
            "{text}: \"target\" announces the recipient (CR 115.1)"
        );
    }

    // A mass scope is not a DealDamage node at all; the reach-guard proves the
    // text parsed (no Unimplemented) before the negative is read.
    let each = parse_head("~ deals 3 damage to each opponent.");
    assert!(!chain_has_unimplemented(&each), "{each:#?}");
    assert!(!is_damage(&each), "{each:#?}");
    assert_eq!(each.target_choice_timing, TargetChoiceTiming::Stack);

    // "that creature" after a targeted clause is the earlier clause's target
    // (CR 608.2c), never a fresh choice.
    let anaphor = parse_head("Destroy target creature. ~ deals 3 damage to that creature.");
    assert!(!chain_has_unimplemented(&anaphor), "{anaphor:#?}");
    let damage = anaphor.sub_ability.as_deref().expect("the damage clause");
    assert!(is_damage(damage), "{damage:#?}");
    assert_eq!(damage.target_choice_timing, TargetChoiceTiming::Stack);
}

/// CR 101.1: the card's own words say the game, not the controller, makes the
/// selection ("chosen at random"), so there is no controller's choice to prompt
/// for: the paired positive above shares its recipient shape.
#[test]
fn a_random_selection_keeps_its_reading() {
    let random = parse_head("~ deals 3 damage to a creature chosen at random.");
    assert!(!chain_has_unimplemented(&random), "{random:#?}");
    assert!(is_damage(&random), "{random:#?}");
    assert_eq!(random.target_selection_mode, TargetSelectionMode::Random);
    assert_eq!(random.target_choice_timing, TargetChoiceTiming::Stack);
}

/// CR 601.2d + CR 608.2d: a division is a multi-recipient choice, not the single
/// recipient this class picks. A described population that is divided keeps the
/// reading it had before: the paired positive above shares its recipient shape.
#[test]
fn a_division_over_a_described_population_keeps_its_reading() {
    let divided = parse_head(
        "~ deals 4 damage divided as you choose among any number of creatures you control.",
    );
    assert!(!chain_has_unimplemented(&divided), "{divided:#?}");
    assert!(is_damage(&divided), "{divided:#?}");
    assert!(
        divided.distribute.is_some(),
        "reach-guard: the division parsed: {divided:#?}"
    );
    assert_eq!(divided.target_choice_timing, TargetChoiceTiming::Stack);
}

/// CR 303.4b + CR 301.5f: an Aura's own "enchanted creature" is determined by the
/// attachment, never chosen; it was resolved at resolution before this change and
/// still is. The paired positive is a described recipient parsed in the same run.
#[test]
fn an_attachment_host_is_determined_not_chosen() {
    let parsed = parse_oracle_text(
        FATAL_ATTRACTION,
        "Fatal Attraction",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let etb = parsed
        .triggers
        .first()
        .and_then(|trigger| trigger.execute.as_deref())
        .expect("the enters trigger body");
    assert!(is_damage(etb), "{etb:#?}");
    assert_eq!(etb.target_choice_timing, TargetChoiceTiming::Resolution);
    assert!(
        matches!(
            &*etb.effect,
            Effect::DealDamage {
                target: TargetFilter::Typed(filter),
                ..
            } if filter
                .properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::EnchantedBy))
        ),
        "the host stays a bare enchanted-by filter, not a recipient pool: {etb:#?}"
    );
    let described = parse_head("~ deals 3 damage to a creature or player.");
    assert_eq!(
        described.target_choice_timing,
        TargetChoiceTiming::Resolution
    );
}

/// CR 115.10a + CR 608.2d: nothing is announced when the ability goes on the
/// stack, and the controller is asked at resolution. The eligible set is every
/// creature and player — the hexproof creature included, since this is a choice
/// and not a target — and no non-creature permanent.
#[test]
fn the_recipient_is_chosen_at_resolution_from_every_creature_and_player() {
    let mut b = board(PLAIN_WALKER);
    let index = loyalty_index(&b.runner, b.walker);
    assert_recipient_ability(&b.runner, b.walker, index);

    activate(&mut b.runner, b.walker, index);
    assert_priority_for(&b.runner, P0);

    b.runner.advance_until_stack_empty();
    let prompt = prompt(&b.runner);
    assert_eq!(prompt.player, P0, "the controller chooses");
    assert_eq!((prompt.min, prompt.max), (1, Some(1)));
    assert_eq!(
        prompt.eligible,
        vec![
            TargetRef::Object(b.own_creature),
            TargetRef::Object(b.opposing_creature),
            TargetRef::Object(b.hexproof_creature),
            TargetRef::Player(P0),
            TargetRef::Player(P1),
        ],
        "objects in battlefield order, then players in seat order"
    );
    for excluded in [b.own_artifact, b.opposing_walker, b.walker] {
        assert!(
            !prompt.eligible.contains(&TargetRef::Object(excluded)),
            "a non-creature permanent is not in a creature-or-player population"
        );
    }
}

/// CR 608.2h + CR 609.3: the amount is the loyalty counters on the source when
/// the damage is applied — before the following removal clause — and the pick,
/// a hexproof creature, takes it (CR 115.10a: a choice, not a target).
#[test]
fn the_chosen_creature_takes_damage_before_the_following_clause_runs() {
    let mut b = board_at_prompt(PLAIN_WALKER);
    assert_eq!(loyalty_counters(&b.runner, b.walker), 5, "reach-guard");
    let events = pick(&mut b.runner, TargetRef::Object(b.hexproof_creature));

    assert_eq!(
        marked_damage(&b.runner, b.hexproof_creature),
        5,
        "damage equal to the loyalty before the removal"
    );
    assert_eq!(
        loyalty_counters(&b.runner, b.walker),
        3,
        "reach-guard: the removal clause ran after the damage"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(target),
                amount: 5,
                ..
            } if *source_id == b.walker && *target == b.hexproof_creature
        )),
        "the walker dealt the damage"
    );
    assert!(is_priority_for(&b.runner, P0));
    assert!(b.runner.state().stack.is_empty());
}

/// CR 120.3a: damage to a player causes life loss. Either player may be chosen,
/// the controller included; the creature rows above are the paired positive.
#[test]
fn a_player_may_be_chosen_as_the_recipient() {
    for (chosen, damaged, untouched) in [(P1, P1, P0), (P0, P0, P1)] {
        let mut b = board_at_prompt(PLAIN_WALKER);
        pick(&mut b.runner, TargetRef::Player(chosen));
        assert_eq!(life(&b.runner, damaged), 15, "{damaged:?} lost 5 life");
        assert_eq!(life(&b.runner, untouched), 20, "{untouched:?} is untouched");
        assert_eq!(
            marked_damage(&b.runner, b.hexproof_creature),
            0,
            "no creature was damaged"
        );
        assert_eq!(
            loyalty_counters(&b.runner, b.walker),
            3,
            "reach-guard: the chain ran past the damage"
        );
        assert!(is_priority_for(&b.runner, P0));
    }
}

/// The answer is exactly one member of the published set: a non-member, an empty
/// answer, two picks and a duplicate are each rejected with the prompt intact,
/// and a valid pick afterwards still succeeds.
#[test]
fn only_one_eligible_member_is_accepted() {
    let mut b = board_at_prompt(PLAIN_WALKER);
    let before = b.runner.state().waiting_for.clone();
    let rejected: [Vec<TargetRef>; 5] = [
        vec![TargetRef::Object(b.own_artifact)],
        vec![TargetRef::Object(b.opposing_walker)],
        vec![],
        vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        vec![TargetRef::Player(P1), TargetRef::Player(P1)],
    ];
    for targets in rejected {
        let result = b.runner.act(GameAction::SelectTargets {
            targets: targets.clone(),
        });
        assert!(result.is_err(), "{targets:?} must be rejected");
        assert_eq!(
            b.runner.state().waiting_for,
            before,
            "{targets:?}: the prompt is unchanged"
        );
        assert!(
            b.runner.state().active_ability_continuation().is_some(),
            "{targets:?}: the parked instruction is unchanged"
        );
    }
    pick(&mut b.runner, TargetRef::Player(P1));
    assert_eq!(
        life(&b.runner, P1),
        15,
        "paired positive: a valid pick works"
    );
}

/// Only the chooser answers: the opponent is rejected as the wrong player and the
/// controller then succeeds.
#[test]
fn only_the_controller_may_answer() {
    let mut b = board_at_prompt(PLAIN_WALKER);
    let before = b.runner.state().waiting_for.clone();
    let rejected = apply(
        b.runner.state_mut(),
        P1,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        },
    );
    assert!(
        matches!(rejected, Err(EngineError::WrongPlayer)),
        "{rejected:?}"
    );
    assert_eq!(b.runner.state().waiting_for, before);
    apply(
        b.runner.state_mut(),
        P0,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        },
    )
    .expect("the controller answers");
    assert_eq!(life(&b.runner, P1), 15);
}

/// The pick binds to the parked instruction; it is not a tracked-set selection.
/// No tracked set is published and no tracked-set event is emitted. The producer
/// test below is the paired positive: its answer does publish.
#[test]
fn a_recipient_pick_publishes_no_tracked_set() {
    let mut b = board_at_prompt(PLAIN_WALKER);
    let sets = b.runner.state().tracked_object_sets.clone();
    let next_id = b.runner.state().next_tracked_set_id;
    let events = pick(&mut b.runner, TargetRef::Object(b.opposing_creature));
    assert_eq!(
        marked_damage(&b.runner, b.opposing_creature),
        5,
        "reach-guard: the pick resolved"
    );
    assert!(!tracked_set_event(&events));
    assert_eq!(b.runner.state().tracked_object_sets, sets);
    assert_eq!(b.runner.state().next_tracked_set_id, next_id);
}

/// CR 608.2d: the chooser is the ability's controller — not the active player.
/// An artifact controlled by the non-active player asks its controller.
#[test]
fn the_prompt_goes_to_the_controller_when_it_is_not_the_active_player() {
    let mut scenario = scenario();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", OBJECT_RELIC)
        .id();
    let first = scenario.add_creature(P0, "First Bear", 2, 9).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 9).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let index = tap_index(&runner, relic);
    activate(&mut runner, relic, index);
    runner.advance_until_stack_empty();

    let prompt = prompt(&runner);
    assert_eq!(prompt.player, P0, "the controller, not the active player");
    assert_eq!(
        prompt.eligible,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );
    pick(&mut runner, TargetRef::Object(second));
    assert_eq!(marked_damage(&runner, second), 2);
    assert_eq!(marked_damage(&runner, first), 0);
    assert_eq!(
        life(&runner, P0),
        21,
        "reach-guard: the following clause ran"
    );
}

/// CR 608.2d + CR 609.3: with no creature to choose there is nothing to damage,
/// no target was announced (so the ability could be activated at all), and the
/// instruction after it still happens.
#[test]
fn zero_candidates_resolve_without_a_prompt_or_damage() {
    let mut scenario = scenario();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", OBJECT_RELIC)
        .id();
    let opposing = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    let mut runner = scenario.build();
    let index = tap_index(&runner, relic);
    activate(&mut runner, relic, index);
    runner.advance_until_stack_empty();
    assert_priority_for(&runner, P0);
    assert_eq!(
        marked_damage(&runner, opposing),
        0,
        "not a creature you control"
    );
    assert_eq!(life(&runner, P0), 21, "the following clause ran");
}

/// CR 115.10a + CR 608.2d: a single candidate is a forced choice made as the
/// instruction is applied, not a target announced with the ability: the ability
/// on the stack carries none, and it is bound without a prompt at resolution.
#[test]
fn a_single_candidate_is_chosen_without_a_prompt() {
    let mut scenario = scenario();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", OBJECT_RELIC)
        .id();
    let only = scenario.add_creature(P0, "Only Bear", 2, 9).id();
    let mut runner = scenario.build();
    let index = tap_index(&runner, relic);
    activate(&mut runner, relic, index);
    let StackEntryKind::ActivatedAbility { ability, .. } = &runner.state().stack[0].kind else {
        panic!("expected the activated ability on the stack");
    };
    assert!(
        ability.targets.is_empty(),
        "nothing is announced for a described recipient: {:?}",
        ability.targets
    );

    runner.advance_until_stack_empty();
    assert_priority_for(&runner, P0);
    assert_eq!(marked_damage(&runner, only), 2, "the only candidate is hit");
    assert_eq!(life(&runner, P0), 21, "the following clause ran once");
    assert_eq!(
        runner
            .state()
            .ability_resolutions_this_turn
            .get(&(relic, index)),
        Some(&1),
        "binding the forced choice does not count a second resolution"
    );
}

/// With two candidates the controller is asked, among exactly their own creatures
/// ("you control"): the opposing creature is not offered.
#[test]
fn many_candidates_prompt_among_exactly_the_described_population() {
    let mut scenario = scenario();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", OBJECT_RELIC)
        .id();
    let first = scenario.add_creature(P0, "First Bear", 2, 9).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 9).id();
    let opposing = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    let mut runner = scenario.build();
    let index = tap_index(&runner, relic);
    activate(&mut runner, relic, index);
    runner.advance_until_stack_empty();
    let prompt = prompt(&runner);
    assert_eq!(
        prompt.eligible,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );
    assert!(!prompt.eligible.contains(&TargetRef::Object(opposing)));
    pick(&mut runner, TargetRef::Object(first));
    assert_eq!(marked_damage(&runner, first), 2);
    assert_eq!(marked_damage(&runner, second), 0);
    assert_eq!(life(&runner, P0), 21, "the following clause ran");
    assert_eq!(
        runner
            .state()
            .ability_resolutions_this_turn
            .get(&(relic, index)),
        Some(&1),
        "resuming after the prompt does not count a second resolution"
    );
}

/// A battlefield-object population that is not creature-only is offered whole:
/// every creature and planeswalker, and no player. The same walker-shaped
/// population as the creature-or-player rows, a different recipient class member.
#[test]
fn a_creature_or_planeswalker_population_offers_objects_only() {
    let mut scenario = scenario();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", POPULATION_RELIC)
        .id();
    let own_walker = scenario
        .add_planeswalker_from_oracle(P0, "Own Walker", "Probe", 5, "[+1]: You gain 1 life.")
        .id();
    let creature = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    let opposing_walker = scenario
        .add_planeswalker_from_oracle(P1, "Opposing Walker", "Probe", 4, "[+1]: You gain 1 life.")
        .id();
    let artifact = scenario.add_artifact_from_oracle(P1, "Idol", "").id();
    let mut runner = scenario.build();
    let index = tap_index(&runner, relic);
    let def = &runner.state().objects[&relic].abilities[index];
    assert_eq!(def.target_choice_timing, TargetChoiceTiming::Resolution);

    activate(&mut runner, relic, index);
    runner.advance_until_stack_empty();
    let prompt = prompt(&runner);
    assert_eq!(
        prompt.eligible,
        vec![
            TargetRef::Object(own_walker),
            TargetRef::Object(creature),
            TargetRef::Object(opposing_walker),
        ],
        "creatures and planeswalkers only: no player, no artifact"
    );
    assert!(!prompt.eligible.contains(&TargetRef::Object(artifact)));
    pick(&mut runner, TargetRef::Object(opposing_walker));
    assert_eq!(
        loyalty_counters(&runner, opposing_walker),
        2,
        "CR 120.3c: damage to a planeswalker removes that many loyalty counters"
    );
}

/// CR 608.2d inside a die-result branch frame: the prompt is raised from the
/// branch, the answer resumes it, and the chain ends with nothing left parked.
#[test]
fn the_prompt_works_inside_a_die_result_branch() {
    let mut b = board(DIE_WALKER);
    let die = ability_index(&b.runner, b.walker, |a| {
        matches!(&*a.effect, Effect::RollDie { .. })
    });
    let Effect::RollDie { results, .. } =
        &*b.runner.state().objects[&b.walker].abilities[die].effect
    else {
        unreachable!("the index was found by that shape");
    };
    assert_eq!(results.len(), 1);
    let row = &results[0].effect;
    assert!(!chain_has_unimplemented(row), "{row:#?}");
    assert_eq!(row.target_choice_timing, TargetChoiceTiming::Resolution);

    activate(&mut b.runner, b.walker, die);
    b.runner.advance_until_stack_empty();
    let prompt = prompt(&b.runner);
    assert_eq!(prompt.player, P0);
    assert!(prompt.eligible.contains(&TargetRef::Player(P1)));
    assert!(prompt
        .eligible
        .contains(&TargetRef::Object(b.hexproof_creature)));

    let events = pick(&mut b.runner, TargetRef::Object(b.hexproof_creature));
    assert!(die_face(&events).is_none(), "the die is not rolled again");
    assert_eq!(marked_damage(&b.runner, b.hexproof_creature), 5);
    assert_eq!(
        loyalty_counters(&b.runner, b.walker),
        3,
        "reach-guard: the removal clause ran in the branch"
    );
    assert!(is_priority_for(&b.runner, P0));
    assert!(b.runner.state().stack.is_empty());
    assert!(
        b.runner.state().resolution_stack.is_empty(),
        "no frame is left parked: {:?}",
        b.runner.state().resolution_stack
    );
}

/// CR 706.2 + CR 608.2h: the die result is still the rolled face after the
/// prompt, so the damage equals the roll. The rolled face is read from the event
/// as the positive reach-guard; a prompt that lost it would deal 0.
#[test]
fn the_die_result_survives_the_recipient_prompt() {
    let mut b = board(DIE_RESULT_WALKER);
    let die = ability_index(&b.runner, b.walker, |a| {
        matches!(&*a.effect, Effect::RollDie { .. })
    });
    let rolled = {
        activate(&mut b.runner, b.walker, die);
        let mut events = Vec::new();
        for _ in 0..20 {
            if b.runner.state().stack.is_empty() {
                break;
            }
            match b.runner.act(GameAction::PassPriority) {
                Ok(result) => events.extend(result.events),
                Err(_) => break,
            }
        }
        die_face(&events).expect("reach-guard: the die rolled")
    };
    let prompt = prompt(&b.runner);
    assert_eq!(prompt.player, P0);

    pick(&mut b.runner, TargetRef::Object(b.hexproof_creature));
    assert_eq!(
        marked_damage(&b.runner, b.hexproof_creature),
        u32::from(rolled),
        "damage equals the rolled face"
    );
}

/// Inside a die-result branch the controller alone answers, as at the top level.
#[test]
fn only_the_controller_answers_inside_a_die_result_branch() {
    let mut b = board(DIE_WALKER);
    let die = ability_index(&b.runner, b.walker, |a| {
        matches!(&*a.effect, Effect::RollDie { .. })
    });
    activate(&mut b.runner, b.walker, die);
    b.runner.advance_until_stack_empty();
    let _reach_guard = prompt(&b.runner);
    let rejected = apply(
        b.runner.state_mut(),
        P1,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        },
    );
    assert!(
        matches!(rejected, Err(EngineError::WrongPlayer)),
        "{rejected:?}"
    );
    pick(&mut b.runner, TargetRef::Player(P1));
    assert_eq!(life(&b.runner, P1), 15);
}

/// CR 115.10a + CR 702.16e: protection restricts what a source may DAMAGE, not
/// what a player may CHOOSE. The protected creature is offered and chosen; the
/// damage is prevented; the instruction after it still runs. The unprotected
/// hexproof creature in the other rows is the paired positive.
#[test]
fn protection_does_not_restrict_the_choice_only_the_damage() {
    let mut scenario = scenario();
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, PLAIN_WALKER)
        .with_color(vec![ManaColor::Red])
        .id();
    let protected = scenario
        .add_creature(P1, "Red-Proof Ogre", 3, 9)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
        .id();
    let mut runner = scenario.build();
    let index = loyalty_index(&runner, walker);
    activate(&mut runner, walker, index);
    runner.advance_until_stack_empty();
    let prompt = prompt(&runner);
    assert!(
        prompt.eligible.contains(&TargetRef::Object(protected)),
        "a creature with protection from the source's color is still offered"
    );
    pick(&mut runner, TargetRef::Object(protected));
    assert_eq!(
        marked_damage(&runner, protected),
        0,
        "CR 702.16e: prevented"
    );
    assert_eq!(
        loyalty_counters(&runner, walker),
        3,
        "reach-guard: the chain ran past the prevented damage"
    );
}

/// The targeted form of the class is unchanged: "target" announces the recipient
/// as the ability goes on the stack (CR 115.1c + CR 602.2b) and nothing is asked
/// at resolution.
#[test]
fn the_targeted_form_still_announces_its_target() {
    let mut scenario = scenario();
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Probe Walker", "Probe", 5, TARGETED_WALKER)
        .id();
    let victim = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    let mut runner = scenario.build();
    let index = loyalty_index(&runner, walker);
    let def = &runner.state().objects[&walker].abilities[index];
    assert_eq!(def.target_choice_timing, TargetChoiceTiming::Stack);

    let outcome = runner
        .activate(walker, index)
        .target_object(victim)
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{}",
        short(outcome.final_waiting_for())
    );
    assert_eq!(marked_damage(&runner, victim), 2);
}

/// A tracked-set producer queued ahead of a described damage recipient keeps its
/// own contract: the first answer publishes a tracked set and does NOT bind the
/// damage, which then raises its own prompt. An object-only recipient makes the
/// two populations identical, so any routing that infers "recipient prompt" from
/// the candidate set alone would misroute the first answer.
#[test]
fn a_tracked_set_producer_ahead_of_the_damage_keeps_its_own_answer() {
    let mut scenario = scenario();
    let source = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", "")
        .id();
    let first = scenario.add_creature(P0, "First Bear", 2, 9).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 9).id();
    let mut runner = scenario.build();

    let creatures_you_control =
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
    let mut damage = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: creatures_you_control.clone(),
            damage_source: None,
            excess: None,
        },
        vec![],
        source,
        P0,
    );
    damage.target_choice_timing = TargetChoiceTiming::Resolution;
    let mut producer = ResolvedAbility::new(
        Effect::ChooseObjectsIntoTrackedSet {
            chooser: TargetFilter::Controller,
            filter: creatures_you_control,
            min: 1,
            max: Some(1),
            cardinality: None,
            eligibility: None,
        },
        vec![],
        source,
        P0,
    );
    producer.sub_ability = Some(Box::new(damage));

    let sets_before = runner.state().tracked_object_sets.len();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &producer, &mut events, 0).expect("chain resolves");
    let producer_prompt = prompt(&runner);
    assert_eq!(
        producer_prompt.eligible,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );

    // First answer: the producer's. It publishes a set and binds nothing.
    let events = pick(&mut runner, TargetRef::Object(first));
    assert!(tracked_set_event(&events), "the producer publishes");
    assert_eq!(runner.state().tracked_object_sets.len(), sets_before + 1);
    assert_eq!(
        marked_damage(&runner, first),
        0,
        "the producer's answer is not a damage recipient"
    );
    let recipient_prompt = prompt(&runner);
    assert_eq!(
        recipient_prompt.eligible,
        vec![TargetRef::Object(first), TargetRef::Object(second)],
        "the damage raised its own prompt"
    );

    // Second answer: the recipient. It binds the damage and publishes nothing.
    let events = pick(&mut runner, TargetRef::Object(second));
    assert!(!tracked_set_event(&events));
    assert_eq!(runner.state().tracked_object_sets.len(), sets_before + 1);
    assert_eq!(marked_damage(&runner, second), 2);
    assert_eq!(marked_damage(&runner, first), 0);
}

/// Wicked Guardian — "you may have it deal 2 damage to another creature you
/// control. If you do, draw a card." — names its recipient with no "target", so
/// the recipient is chosen at resolution. Accepting asks which of the other
/// creatures (never itself, never the opponent's); the pick takes the damage and
/// the "if you do" draw follows.
#[test]
fn wicked_guardian_asks_for_its_recipient_when_the_optional_is_accepted() {
    let mut scenario = scenario();
    let first = scenario.add_creature(P0, "First Bear", 2, 9).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 9).id();
    let opposing = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    scenario.add_land_to_library_top(P0, "Forest");
    scenario.add_land_to_library_top(P0, "Forest");
    let guardian = scenario
        .add_creature_to_hand_from_oracle(P0, "Wicked Guardian", 4, 2, WICKED_GUARDIAN)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(guardian).accept_optional().resolve();
    let WaitingFor::ChooseObjectsSelection {
        player, eligible, ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "expected the recipient prompt, got {}",
            short(outcome.final_waiting_for())
        );
    };
    assert_eq!(player, P0);
    assert_eq!(
        eligible,
        vec![TargetRef::Object(first), TargetRef::Object(second)],
        "exactly the controller's other creatures"
    );
    assert!(!eligible.contains(&TargetRef::Object(guardian)));
    assert!(!eligible.contains(&TargetRef::Object(opposing)));

    let hand_before = hand_size(&runner, P0);
    pick(&mut runner, TargetRef::Object(second));
    assert_eq!(marked_damage(&runner, second), 2);
    assert_eq!(marked_damage(&runner, first), 0);
    assert_eq!(
        hand_size(&runner, P0),
        hand_before + 1,
        "\"if you do\": the draw follows the damage"
    );
}

/// Declining the optional damage deals none and draws none; the accepted case
/// above is the paired positive that reaches the same trigger.
#[test]
fn wicked_guardian_declined_deals_no_damage_and_draws_nothing() {
    let mut scenario = scenario();
    let first = scenario.add_creature(P0, "First Bear", 2, 9).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 9).id();
    scenario.add_land_to_library_top(P0, "Forest");
    let guardian = scenario
        .add_creature_to_hand_from_oracle(P0, "Wicked Guardian", 4, 2, WICKED_GUARDIAN)
        .id();
    let mut runner = scenario.build();

    let hand_at_cast = hand_size(&runner, P0);
    let outcome = runner.cast(guardian).decline_optional().resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{}",
        short(outcome.final_waiting_for())
    );
    assert_eq!(marked_damage(&runner, first), 0);
    assert_eq!(marked_damage(&runner, second), 0);
    assert_eq!(
        hand_size(&runner, P0),
        hand_at_cast - 1,
        "only the cast creature left the hand: nothing was drawn"
    );
}

/// CR 608.2d: "a player can't choose an impossible option". With no other creature
/// there is nothing the damage could hit, so the optional is not offered at all
/// and accepting cannot draw. The two-creature acceptance above is the paired
/// positive that reaches the same trigger.
#[test]
fn wicked_guardian_with_no_other_creature_offers_nothing_and_draws_nothing() {
    let mut scenario = scenario();
    let opposing = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    scenario.add_land_to_library_top(P0, "Forest");
    let guardian = scenario
        .add_creature_to_hand_from_oracle(P0, "Wicked Guardian", 4, 2, WICKED_GUARDIAN)
        .id();
    let mut runner = scenario.build();

    let hand_at_cast = hand_size(&runner, P0);
    let outcome = runner.cast(guardian).accept_optional().resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{}",
        short(outcome.final_waiting_for())
    );
    assert_eq!(
        marked_damage(&runner, guardian),
        0,
        "not \"another\" creature"
    );
    assert_eq!(
        marked_damage(&runner, opposing),
        0,
        "not a creature you control"
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_at_cast - 1,
        "nothing was drawn"
    );
    assert_eq!(
        runner.state().objects[&guardian].zone,
        Zone::Battlefield,
        "reach-guard: the cast completed"
    );
}

/// With exactly one other creature the choice is forced: accepting binds it with
/// no prompt, and the draw follows. The lone creature has shroud — it cannot be
/// targeted, but this damage names no target (CR 115.10a), so it is still the
/// recipient. This is the derived reading of the card's text: the recipient is
/// chosen at resolution, so a creature a targeted effect could never name is
/// still a legal recipient. (At base the clause announced a target at the
/// trigger, found none legal, and the whole trigger was removed.)
#[test]
fn wicked_guardian_with_one_other_creature_binds_it_without_a_prompt() {
    let mut scenario = scenario();
    let only = scenario
        .add_creature(P0, "Shrouded Bear", 2, 9)
        .with_keyword(Keyword::Shroud)
        .id();
    scenario.add_land_to_library_top(P0, "Forest");
    let guardian = scenario
        .add_creature_to_hand_from_oracle(P0, "Wicked Guardian", 4, 2, WICKED_GUARDIAN)
        .id();
    let mut runner = scenario.build();

    let hand_at_cast = hand_size(&runner, P0);
    let outcome = runner.cast(guardian).accept_optional().resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{}",
        short(outcome.final_waiting_for())
    );
    assert_eq!(marked_damage(&runner, only), 2);
    assert_eq!(
        hand_size(&runner, P0),
        hand_at_cast,
        "the cast creature left the hand and one card was drawn"
    );
}

/// The wire shape is unchanged: the same `ChooseObjectsSelection` fields, now
/// carrying player entries, and the whole state round-trips while the prompt is
/// pending with its parked instruction. A relic carries the clause: a loyalty
/// activation count is deliberately not restored from a save, so a planeswalker
/// that has activated this turn would not be a fixpoint for a reason that has
/// nothing to do with the prompt.
#[test]
fn the_prompt_keeps_its_wire_shape_and_round_trips() {
    let mut scenario = scenario();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Probe Relic", RECIPIENT_RELIC)
        .id();
    let own = scenario.add_creature(P0, "Own Bear", 2, 9).id();
    let opposing = scenario.add_creature(P1, "Opposing Ogre", 3, 9).id();
    let mut runner = scenario.build();
    let index = tap_index(&runner, relic);
    activate(&mut runner, relic, index);
    runner.advance_until_stack_empty();

    let value = serde_json::to_value(&runner.state().waiting_for).expect("serialize the prompt");
    assert_eq!(value["type"], "ChooseObjectsSelection");
    let data = value["data"].as_object().expect("the prompt payload");
    for key in data.keys() {
        assert!(
            ["player", "eligible", "min", "max", "trigger_event"].contains(&key.as_str()),
            "no new field on the prompt: {key}"
        );
    }
    assert_eq!(
        data["eligible"],
        serde_json::json!([
            { "Object": own.0 },
            { "Object": opposing.0 },
            { "Player": 0 },
            { "Player": 1 },
        ])
    );
    assert_eq!(data["min"], 1);
    assert_eq!(data["max"], 1);

    let state: &GameState = runner.state();
    let first = serde_json::to_string(state).expect("serialize the state");
    let mut restored: GameState = serde_json::from_str(&first).expect("deserialize the state");
    let first: serde_json::Value = serde_json::from_str(&first).expect("state JSON");
    let second = serde_json::to_value(&restored).expect("reserialize the state");
    assert_eq!(
        first_difference(&first, &second, "state"),
        None,
        "the pending prompt round-trips as a fixpoint"
    );

    // The restored prompt is answerable: the parked instruction and the pool it
    // is waiting on survived, so a player pick still binds and damages.
    apply(
        &mut restored,
        P0,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        },
    )
    .expect("the restored prompt accepts a player pick");
    let life = |state: &GameState, player: PlayerId| {
        state
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists")
            .life
    };
    assert_eq!(life(&restored, P1), 17);
    assert_eq!(life(state, P1), 20, "the original state is untouched");
}

/// The first path at which two JSON values differ, for a readable failure.
fn first_difference(a: &serde_json::Value, b: &serde_json::Value, path: &str) -> Option<String> {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for key in x.keys().chain(y.keys()) {
                match (x.get(key), y.get(key)) {
                    (Some(l), Some(r)) => {
                        if let Some(d) = first_difference(l, r, &format!("{path}.{key}")) {
                            return Some(d);
                        }
                    }
                    (l, r) => {
                        return Some(format!(
                            "{path}.{key}: {:?} vs {:?}",
                            l.map(|v| v.to_string().chars().take(120).collect::<String>()),
                            r.map(|v| v.to_string().chars().take(120).collect::<String>())
                        ))
                    }
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: length {} vs {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (l, r))| first_difference(l, r, &format!("{path}[{i}]")))
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {a} vs {b}")),
    }
}

/// The AI consumer is generic over `TargetRef`: one `SelectTargets` per eligible
/// entry, both players included, and every one of them applies — a creature takes
/// the damage, a player loses the life.
#[test]
fn ai_candidates_cover_every_eligible_entry_including_players() {
    let eligible = prompt(&board_at_prompt(PLAIN_WALKER).runner).eligible;
    assert_eq!(eligible.len(), 5, "reach-guard: the full population");

    for target in eligible {
        let mut b = board_at_prompt(PLAIN_WALKER);
        let actions = engine::ai_support::legal_actions(b.runner.state());
        let offered = actions
            .iter()
            .filter(|action| matches!(action, GameAction::SelectTargets { .. }))
            .count();
        assert_eq!(offered, 5, "one candidate per eligible entry: {actions:?}");
        let candidate = GameAction::SelectTargets {
            targets: vec![target.clone()],
        };
        assert!(actions.contains(&candidate), "{target:?} is offered");

        b.runner.act(candidate).expect("the candidate applies");
        assert_eq!(loyalty_counters(&b.runner, b.walker), 3, "reach-guard");
        match target {
            TargetRef::Object(id) => assert_eq!(marked_damage(&b.runner, id), 5),
            TargetRef::Player(player) => assert_eq!(life(&b.runner, player), 15),
        }
    }
}
