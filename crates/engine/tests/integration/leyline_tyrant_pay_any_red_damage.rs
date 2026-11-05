//! Leyline Tyrant — "When this creature dies, you may pay any amount of {R}.
//! When you do, it deals that much damage to any target."
//!
//! End to end through `apply()`: Vendetta (real card, verbatim Oracle text)
//! destroys P0's own Leyline Tyrant (red, hence a legal nonblack target); the
//! dies trigger offers the optional payment, the per-unit `{R}` prompt
//! (`PayableResource::Mana { composition: PerUnit }`) bounds the chosen amount
//! by the red mana actually payable, the payment spends that many red units,
//! and the "When you do" rider (CR 603.12) deals "that much" damage.
//!
//! Discriminating: before the parser leaf, "pay any amount of {R}" lowered to
//! `Effect::Unimplemented`, so no `PayAmountChoice` ever surfaced and no damage
//! was dealt.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ManaAmountComposition, PayableResource, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const LEYLINE_TYRANT: &str = "Flying\nYou don't lose unspent red mana as steps and phases end.\nWhen this creature dies, you may pay any amount of {R}. When you do, it deals that much damage to any target.";
const VENDETTA: &str = "Destroy target nonblack creature. It can't be regenerated. You lose life equal to that creature's toughness.";

/// P0 controls Leyline Tyrant ({2}{R}{R}) and holds Vendetta ({B}); P0's pool
/// is `pool` (CR 106.4: floating mana, spent by Vendetta's auto-payment and by
/// the resolution-time payment). Vendetta is committed to the stack targeting
/// the Tyrant.
fn cast_vendetta_on_own_tyrant(pool: &[ManaType]) -> GameRunner {
    let (scenario, tyrant, vendetta) = leyline_scenario(pool);
    commit_vendetta(scenario, tyrant, vendetta)
}

/// Builds the Leyline Tyrant / Vendetta scenario without building or casting,
/// so a test can add more permanents first. Returns `(scenario, tyrant,
/// vendetta)`.
fn leyline_scenario(pool: &[ManaType]) -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let tyrant = scenario
        .add_creature_from_oracle(P0, "Leyline Tyrant", 4, 4, LEYLINE_TYRANT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 2,
        })
        .id();
    let vendetta = scenario
        .add_spell_to_hand_from_oracle(P0, "Vendetta", true, VENDETTA)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool.iter()
            .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    (scenario, tyrant, vendetta)
}

/// Builds the scenario and commits Vendetta to the stack targeting the Tyrant.
fn commit_vendetta(scenario: GameScenario, tyrant: ObjectId, vendetta: ObjectId) -> GameRunner {
    let mut runner = scenario.build();
    runner.cast(vendetta).target_object(tyrant).commit();
    runner
}

fn pool_color(runner: &GameRunner, player: PlayerId, color: ManaType) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.count_color(color))
        .unwrap_or(0)
}

/// Pass priority until Vendetta resolves and the dies trigger reaches its
/// optional "you may" (answered with `accept`), then stop at the first
/// `PayAmountChoice` (`Some`) or once the stack is empty again (`None`).
fn drive_to_pay_prompt(runner: &mut GameRunner, accept: bool) -> Option<WaitingFor> {
    let mut decided = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0, "the optional payment belongs to the controller");
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("decide the optional payment");
                decided = true;
            }
            prompt @ WaitingFor::PayAmountChoice { .. } => return Some(prompt),
            WaitingFor::Priority { .. } if decided && runner.state().stack.is_empty() => {
                return None;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt before the payment: {other:?}"),
        }
    }
    panic!("the dies trigger never reached its payment decision");
}

