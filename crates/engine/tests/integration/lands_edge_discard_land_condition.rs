//! CR 602.1 + CR 602.2 + CR 118.1 + CR 608.2c + CR 608.2k + CR 400.7j:
//! Land's Edge — "Discard a card: If the discarded card was a land card, this
//! enchantment deals 2 damage to target player or planeswalker. Any player may
//! activate this ability."
//!
//! This is the first end-to-end exercise of the bare (non-`instead`)
//! `AbilityCondition::CostPaidObjectMatchesFilter` composition on a plain
//! `Discard` cost, combined with a `PlayerFilter::All` ("any player may
//! activate") activation instruction. The building block was added for Agency
//! Coroner / Surtland Flinger / Stormscale Anarch / Grab the Prize, but this
//! exact shape — a *type* predicate (`[Card, Land]`) checked against the
//! discard-cost LKI snapshot to gate the *whole* effect (not a "…instead"
//! override) — had zero runtime coverage.
//!
//! Discriminating design (revert-failing assertions called out per test):
//!   - Discard a LAND  → condition true  → 2 damage (life delta -2).
//!   - Discard a NONLAND → condition false → 0 damage (life unchanged), yet the
//!     ability still resolves (reach-guard: the discarded card lands in the
//!     graveyard). If the condition were dropped, the nonland case would ALSO
//!     deal 2, since an unconditioned ability always applies its effect.
//!   - The snapshot binds the *specifically chosen* discarded object, not "any
//!     land present in hand at cost-payment time" (hostile land+nonland hand).
//!   - `activator_filter = All` lets a non-controller (P1) activate an
//!     enchantment P0 controls, paying the discard from P1's OWN hand
//!     (CR 602.1a: the activating player pays the cost). The control-only
//!     variant (no "any player may activate") rejects that non-controller
//!     activation.
//!
//! CR 602.1a verified: docs/MagicCompRules.txt:2516 ("An ability's activation
//! cost must be paid by the player who is activating it.").

use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::{ObjectId, Zone};

const LANDS_EDGE: &str = "Discard a card: If the discarded card was a land card, \
     this enchantment deals 2 damage to target player or planeswalker. Any player \
     may activate this ability.";

/// Same ability WITHOUT the "Any player may activate this ability." instruction:
/// controller-only activation per CR 602.2.
const CONTROL_ONLY_EDGE: &str = "Discard a card: If the discarded card was a land \
     card, this enchantment deals 2 damage to target player or planeswalker.";

/// Build Land's Edge as a proper Enchantment on `controller`'s battlefield.
/// `.as_enchantment()` runs BEFORE `from_oracle_text` so the object's types
/// carry Enchantment when the Oracle text is parsed (matching the real card's
/// "this enchantment" self-reference context).
fn add_lands_edge(scenario: &mut GameScenario, controller: PlayerId, text: &str) -> ObjectId {
    scenario
        .add_creature(controller, "Land's Edge", 0, 0)
        .as_enchantment()
        .from_oracle_text(text)
        .id()
}

fn add_land_in_hand(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_creature_to_hand(player, name, 0, 0)
        .as_land()
        .id()
}

fn add_nonland_in_hand(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    // A plain creature card — a card that is NOT a land.
    scenario.add_creature_to_hand(player, name, 2, 2).id()
}

fn life(runner: &GameRunner, p: PlayerId) -> i32 {
    runner.state().players[p.0 as usize].life
}

fn hand_len(runner: &GameRunner, p: PlayerId) -> usize {
    runner.state().players[p.0 as usize].hand.len()
}

/// Drive one Land's Edge activation to stack resolution. Handles the target and
/// discard-cost windows in whichever order the pipeline surfaces them (targets
/// are announced at activation; the cost is paid during announcement), then
/// passes priority until the ability resolves and leaves the stack.
///
/// `activator` pays the discard and announces the target explicitly (so a
/// non-controller can drive their own activation); priority passes route through
/// `apply_as_current` for whichever player holds priority.
fn drive_to_resolution(
    runner: &mut GameRunner,
    activator: PlayerId,
    discard: ObjectId,
    target: TargetRef,
) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                apply(
                    runner.state_mut(),
                    activator,
                    GameAction::SelectTargets {
                        targets: vec![target.clone()],
                    },
                )
                .expect("selecting the player/planeswalker target must succeed");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                player,
                ..
            } => {
                // CR 602.1a: the discard cost is prompted to the activating player.
                assert_eq!(
                    player, activator,
                    "the discard cost must be paid by the activating player"
                );
                apply(
                    runner.state_mut(),
                    activator,
                    GameAction::SelectCards {
                        cards: vec![discard],
                    },
                )
                .expect("paying the discard cost must succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize (empty) mana payment");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the ability");
            }
            other => panic!("unexpected window driving Land's Edge: {other:?}"),
        }
    }
}

