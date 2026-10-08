//! Runtime tests for issue #8380 — compound damage subjects whose two conjuncts
//! are a player scope and an object scope.
//!
//! The defect: in the PLAYER-FIRST ordering (`"to each player and each other
//! creature"`) the object conjunct was silently discarded, so Exocrine's
//! Bio-plasmic Barrage damaged players and left every creature untouched. The
//! object-first ordering of the same subject already produced the correct AST.
//!
//! CR 608.2f: "Some spells and abilities include actions taken on multiple
//! players and/or objects. In most cases, each such action is processed
//! simultaneously." A conjoined damage subject is ONE such action over both
//! audiences, which is why the correct representation is a single
//! `Effect::DamageAll` carrying both `target` and a non-null `player_filter`
//! rather than two chained effects.
//!
//! Every test here drives the real pipeline. Each names the assertion that flips
//! when the fix is reverted, and pairs it with a reach guard that holds in BOTH
//! states — so a test can never go green merely because the card failed to parse.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Verbatim Oracle text (Scryfall). A paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const EXOCRINE_ORACLE: &str = "Ravenous (This creature enters with X +1/+1 counters on it. \
If X is 5 or more, draw a card when it enters.)\n\
Bio-plasmic Barrage — When this creature enters, it deals X damage to each player and each other creature.";

/// Verbatim Oracle text for `Aurelia, the Law Above` (Scryfall).
///
/// Both attack triggers are present deliberately: the `GE 3` draw trigger is
/// what makes the library stocking below mandatory, and eliding it would build a
/// fixture the real card does not have.
const AURELIA_ORACLE: &str = "Flying, vigilance, haste\n\
Whenever a player attacks with three or more creatures, you draw a card.\n\
Whenever a player attacks with five or more creatures, Aurelia deals 3 damage to each of your opponents and you gain 3 life.";

/// Verbatim Oracle text for `Rupture` (Scryfall) — the amount-SUFFIX spelling.
const RUPTURE_ORACLE: &str = "Sacrifice a creature. Rupture deals damage equal to that creature's \
power to each creature without flying and each player.";