/// After the payment: answer the "When you do" rider's target prompt with
/// `target` (returning the legal targets it offered, if it prompted) and pass
/// priority until the stack is empty. Every event emitted is appended to
/// `events`.
fn resolve_rider(
    runner: &mut GameRunner,
    target: TargetRef,
    events: &mut Vec<GameEvent>,
) -> Option<Vec<TargetRef>> {
    let mut offered = None;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { target_slots, .. }
            | WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                offered = Some(target_slots[0].legal_targets.clone());
                let result = runner
                    .act(GameAction::SelectTargets {
                        targets: vec![target.clone()],
                    })
                    .expect("choose the rider's target");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return offered,
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
            other => panic!("unexpected prompt after the payment: {other:?}"),
        }
    }
    panic!("the When-you-do rider never finished resolving");
}

fn red_unit() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

/// CR 107.1c + CR 603.12: accept, pay 2 of 3 red, and the rider deals that
/// much (2) damage to the chosen target.
#[test]
fn leyline_tyrant_pays_two_red_and_deals_two_damage() {
    let mut runner = cast_vendetta_on_own_tyrant(&[
        ManaType::Black,
        ManaType::Red,
        ManaType::Red,
        ManaType::Red,
    ]);
    let p1_life_before = runner.life(P1);

    let prompt = drive_to_pay_prompt(&mut runner, true).expect("the per-unit {R} prompt");
    // Reach-guard: Vendetta's {B} was paid from the pool, so every red unit is
    // still floating when the dies trigger resolves.
    assert_eq!(pool_color(&runner, P0, ManaType::Black), 0);
    assert_eq!(pool_color(&runner, P0, ManaType::Red), 3);
    let WaitingFor::PayAmountChoice {
        player,
        resource,
        min,
        max,
        accumulated,
        ..
    } = prompt
    else {
        unreachable!()
    };
    assert_eq!(player, P0);
    assert_eq!(
        resource,
        PayableResource::Mana {
            base_cost: red_unit(),
            composition: ManaAmountComposition::PerUnit,
        },
        "the prompt pays {{R}} per unit"
    );
    assert_eq!(min, 0, "CR 107.1c: zero is a legal amount");
    assert_eq!(max, 3, "three red units are payable");
    assert_eq!(accumulated, 0);

    let mut events = runner
        .act(GameAction::SubmitPayAmount { amount: 2 })
        .expect("pay {R}{R}")
        .events;
    assert_eq!(
        pool_color(&runner, P0, ManaType::Red),
        1,
        "two red units were spent"
    );

    let offered = resolve_rider(&mut runner, TargetRef::Player(P1), &mut events)
        .expect("the When-you-do rider chooses its target");
    assert!(
        offered.contains(&TargetRef::Player(P1)),
        "any target includes the opponent: {offered:?}"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "the rider deals that much (2) damage"
    );
}

/// The maximum counts only red mana actually payable: one red unit plus two
/// colorless units allow exactly one {R}.
#[test]
fn leyline_tyrant_max_counts_only_payable_red() {
    let mut runner = cast_vendetta_on_own_tyrant(&[
        ManaType::Black,
        ManaType::Red,
        ManaType::Colorless,
        ManaType::Colorless,
    ]);
    let prompt = drive_to_pay_prompt(&mut runner, true).expect("the per-unit {R} prompt");
    assert!(
        matches!(prompt, WaitingFor::PayAmountChoice { max: 1, .. }),
        "only the single red unit is payable: {prompt:?}"
    );
}

/// CR 107.1c + CR 120.8: paying zero spends no red and deals no damage.
#[test]
fn leyline_tyrant_pays_zero_and_deals_no_damage() {
    let mut runner = cast_vendetta_on_own_tyrant(&[ManaType::Black, ManaType::Red, ManaType::Red]);
    let p1_life_before = runner.life(P1);
    drive_to_pay_prompt(&mut runner, true).expect("the per-unit {R} prompt");

    let mut events = runner
        .act(GameAction::SubmitPayAmount { amount: 0 })
        .expect("pay zero")
        .events;
    // CR 603.12: the reflexive trigger is created by the resolving payment
    // instruction and still prompts for its "any target" on a zero payment.
    let offered = resolve_rider(&mut runner, TargetRef::Player(P1), &mut events)
        .expect("CR 603.12: the reflexive trigger still prompts for a target on a zero payment");
    assert!(
        offered.contains(&TargetRef::Player(P1)),
        "any target includes the opponent: {offered:?}"
    );

    assert_eq!(
        pool_color(&runner, P0, ManaType::Red),
        2,
        "no red was spent"
    );
    assert_eq!(runner.life(P1), p1_life_before, "no damage was dealt");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { amount, .. } if *amount > 0)),
        "CR 120.8: zero damage is no damage: {events:?}"
    );
}