/// Positive: discarding a LAND satisfies the `[Card, Land]` condition and deals
/// 2 damage to the targeted player. Revert-fail: dropping the condition or the
/// Land predicate keeps this at -2 but flips the nonland sibling below.
#[test]
fn discarding_a_land_deals_two_damage_to_target_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let edge = add_lands_edge(&mut scenario, P0, LANDS_EDGE);
    let land = add_land_in_hand(&mut scenario, P0, "Mountain");

    let mut runner = scenario.build();
    let target_life_before = life(&runner, P1);

    apply(
        runner.state_mut(),
        P0,
        GameAction::ActivateAbility {
            source_id: edge,
            ability_index: 0,
        },
    )
    .expect("P0 activates Land's Edge");
    drive_to_resolution(&mut runner, P0, land, TargetRef::Player(P1));

    // Reach-guard: the discarded land actually left the hand (cost paid, ability
    // resolved) — so the -2 below is not a vacuous "nothing happened".
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "the discarded land must be in the graveyard"
    );
    assert_eq!(
        life(&runner, P1),
        target_life_before - 2,
        "discarding a land must deal 2 damage to the targeted player"
    );
}

/// Discriminating negative: discarding a NONLAND fails the `[Card, Land]`
/// condition, so ZERO damage is dealt — even though the ability still resolves.
/// Revert-fail: if the condition were dropped, this would also deal 2. The
/// reach-guard (nonland in graveyard) proves the ability got past cost payment
/// and resolution, so `life unchanged` is a real negative, not a short-circuit.
#[test]
fn discarding_a_nonland_deals_no_damage_but_still_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let edge = add_lands_edge(&mut scenario, P0, LANDS_EDGE);
    let nonland = add_nonland_in_hand(&mut scenario, P0, "Grizzly Bears");

    let mut runner = scenario.build();
    let target_life_before = life(&runner, P1);

    apply(
        runner.state_mut(),
        P0,
        GameAction::ActivateAbility {
            source_id: edge,
            ability_index: 0,
        },
    )
    .expect("P0 activates Land's Edge");
    drive_to_resolution(&mut runner, P0, nonland, TargetRef::Player(P1));

    // Reach-guard: the ability resolved (the nonland was discarded), so the
    // condition was genuinely evaluated and found false.
    assert_eq!(
        runner.state().objects[&nonland].zone,
        Zone::Graveyard,
        "the discarded nonland must be in the graveyard (ability resolved)"
    );
    assert_eq!(
        life(&runner, P1),
        target_life_before,
        "discarding a nonland must deal 0 damage (condition false)"
    );
}

/// CR 608.2k: the condition binds the *specifically chosen* discarded object's
/// snapshot, not "a land is present in hand". Hostile hand = {land, nonland}:
///   - choosing the LAND → 2 damage.
///   - choosing the NONLAND (with a land STILL in hand) → 0 damage.
///
/// If the check scanned the hand for any land instead of reading the chosen
/// discard's snapshot, the nonland sub-case would wrongly deal 2.
#[test]
fn chosen_discard_binds_snapshot_not_any_land_in_hand() {
    // Sub-case A: choose the land → damage.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let edge = add_lands_edge(&mut scenario, P0, LANDS_EDGE);
        let land = add_land_in_hand(&mut scenario, P0, "Island");
        let _nonland = add_nonland_in_hand(&mut scenario, P0, "Hill Giant");

        let mut runner = scenario.build();
        let before = life(&runner, P1);
        apply(
            runner.state_mut(),
            P0,
            GameAction::ActivateAbility {
                source_id: edge,
                ability_index: 0,
            },
        )
        .expect("activate");
        drive_to_resolution(&mut runner, P0, land, TargetRef::Player(P1));
        assert_eq!(
            life(&runner, P1),
            before - 2,
            "choosing the land to discard must deal 2 damage"
        );
    }

    // Sub-case B: choose the nonland WHILE a land remains in hand → no damage.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let edge = add_lands_edge(&mut scenario, P0, LANDS_EDGE);
        let land = add_land_in_hand(&mut scenario, P0, "Island");
        let nonland = add_nonland_in_hand(&mut scenario, P0, "Hill Giant");

        let mut runner = scenario.build();
        let before = life(&runner, P1);
        apply(
            runner.state_mut(),
            P0,
            GameAction::ActivateAbility {
                source_id: edge,
                ability_index: 0,
            },
        )
        .expect("activate");
        drive_to_resolution(&mut runner, P0, nonland, TargetRef::Player(P1));
        // Reach-guard: nonland discarded, land untouched in hand.
        assert_eq!(
            runner.state().objects[&nonland].zone,
            Zone::Graveyard,
            "the chosen nonland must be discarded"
        );
        assert_eq!(
            runner.state().objects[&land].zone,
            Zone::Hand,
            "the untouched land must remain in hand"
        );
        assert_eq!(
            life(&runner, P1),
            before,
            "choosing a nonland must deal 0 damage even with a land still in hand"
        );
    }
}