fn red_pool(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

/// Drive the game to an empty stack, resolving whatever the triggers put there.
///
/// BOUNDED deliberately: an unbounded loop whose `Priority` arm never empties the
/// stack produces a nextest TIMEOUT with no message, which is strictly worse than
/// a red. It is the bound itself — not the assertion after it — that removes that
/// risk. Precedent: `alania_divergent_storm.rs`, `birgi.rs`.
fn drive_to_empty_stack(runner: &mut GameRunner) {
    for _ in 0..128 {
        match runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                // `drain_order_triggers_with_identity` submits the APNAP ordering
                // and returns with the abilities ON THE STACK, unresolved — its
                // loop condition exits the instant the state stops being
                // `OrderTriggers`. Resolution happens below, by passing priority.
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    // What this catches: the loop left `waiting_for` in a NON-Priority state — an
    // unmodeled prompt via `_ => break`, or exhaustion while still ordering.
    // Exhaustion that ends in Priority with a non-empty stack passes THIS assert
    // and is caught by the stack guard below; the two together leave no silent path.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "drive loop hit its iteration bound"
    );
    // TWO reach guards, because these are DIFFERENT failures. Reporting them
    // identically would let an empty-library elimination masquerade as an
    // unresolved trigger.
    assert!(
        !runner.state().players.iter().any(|p| p.is_eliminated),
        "fixture killed a player before the trigger resolved (empty library + a draw \
         trigger, CR 704.5b) — stock the library rather than weakening the assertion"
    );
    assert!(
        runner.state().stack.is_empty(),
        "drive loop exited with the trigger unresolved; every assertion below would read 0"
    );
}

/// Read marked damage off the live state.
///
/// Deliberately the raw field (`GameObject::damage_marked`) rather than the
/// `Outcome::damage_marked` accessor: nothing in a `declare_attackers` flow
/// produces an `Outcome`, and a wrong reach there returns `0` — which IS the
/// broken-state expectation, so the instrument's failure mode would be
/// indistinguishable from the result it is meant to prove.
fn marked(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner.state().objects[&obj].damage_marked
}

fn life(runner: &GameRunner, player: engine::types::player::PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player must exist")
        .life
}

/// R1 — the reported defect, end to end.
///
/// Exocrine's ETB deals X damage to each player AND each other creature. At HEAD
/// the object conjunct was dropped, so only the players took damage.
#[test]
fn exocrine_damages_every_other_creature_and_every_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red_pool(8));

    // Bystanders are 5/5 so they survive 3 damage and the assertions read live
    // permanents rather than graveyard residue.
    let ally = scenario.add_creature(P0, "Ally Bear", 5, 5).id();
    let enemy = scenario.add_creature(P1, "Enemy Bear", 5, 5).id();

    // Verbatim Oracle text PLUS the explicit keyword name: `Ravenous`'s reminder
    // text parsed as plain text would lower to `Unimplemented` and the whole
    // fixture would be vacuous.
    //
    // The mana cost is Exocrine's real {X}{2}{R}, and the `X` shard is
    // LOAD-BEARING rather than decoration: `.x(3)` binds the announced value onto
    // the cast, and the trigger's amount is `CostXPaid`. Without an `X` in the
    // cost there is nothing for the announcement to bind to, `CostXPaid` resolves
    // to 0, and the whole test passes or fails on a zero-damage no-op.
    let exocrine = {
        let mut b = scenario.add_creature_to_hand(P0, "Exocrine", 2, 2);
        b.from_oracle_text_with_keywords(&["Ravenous"], EXOCRINE_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 2,
        });
        b.id()
    };

    let mut runner = scenario.build();
    let outcome = runner.cast(exocrine).x(3).resolve();

    // REACH GUARD (positive control — holds BOTH before and after the fix).
    // Proves the card parsed, the ETB trigger fired, and the damage effect
    // actually resolved, so the creature assertions below cannot pass vacuously
    // on a card that simply failed to parse.
    // CR 120.3a: damage dealt to a player by a source without infect causes that
    // player to lose that much life. Exocrine has no infect.
    outcome.assert_life_delta(P0, -3);
    outcome.assert_life_delta(P1, -3);

    // DISCRIMINATING — 0 at HEAD, 3 with the fix.
    // CR 120.3e: damage dealt to a creature by a source with neither wither nor
    // infect causes that much damage to be marked on that creature.
    assert_eq!(
        outcome.damage_marked(ally),
        3,
        "'each other creature' must include the controller's own creature"
    );
    assert_eq!(
        outcome.damage_marked(enemy),
        3,
        "'each other creature' must include the opponent's creature"
    );

    // SECOND DISCRIMINATING AXIS — "each OTHER creature" excludes the source.
    // Fails if a naive fix drops the `other` (FilterProp::Another).
    assert_eq!(
        outcome.damage_marked(exocrine),
        0,
        "'each other creature' must exclude Exocrine itself"
    );
}