/// Declining the optional payment never prompts for an amount and pays
/// nothing.
#[test]
fn leyline_tyrant_decline_pays_nothing() {
    let mut runner = cast_vendetta_on_own_tyrant(&[ManaType::Black, ManaType::Red, ManaType::Red]);
    let p1_life_before = runner.life(P1);
    assert_eq!(
        drive_to_pay_prompt(&mut runner, false),
        None,
        "a declined payment never prompts for an amount"
    );
    assert_eq!(pool_color(&runner, P0, ManaType::Red), 2);
    assert_eq!(runner.life(P1), p1_life_before);
}

const NYX_LOTUS: &str = "Nyx Lotus enters tapped.\n{T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color. (Your devotion to a color is the number of mana symbols of that color in the mana costs of permanents you control.)";
const FERTILE_GROUND: &str = "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional one mana of any color.";

/// Reach-guard shared by the empty-pool rows: Vendetta spent the only floating
/// {B}, so P0's pool is empty and the per-unit `{R}` prompt is up. Returns the
/// prompt's `max`.
fn empty_pool_prompt_max(runner: &mut GameRunner) -> u32 {
    let prompt = drive_to_pay_prompt(runner, true).expect("the per-unit {R} prompt");
    let pool_total = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map_or(0, |p| p.mana_pool.total());
    assert_eq!(pool_total, 0, "the pool is empty at the prompt");
    let WaitingFor::PayAmountChoice { player, max, .. } = prompt else {
        unreachable!("drive_to_pay_prompt returns only PayAmountChoice")
    };
    assert_eq!(player, P0);
    max
}

/// Adds Nyx Lotus (and, when `red_devotion` is true, Hellrider, printed
/// {2}{R}{R} 3/3) to the Leyline scenario with an otherwise empty pool.
/// Hellrider is only a devotion source on the battlefield: it is a plain body
/// with its printed cost, and none of its Oracle abilities (haste, attack
/// trigger) are exercised.
fn lotus_scenario(red_devotion: bool) -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let (mut scenario, tyrant, vendetta) = leyline_scenario(&[ManaType::Black]);
    let lotus = scenario
        .add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS)
        .id();
    if red_devotion {
        scenario
            .add_creature(P0, "Hellrider", 3, 3)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::Red],
                generic: 2,
            });
    }
    (scenario, tyrant, vendetta, lotus)
}

/// Builds the Mountain scenario, optionally enchanted with Fertile Ground,
/// attached before Vendetta is cast. Returns `(runner, mountain)`.
fn mountain_scenario(with_fertile_ground: bool) -> (GameRunner, ObjectId) {
    let (mut scenario, tyrant, vendetta) = leyline_scenario(&[ManaType::Black]);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let aura = with_fertile_ground.then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Fertile Ground", FERTILE_GROUND)
            .id()
    });
    let mut runner = scenario.build();
    if let Some(aura) = aura {
        let state = runner.state_mut();
        state.objects.get_mut(&aura).expect("the Aura").attached_to =
            Some(AttachTarget::Object(mountain));
        state
            .objects
            .get_mut(&mountain)
            .expect("the Mountain")
            .attachments
            .push(aura);
        state.layers_dirty.mark_full();
    }
    runner.cast(vendetta).target_object(tyrant).commit();
    (runner, mountain)
}