/// CR 602.2a + CR 602.1a: `activator_filter = All` lets P1 (a non-controller)
/// activate the enchantment P0 controls, and P1 pays the discard from P1's OWN
/// hand. Revert-fail: without the "Any player may activate" clause the initial
/// activation would be rejected (see the sibling negative below).
#[test]
fn any_player_may_activate_lets_non_controller_pay_from_own_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 controls the enchantment; P0 also holds cards that must NOT be spent.
    let edge = add_lands_edge(&mut scenario, P0, LANDS_EDGE);
    scenario.with_cards_in_hand(P0, &["P0-Keep-A", "P0-Keep-B"]);
    // P1 (the non-controller activator) holds the land they will discard.
    let p1_land = add_land_in_hand(&mut scenario, P1, "Swamp");

    let mut runner = scenario.build();
    let p0_life_before = life(&runner, P0);
    let p0_hand_before = hand_len(&runner, P0);

    // CR 117.3d: a player can only activate an ability when they hold priority.
    // At PreCombatMain with an empty stack, the active player (P0) passing
    // priority hands it to P1 (the non-active player) before the step ends.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    assert_eq!(
        runner.state().priority_player,
        P1,
        "P1 (the non-controller) must now hold priority"
    );

    // Explicit actor: P1 activates P0's enchantment. This ONLY succeeds because
    // activator_filter = All (CR 602.2a) — the control-only sibling test proves
    // the same P1-with-priority attempt is rejected without that clause.
    apply(
        runner.state_mut(),
        P1,
        GameAction::ActivateAbility {
            source_id: edge,
            ability_index: 0,
        },
    )
    .expect("P1 (non-controller) may activate because activator_filter = All");
    // P1 targets P0 (the controller) with the damage.
    drive_to_resolution(&mut runner, P1, p1_land, TargetRef::Player(P0));

    assert_eq!(
        runner.state().objects[&p1_land].zone,
        Zone::Graveyard,
        "P1's land must be discarded from P1's own hand"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "the controller's hand must be untouched — the activator (P1) pays the cost"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life_before - 2,
        "P1's activation must deal 2 damage to the targeted player (P0)"
    );
}

/// Sibling negative to the above: the control-only variant (no "Any player may
/// activate this ability.") must REJECT a non-controller's activation attempt
/// (CR 602.2 — only the controller may activate). Proves the permission in the
/// positive test is driven by the "any player" clause, not a default.
#[test]
fn control_only_variant_rejects_non_controller_activation() {
    // Negative: P1 HOLDS PRIORITY but still cannot activate — the ONLY thing
    // stopping them is activator_filter (controller-only), not a lack of
    // priority. This is what makes the rejection discriminate the "any player"
    // clause rather than being a vacuous no-priority WrongPlayer.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let edge = add_lands_edge(&mut scenario, P0, CONTROL_ONLY_EDGE);
        // Give both players a discardable land so the rejection is a permission
        // gate, not "no card to discard".
        let _p0_land = add_land_in_hand(&mut scenario, P0, "Forest");
        let _p1_land = add_land_in_hand(&mut scenario, P1, "Swamp");

        let mut runner = scenario.build();
        // Hand priority to P1 so the actor-authorization gate passes; the
        // remaining barrier is purely the missing "any player may activate".
        runner
            .act(GameAction::PassPriority)
            .expect("P0 passes priority to P1");
        assert_eq!(
            runner.state().priority_player,
            P1,
            "P1 must hold priority so the rejection is the activator gate, not WrongPlayer"
        );

        let result = apply(
            runner.state_mut(),
            P1,
            GameAction::ActivateAbility {
                source_id: edge,
                ability_index: 0,
            },
        );
        assert!(
            result.is_err(),
            "a non-controller (even holding priority) must not activate the control-only \
             variant, got {result:?}"
        );
    }

    // Positive control (reach-guard): the SAME ability IS activatable by its
    // controller (P0), so the rejection above is a permission gate on the
    // non-controller, not a broken/unparsed ability.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let edge = add_lands_edge(&mut scenario, P0, CONTROL_ONLY_EDGE);
        let _p0_land = add_land_in_hand(&mut scenario, P0, "Forest");

        let mut runner = scenario.build();
        // P0 holds priority at PreCombatMain and controls the enchantment.
        apply(
            runner.state_mut(),
            P0,
            GameAction::ActivateAbility {
                source_id: edge,
                ability_index: 0,
            },
        )
        .expect("the controller (P0) may activate the control-only variant");
    }
}