/// R12 — Unit 4: `each of your opponents` names a PLAYER SCOPE, not an object
/// filter that happens to be empty.
///
/// At HEAD the partitive spelling matched no arm of the player-scope grammar, so
/// the recipient fell through to `parse_target` and produced
/// `Typed{type_filters: []}`. An empty type-filter list is not "matches nothing":
/// `filter.rs`'s `for tf in type_filters` loop is a no-op on an empty vec, so the
/// filter matches EVERY permanent — while `player_filter: None` damages nobody.
/// Aurelia therefore dealt 3 damage to every permanent in play and to no player,
/// the precise inverse of her Oracle text.
///
/// This is asserted at RUNTIME, not on the AST, because an AST-shape assertion
/// cannot distinguish "matches nothing" from "matches everything" — which is
/// exactly the ambiguity an empty filter creates.
#[test]
fn aurelia_partitive_player_scope_damages_opponents_not_permanents() {
    let mut scenario = GameScenario::new();
    // MANDATORY: `GameRunner::act` auto-passes the CR 507.2 priority window
    // before submitting a declaration only when three conditions hold
    // CONJUNCTIVELY — phase == BeginCombat, waiting_for is Priority, and the
    // stack is empty. `advance_to_combat()` is NOT a substitute: it targets
    // Phase::DeclareAttackers and misses the BeginCombat conjunct.
    scenario.at_phase(Phase::BeginCombat);

    // MANDATORY: a `GameScenario` player's library is EMPTY by default. Aurelia's
    // OTHER attack trigger ("three or more creatures → you draw a card") fires
    // under this same setup, and drawing from an empty library eliminates P0 at
    // the next state-based check (CR 704.5b), ending the game with the damage
    // trigger still on the stack. The attacker count cannot separate the two
    // triggers — `GE 5` implies `GE 3` — so the library must be stocked instead.
    scenario.with_library_top(
        P0,
        &[
            "Filler A", "Filler B", "Filler C", "Filler D", "Filler E", "Filler F",
        ],
    );

    scenario
        .add_creature_from_oracle(P0, "Aurelia, the Law Above", 4, 4, AURELIA_ORACLE)
        .id();

    // Five attackers: `GE 5` is what arms the ability under test.
    let attackers: Vec<ObjectId> = (0..5)
        .map(|i| {
            scenario
                .add_creature(P0, &format!("Attacker {i}"), 1, 1)
                .id()
        })
        .collect();

    // Bystanders on BOTH sides, 5/5 so they survive and stay readable. These are
    // the permanents the broken empty filter would damage.
    let own_bystander = scenario.add_creature(P0, "Own Bystander", 5, 5).id();
    let foe_bystander = scenario.add_creature(P1, "Foe Bystander", 5, 5).id();

    let mut runner = scenario.build();

    // Snapshot ALL FOUR measured quantities at the SAME point, before declaring.
    // A baseline taken after resolution measures nothing.
    let p0_life_before = life(&runner, P0);
    let p1_life_before = life(&runner, P1);
    let own_marked_before = marked(&runner, own_bystander);
    let foe_marked_before = marked(&runner, foe_bystander);
    let p0_hand_before = runner.state().players[0].hand.len();

    let attacks: Vec<(ObjectId, engine::game::combat::AttackTarget)> = attackers
        .iter()
        .map(|id| (*id, engine::game::combat::AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("five 1/1s must be able to attack");

    drive_to_empty_stack(&mut runner);

    // CONTROL (holds in BOTH states, and is upstream of the parser change):
    // the draw trigger fired and resolved. Doubles as evidence that BOTH
    // triggers reached resolution, not just one.
    assert_eq!(
        runner.state().players[0].hand.len(),
        p0_hand_before + 1,
        "the 'three or more creatures' draw trigger must have resolved"
    );

    // CONTROL: the GainLife sub_ability resolves for the controller in both
    // states, so a green here is not evidence of the fix — it is evidence the
    // damage trigger itself resolved.
    assert_eq!(
        life(&runner, P0) - p0_life_before,
        3,
        "'you gain 3 life' resolves for the controller in both states"
    );

    // DISCRIMINATOR 1 — the opponent must lose 3 life. At HEAD `player_filter`
    // was absent, so this delta was 0.
    assert_eq!(
        life(&runner, P1) - p1_life_before,
        -3,
        "'each of your opponents' must actually damage the opponent"
    );

    // DISCRIMINATOR 2 — no permanent may be damaged. At HEAD the empty type
    // filter matched every permanent on the battlefield, so both of these were 3.
    assert_eq!(
        marked(&runner, own_bystander) - own_marked_before,
        0,
        "a player-scoped damage clause must not damage permanents"
    );
    assert_eq!(
        marked(&runner, foe_bystander) - foe_marked_before,
        0,
        "a player-scoped damage clause must not damage permanents"
    );
}

/// R11 — Unit 3: the amount-SUFFIX spelling keeps its player half.
///
/// `"deals damage equal to <expr> to each creature without flying and each
/// player"` made BOTH compound parsers decline at HEAD (they hardcoded the
/// prefix amount spelling), and the fall-through lift then dropped the player
/// half because the trailing `without flying` made `parse_target` consume past
/// the connector. The creature half worked; the players took nothing.
#[test]
fn rupture_amount_suffix_keeps_both_damage_audiences() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red_pool(8));

    // The sacrificial creature supplies the damage amount ("that creature's
    // power"), so its power is the measured quantity: 3.
    scenario.add_creature(P0, "Sacrificial Ox", 3, 3).id();
    // 5/5 non-flying so it is a legal recipient AND survives 3 damage.
    let bystander = scenario.add_creature(P1, "Grounded Bystander", 5, 5).id();

    let rupture = {
        let mut b = scenario.add_creature_to_hand(P0, "Rupture", 0, 0);
        b.from_oracle_text(RUPTURE_ORACLE).as_sorcery();
        b.id()
    };

    let mut runner = scenario.build();
    let outcome = runner.cast(rupture).resolve();

    // REACH GUARD (holds in both states): the creature half already worked at
    // HEAD, so this proves the spell parsed and resolved and the amount bound.
    assert_eq!(
        outcome.damage_marked(bystander),
        3,
        "the object half of the subject already worked; if this is 0 the fixture \
         never reached the effect and the life assertions below are meaningless"
    );

    // DISCRIMINATING — 0 at HEAD (player_filter absent), -3 with the fix.
    outcome.assert_life_delta(P0, -3);
    outcome.assert_life_delta(P1, -3);
}