/// CR 118.1 + CR 605.3a: Nyx Lotus is a mana ability source that makes red
/// equal to the controller's devotion to red (Leyline Tyrant is in the
/// graveyard, so only Hellrider's {R}{R} counts). With an empty pool the
/// offered maximum is therefore 2, and paying 2 taps the Lotus for 2 damage.
#[test]
fn leyline_tyrant_offers_nyx_lotus_devotion_with_empty_pool() {
    let (scenario, tyrant, vendetta, lotus) = lotus_scenario(true);
    let mut runner = commit_vendetta(scenario, tyrant, vendetta);
    let p1_life_before = runner.life(P1);

    let max = empty_pool_prompt_max(&mut runner);
    assert!(
        !runner.state().objects[&lotus].tapped,
        "reach-guard: the builder places Nyx Lotus untapped"
    );
    assert_eq!(max, 2, "devotion to red is 2, payable from Nyx Lotus alone");

    let mut events = runner
        .act(GameAction::SubmitPayAmount { amount: 2 })
        .expect("pay {R}{R} from Nyx Lotus")
        .events;
    assert!(
        runner.state().objects[&lotus].tapped,
        "Nyx Lotus was tapped for the payment"
    );
    resolve_rider(&mut runner, TargetRef::Player(P1), &mut events);
    assert_eq!(runner.life(P1), p1_life_before - 2, "that much (2) damage");
}

/// Sibling of the Nyx Lotus row: with zero devotion to red the Lotus makes no
/// red, so the prompt still appears (CR 107.1c: zero is legal) with `max: 0`.
/// Paired positive: `leyline_tyrant_offers_nyx_lotus_devotion_with_empty_pool`.
#[test]
fn leyline_tyrant_nyx_lotus_without_red_devotion_offers_zero() {
    let (scenario, tyrant, vendetta, lotus) = lotus_scenario(false);
    let mut runner = commit_vendetta(scenario, tyrant, vendetta);

    let max = empty_pool_prompt_max(&mut runner);
    assert!(
        !runner.state().objects[&lotus].tapped,
        "reach-guard: Nyx Lotus is untapped and available"
    );
    assert_eq!(max, 0, "zero devotion to red makes no red mana");
}

/// CR 118.1 + CR 605.1b + CR 106.12a: Fertile Ground adds one additional mana
/// whenever the enchanted Mountain is tapped for mana, so the Mountain's {R}
/// plus the bonus mana make an offered maximum of 2 from an empty pool. The
/// bonus mana is of any color, so the exact affordability check admits 2 red.
#[test]
fn leyline_tyrant_offers_fertile_ground_bonus_with_empty_pool() {
    let (mut runner, mountain) = mountain_scenario(true);
    let p1_life_before = runner.life(P1);

    let max = empty_pool_prompt_max(&mut runner);
    assert!(
        !runner.state().objects[&mountain].tapped,
        "reach-guard: the Mountain is untapped at the prompt"
    );
    assert_eq!(max, 2, "Mountain {{R}} plus Fertile Ground's extra mana");

    let mut events = runner
        .act(GameAction::SubmitPayAmount { amount: 2 })
        .expect("pay {R}{R} from the enchanted Mountain")
        .events;
    assert!(
        runner.state().objects[&mountain].tapped,
        "the Mountain was tapped for the payment"
    );
    resolve_rider(&mut runner, TargetRef::Player(P1), &mut events);
    assert_eq!(runner.life(P1), p1_life_before - 2, "that much (2) damage");
}

/// Sibling of the Fertile Ground row: the same Mountain without the Aura offers
/// exactly 1. Paired positive:
/// `leyline_tyrant_offers_fertile_ground_bonus_with_empty_pool`. The base 1
/// comes from the Mountain's explicit `{T}: Add {R}` ability (both
/// `feasible_mana_capacity` and `max_mana_yield` report 1).
#[test]
fn leyline_tyrant_mountain_alone_offers_one() {
    let (mut runner, mountain) = mountain_scenario(false);

    let max = empty_pool_prompt_max(&mut runner);
    assert!(
        !runner.state().objects[&mountain].tapped,
        "reach-guard: the Mountain is untapped at the prompt"
    );
    assert_eq!(max, 1, "a lone Mountain makes one {{R}}");
}
