//! phase-rs/phase#9812: a targeted Auto cast payable only with the help of a
//! mana ability the Auto payer doesn't activate (Treasonous Ogre's "Pay 3
//! life: Add {R}", Skirge Familiar's "Discard a card: Add {B}", a Citanul
//! Stalwart's helper tap) is offered exactly when the real reducer can carry
//! the announced cast to completion.
//!
//! The offer seam (`ai_support::filter`, after
//! `post_origin_auto_payment_verdict` refuses) consults
//! `witness_targeted_cast_offer`: a bounded search that drives the real
//! reducer from the announcement through targets, mana abilities and their
//! choices, and payment. No witness keeps the refusal.
//!
//! Printed cards use Oracle text verbatim from Scryfall `cards/named`.
//! Synthetic sources carry a declaration: no printed counterpart, the nearest
//! printed card, and why the fixture is needed.
//!
//! CR 601.2c: targets are announced before the total cost is paid.
//! CR 601.2g + CR 605.3a: mana abilities are activated during casting.
//! CR 601.2h + CR 118.3: the total cost is paid only with the resources for it.
//! CR 601.2i: the spell becomes cast.

use engine::ai_support::{
    candidate_actions, legal_actions, witness_targeted_cast_offer, TargetedCastVerdict,
    PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS, TARGETED_CAST_OFFER_MAX_REDUCER_ATTEMPTS,
};
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, FilterProp,
    ManaContribution, ManaProduction, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
    TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{FlashbackCost, Keyword};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const NYX_LOTUS: &str = "Nyx Lotus enters tapped.\n{T}: Choose a color. Add an amount of mana of that color equal to your devotion to that color. (Your devotion to a color is the number of mana symbols of that color in the mana costs of permanents you control.)";
const TREASONOUS_OGRE: &str = "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\nPay 3 life: Add {R}.";
const SKIRGE_FAMILIAR: &str = "Flying\nDiscard a card: Add {B}.";
const CITANUL_STALWART: &str =
    "{T}, Tap an untapped artifact or creature you control: Add one mana of any color.";
const UNSUBSTANTIATE: &str = "Return target spell or creature to its owner's hand.";
const WORDS_OF_WISDOM: &str = "You draw two cards, then each other player draws a card.";
const INSPIRATION: &str = "Target player draws two cards.";
const DISFIGURE: &str = "Target creature gets -2/-2 until end of turn.";
const VICTIM_OF_NIGHT: &str = "Destroy target non-Vampire, non-Werewolf, non-Zombie creature.";
const ARC_TRAIL: &str = "Arc Trail deals 2 damage to any target and 1 damage to any other target.";
const FIREBOLT: &str = "Firebolt deals 2 damage to any target.";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn auto_cast(runner: &GameRunner, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn activate(source_id: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    }
}

fn offered(runner: &GameRunner, action: &GameAction) -> bool {
    legal_actions(runner.state()).contains(action)
}

fn generated(runner: &GameRunner, action: &GameAction) -> bool {
    candidate_actions(runner.state())
        .iter()
        .any(|candidate| candidate.action == *action)
}

/// The witness's own verdict on P0's Auto cast of `spell` at `budget`.
fn verdict(runner: &GameRunner, spell: ObjectId, budget: usize) -> TargetedCastVerdict {
    witness_targeted_cast_offer(runner.state(), P0, P0, &auto_cast(runner, spell), budget).verdict
}

/// Announce `action`, require the target-selection stop (the post-target
/// offer seam), and choose `targets`.
fn announce_and_target(
    runner: &mut GameRunner,
    action: GameAction,
    targets: Vec<TargetRef>,
) -> Result<(), String> {
    runner.act(action).expect("the cast is announced");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach: the targeted cast stops at target selection: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets { targets })
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// Announce and target, then require the mana-payment step: the reducer's
/// route for an Auto cast the Auto payer can't finish.
fn announce_to_payment(runner: &mut GameRunner, action: GameAction, targets: Vec<TargetRef>) {
    announce_and_target(runner, action, targets).expect("the targets are accepted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at the mana-payment step: {:?}",
        runner.state().waiting_for
    );
}

/// Activate a mana ability during payment and answer its prompts: a cost
/// choice (discard, exile, tap a helper) with the first card `pick` accepts,
/// and a colour choice with `color`.
fn activate_paying(
    runner: &mut GameRunner,
    source: ObjectId,
    pick: impl Fn(&ObjectId) -> bool,
    color: ManaType,
) -> Result<(), String> {
    runner.act(activate(source)).map_err(|e| format!("{e:?}"))?;
    for _ in 0..4 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                let card = *choices
                    .iter()
                    .find(|id| pick(id))
                    .ok_or_else(|| format!("no acceptable choice in {choices:?}"))?;
                runner
                    .act(GameAction::SelectCards { cards: vec![card] })
                    .map_err(|e| format!("{e:?}"))?;
            }
            WaitingFor::ChooseManaColor { .. } => {
                runner
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(color),
                        count: 1,
                    })
                    .map_err(|e| format!("{e:?}"))?;
            }
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// CR 601.2h + CR 601.2i: finish the payment, require the spell to be cast
/// (on the stack, priority back), then let it resolve.
fn pay_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    runner
        .act(GameAction::PassPriority)
        .expect("passing pays the total cost");
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == spell),
        "CR 601.2i: the spell became cast"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast is complete: {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
}

/// The castability gate generates the cast, and the offer refuses it.
fn assert_generated_not_offered(runner: &GameRunner, action: &GameAction) {
    assert!(
        generated(runner, action),
        "reach: the castability gate generates the cast"
    );
    assert!(!offered(runner, action), "the cast can't be completed");
}

// ---------------------------------------------------------------------------
// The issue's boards
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlueSource {
    /// Nyx Lotus at blue devotion 1 (Merfolk of the Pearl Trident, {U}).
    NyxLotus,
    /// A basic Island.
    Island,
    /// {U}{C} already floating (control: no manual mana needed).
    Floating,
}

struct OgreBoard {
    runner: GameRunner,
    blue_source: Option<ObjectId>,
    ogre: ObjectId,
    bear: ObjectId,
    unsubstantiate: ObjectId,
    words: ObjectId,
    inspiration: ObjectId,
}

/// P0 in its main phase at 20 life: Treasonous Ogre, Merfolk of the Pearl
/// Trident ({U}) and the blue source; P1 has Grizzly Bears.
fn ogre_board(blue: BlueSource) -> OgreBoard {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let blue_source = match blue {
        BlueSource::NyxLotus => {
            let mut b = sc.add_artifact_from_oracle(P0, "Nyx Lotus", NYX_LOTUS);
            b.as_legendary().with_mana_cost(cost(vec![], 4));
            Some(b.id())
        }
        BlueSource::Island => Some(sc.add_basic_land(P0, ManaColor::Blue)),
        BlueSource::Floating => {
            sc.with_mana_pool(
                P0,
                vec![
                    ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
                    ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ],
            );
            None
        }
    };
    sc.add_creature(P0, "Merfolk of the Pearl Trident", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 0));
    let ogre = sc
        .add_creature(P0, "Treasonous Ogre", 2, 3)
        .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
        .id();
    let bear = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    sc.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    let unsubstantiate = sc
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    let words = sc
        .add_spell_to_hand_from_oracle(P0, "Words of Wisdom", true, WORDS_OF_WISDOM)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    let inspiration = sc
        .add_spell_to_hand_from_oracle(P0, "Inspiration", true, INSPIRATION)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 3))
        .id();
    OgreBoard {
        runner: sc.build(),
        blue_source,
        ogre,
        bear,
        unsubstantiate,
        words,
        inspiration,
    }
}

/// Island U + the Ogre's R pays the targeted {1}{U}: offered, and the offered
/// Auto action completes and resolves.
#[test]
fn issue_board_island_targeted_cast_is_offered_and_completes() {
    let mut b = ogre_board(BlueSource::Island);
    let words = auto_cast(&b.runner, b.words);
    assert!(offered(&b.runner, &words), "reach: the untargeted twin");
    let action = auto_cast(&b.runner, b.unsubstantiate);
    assert!(
        offered(&b.runner, &action),
        "CR 601.2g + CR 605.1a: Island U + Ogre R pays the targeted {{1}}{{U}}"
    );
    announce_to_payment(&mut b.runner, action, vec![TargetRef::Object(b.bear)]);
    activate_paying(&mut b.runner, b.ogre, |_| true, ManaType::Red).expect("the Ogre pays");
    pay_and_resolve(&mut b.runner, b.unsubstantiate);
    let s = b.runner.state();
    assert_eq!(s.players[0].life, 17, "the Ogre paid 3 life");
    assert!(
        s.objects[&b.blue_source.unwrap()].tapped,
        "the Island paid {{U}}"
    );
    assert_eq!(
        s.objects[&b.bear].zone,
        Zone::Hand,
        "Unsubstantiate resolved"
    );
    assert_eq!(s.objects[&b.unsubstantiate].zone, Zone::Graveyard);
}

/// The issue's Nyx Lotus board (blue devotion 1, so the Lotus adds exactly U).
#[test]
fn issue_board_nyx_lotus_targeted_cast_is_offered_and_completes() {
    let mut b = ogre_board(BlueSource::NyxLotus);
    assert_eq!(
        engine::game::devotion::count_devotion(b.runner.state(), P0, &[ManaColor::Blue]),
        1,
        "reach: blue devotion 1"
    );
    let words = auto_cast(&b.runner, b.words);
    assert!(offered(&b.runner, &words), "reach: the untargeted twin");
    let action = auto_cast(&b.runner, b.unsubstantiate);
    assert!(offered(&b.runner, &action));
    announce_to_payment(&mut b.runner, action, vec![TargetRef::Object(b.bear)]);
    activate_paying(&mut b.runner, b.ogre, |_| true, ManaType::Red).expect("the Ogre pays");
    pay_and_resolve(&mut b.runner, b.unsubstantiate);
    let s = b.runner.state();
    assert_eq!(s.players[0].life, 17);
    assert!(
        s.objects[&b.blue_source.unwrap()].tapped,
        "the Lotus paid {{U}}"
    );
    assert_eq!(s.objects[&b.bear].zone, Zone::Hand);
    assert_eq!(s.objects[&b.unsubstantiate].zone, Zone::Graveyard);
}

/// Controls: with {U}{C} floating both are offered at base already; and
/// Inspiration ({3}{U}, three Ogre activations) stays with the castability
/// gate, which credits one activation per permanent (#1234).
#[test]
fn controls_floating_mana_and_inspiration() {
    let floating = ogre_board(BlueSource::Floating);
    assert!(offered(
        &floating.runner,
        &auto_cast(&floating.runner, floating.unsubstantiate)
    ));
    let nyx = ogre_board(BlueSource::NyxLotus);
    let inspiration = auto_cast(&nyx.runner, nyx.inspiration);
    assert!(!generated(&nyx.runner, &inspiration));
    assert!(!offered(&nyx.runner, &inspiration));
}

struct SkirgeBoard {
    runner: GameRunner,
    disfigure: ObjectId,
    skirge: ObjectId,
    bear: ObjectId,
    others: Vec<ObjectId>,
}

fn other_cards(sc: &mut GameScenario, n: usize) -> Vec<ObjectId> {
    (0..n)
        .map(|i| {
            sc.add_spell_to_hand_from_oracle(P0, &format!("Other Card {i}"), true, "Draw a card.")
                .with_mana_cost(cost(vec![ManaCostShard::Red, ManaCostShard::Red], 5))
                .id()
        })
        .collect()
}

fn skirge(sc: &mut GameScenario, text: &str) -> ObjectId {
    sc.add_creature(P0, "Skirge Familiar", 3, 2)
        .from_oracle_text_with_keywords(&["Flying"], text)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 4))
        .id()
}

/// P0: Skirge Familiar, no lands, Disfigure and `others` other cards in hand;
/// P1: Grizzly Bears.
fn skirge_board(others: usize) -> SkirgeBoard {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let skirge = skirge(&mut sc, SKIRGE_FAMILIAR);
    let bear = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    sc.with_library_top(P0, &["Card A", "Card B"]);
    let disfigure = sc
        .add_spell_to_hand_from_oracle(P0, "Disfigure", true, DISFIGURE)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 0))
        .id();
    let others = other_cards(&mut sc, others);
    SkirgeBoard {
        runner: sc.build(),
        disfigure,
        skirge,
        bear,
        others,
    }
}

/// The issue's Skirge Familiar board: one other card in hand pays Disfigure's
/// {B}. Offered; the offered action completes by discarding that card.
#[test]
fn issue_board_skirge_familiar_targeted_cast_is_offered_and_completes() {
    let mut b = skirge_board(1);
    let action = auto_cast(&b.runner, b.disfigure);
    assert!(offered(&b.runner, &action));
    announce_to_payment(&mut b.runner, action, vec![TargetRef::Object(b.bear)]);
    let other = b.others[0];
    activate_paying(&mut b.runner, b.skirge, |id| *id == other, ManaType::Black)
        .expect("Skirge Familiar discards the other card");
    pay_and_resolve(&mut b.runner, b.disfigure);
    let s = b.runner.state();
    assert_eq!(
        s.objects[&other].zone,
        Zone::Graveyard,
        "the other card was discarded"
    );
    assert_eq!(
        s.objects[&b.bear].zone,
        Zone::Graveyard,
        "Disfigure killed the 2/2"
    );
    assert_eq!(s.objects[&b.disfigure].zone, Zone::Graveyard);
}

/// CR 601.2a: with Disfigure the only card in hand, Skirge Familiar has
/// nothing to discard once Disfigure is on the stack. The castability gate
/// generates the cast (before announcement Disfigure is in hand); the offer
/// refuses it, and cast anyway the reducer refuses the payment.
#[test]
fn skirge_familiar_cannot_discard_the_spell_being_cast() {
    let b = skirge_board(0);
    let action = auto_cast(&b.runner, b.disfigure);
    assert_generated_not_offered(&b.runner, &action);
    let mut announced = GameRunner::from_state(b.runner.state().clone());
    let refused = announce_and_target(&mut announced, action, vec![TargetRef::Object(b.bear)]);
    assert!(
        refused.is_err(),
        "the cast can't be paid once targeted: {:?}",
        announced.state().waiting_for
    );
    assert_eq!(
        verdict(&b.runner, b.disfigure, 64),
        TargetedCastVerdict::NotFound
    );
}

// ---------------------------------------------------------------------------
// Scope: origin, actor, target slots
// ---------------------------------------------------------------------------

/// Origin: a permission-backed graveyard cast. Firebolt's flashback {4}{R}
/// with four Plains and the Ogre's {R} (CR 702.34a).
#[test]
fn flashback_from_graveyard_is_offered_and_completes() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let plains: Vec<ObjectId> = (0..4)
        .map(|_| sc.add_basic_land(P0, ManaColor::White))
        .collect();
    let ogre = sc
        .add_creature(P0, "Treasonous Ogre", 2, 3)
        .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
        .id();
    let bear = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let firebolt = sc
        .add_spell_to_graveyard(P0, "Firebolt", false)
        .from_oracle_text(FIREBOLT)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 0))
        .with_keyword(Keyword::Flashback(FlashbackCost::Mana(cost(
            vec![ManaCostShard::Red],
            4,
        ))))
        .id();
    let mut runner = sc.build();
    let action = candidate_actions(runner.state())
        .into_iter()
        .map(|candidate| candidate.action)
        .find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == firebolt))
        .expect("reach: the flashback cast is generated");
    assert!(
        offered(&runner, &action),
        "Plains x4 + Ogre R pay {{4}}{{R}}"
    );
    announce_to_payment(&mut runner, action, vec![TargetRef::Object(bear)]);
    activate_paying(&mut runner, ogre, |_| true, ManaType::Red).expect("the Ogre pays");
    pay_and_resolve(&mut runner, firebolt);
    let s = runner.state();
    assert!(plains.iter().all(|id| s.objects[id].tapped));
    assert_eq!(s.players[0].life, 17);
    assert_eq!(
        s.objects[&bear].zone,
        Zone::Graveyard,
        "2 damage killed the 2/2"
    );
    assert_eq!(s.objects[&firebolt].zone, Zone::Exile, "CR 702.34a: exiled");
}

/// Actor (CR 723.5): P0 controls P1's turn. P1 has an Island, Treasonous Ogre
/// and Unsubstantiate; P0 makes P1's choices, and P1 pays the life.
#[test]
fn controlled_player_cast_is_offered_and_completes() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let island = sc.add_basic_land(P1, ManaColor::Blue);
    let ogre = sc
        .add_creature(P1, "Treasonous Ogre", 2, 3)
        .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
        .id();
    let bear = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P1, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    let mut runner = sc.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.turn_decision_controller = Some(P0);
        state.priority_passes.clear();
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
    }
    let action = auto_cast(&runner, spell);
    assert!(offered(&runner, &action), "P0 is offered P1's cast");
    announce_to_payment(&mut runner, action, vec![TargetRef::Object(bear)]);
    activate_paying(&mut runner, ogre, |_| true, ManaType::Red).expect("P1's Ogre pays");
    pay_and_resolve(&mut runner, spell);
    let s = runner.state();
    assert_eq!(s.players[1].life, 17, "P1, the caster, paid the life");
    assert_eq!(s.players[0].life, 20);
    assert!(s.objects[&island].tapped);
    assert_eq!(s.objects[&bear].zone, Zone::Hand);
}

/// Target slots: Arc Trail ({1}{R}) has two "any target" slots; the witness
/// fills both (a player and a creature here). Two Treasonous Ogres share one
/// life total (CR 119.4 + CR 118.3): at 5 life the second "Pay 3 life" can't
/// be paid; at 7 it can.
fn arc_trail_board(life: i32) -> (GameRunner, ObjectId, [ObjectId; 2], ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_life(P0, life);
    let mut ogre = || {
        sc.add_creature(P0, "Treasonous Ogre", 2, 3)
            .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
            .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
            .id()
    };
    let ogres = [ogre(), ogre()];
    let bear = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Arc Trail", false, ARC_TRAIL)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 1))
        .id();
    (sc.build(), spell, ogres, bear)
}

#[test]
fn arc_trail_two_slots_two_ogres_share_one_life_total() {
    let targets = |bear| vec![TargetRef::Player(P1), TargetRef::Object(bear)];

    let (runner, spell, ogres, bear) = arc_trail_board(5);
    let action = auto_cast(&runner, spell);
    assert_generated_not_offered(&runner, &action);
    let mut announced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut announced, action, targets(bear));
    activate_paying(&mut announced, ogres[0], |_| true, ManaType::Red).expect("first Ogre");
    assert_eq!(announced.state().players[0].life, 2);
    assert!(
        announced.act(activate(ogres[1])).is_err(),
        "CR 119.4: 2 life can't pay 3"
    );
    assert!(
        announced.act(GameAction::PassPriority).is_err(),
        "{{R}} can't pay {{1}}{{R}}"
    );
    assert_eq!(verdict(&runner, spell, 64), TargetedCastVerdict::NotFound);
    let (mut runner, spell, ogres, bear) = arc_trail_board(7);
    let action = auto_cast(&runner, spell);
    assert!(offered(&runner, &action));
    announce_to_payment(&mut runner, action, targets(bear));
    for ogre in ogres {
        activate_paying(&mut runner, ogre, |_| true, ManaType::Red).expect("an Ogre pays");
    }
    pay_and_resolve(&mut runner, spell);
    let s = runner.state();
    assert_eq!(s.players[0].life, 1);
    assert_eq!(s.players[1].life, 18, "2 damage to the first target, P1");
    assert_eq!(
        s.objects[&bear].damage_marked, 1,
        "1 damage to the other target, the creature"
    );
}

// ---------------------------------------------------------------------------
// Shared resources (R1, R3)
// ---------------------------------------------------------------------------

fn victim_of_night(sc: &mut GameScenario) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(P0, "Victim of Night", true, VICTIM_OF_NIGHT)
        .with_mana_cost(cost(vec![ManaCostShard::Black, ManaCostShard::Black], 0))
        .id()
}

/// Two Skirge Familiars (`text`), Victim of Night ({B}{B}) and `others` other
/// cards; P1 has Grizzly Bears.
fn twin_skirge_board(
    others: usize,
    text: &str,
) -> (GameRunner, ObjectId, [ObjectId; 2], ObjectId, Vec<ObjectId>) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let skirges = [skirge(&mut sc, text), skirge(&mut sc, text)];
    let bear = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = victim_of_night(&mut sc);
    let others = other_cards(&mut sc, others);
    (sc.build(), spell, skirges, bear, others)
}

/// R1 (CR 601.2h + CR 118.3): two Skirge Familiars can't both discard the one
/// other card. Cast anyway: the first discards it, the second has nothing to
/// discard, and {B} can't pay {B}{B}. With two other cards: offered, completes.
#[test]
fn r1_two_discard_sources_share_one_hand() {
    let (runner, spell, skirges, bear, others) = twin_skirge_board(1, SKIRGE_FAMILIAR);
    let action = auto_cast(&runner, spell);
    assert_generated_not_offered(&runner, &action);
    let mut announced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut announced, action, vec![TargetRef::Object(bear)]);
    activate_paying(
        &mut announced,
        skirges[0],
        |id| *id == others[0],
        ManaType::Black,
    )
    .expect("the first Familiar discards the other card");
    assert!(
        activate_paying(&mut announced, skirges[1], |_| true, ManaType::Black).is_err(),
        "the second Familiar has nothing to discard"
    );
    assert!(announced.act(GameAction::PassPriority).is_err());
    assert_eq!(verdict(&runner, spell, 64), TargetedCastVerdict::NotFound);

    let (mut runner, spell, skirges, bear, others) = twin_skirge_board(2, SKIRGE_FAMILIAR);
    let action = auto_cast(&runner, spell);
    assert!(offered(&runner, &action));
    announce_to_payment(&mut runner, action, vec![TargetRef::Object(bear)]);
    for skirge in skirges {
        activate_paying(
            &mut runner,
            skirge,
            |id| others.contains(id),
            ManaType::Black,
        )
        .expect("a Familiar discards an other card");
    }
    pay_and_resolve(&mut runner, spell);
    let s = runner.state();
    assert_eq!(s.objects[&bear].zone, Zone::Graveyard);
    assert!(others
        .iter()
        .all(|id| s.objects[id].zone == Zone::Graveyard));
}

/// R3-B infrastructure fixture: Skirge Familiar plus a synthetic free
/// "{T}: Add {C}." sibling.
/// - No printed counterpart (Skirge has one mana ability).
/// - Nearest printed card: Skirge Familiar.
/// - Needed because the withdrawn abstract proof took a componentwise minimum
///   of demand over an object's abilities, so a free sibling erased the
///   discard's demand.
const SKIRGE_WITH_FREE_SIBLING: &str = "Flying\nDiscard a card: Add {B}.\n{T}: Add {C}.";

/// R3-B: the free sibling makes {C}, which doesn't pay {B}; one card still
/// can't feed two discards. With two cards it is offered.
#[test]
fn r3b_free_sibling_ability_does_not_pay_the_discard_demand() {
    let (runner, spell, skirges, bear, others) = twin_skirge_board(1, SKIRGE_WITH_FREE_SIBLING);
    let action = auto_cast(&runner, spell);
    assert_generated_not_offered(&runner, &action);
    let mut announced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut announced, action, vec![TargetRef::Object(bear)]);
    activate_paying(
        &mut announced,
        skirges[0],
        |id| *id == others[0],
        ManaType::Black,
    )
    .expect("the first Familiar discards");
    assert!(activate_paying(&mut announced, skirges[1], |_| true, ManaType::Black).is_err());
    announced
        .act(GameAction::ActivateAbility {
            source_id: skirges[1],
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the free sibling adds {C}");
    assert!(
        announced.act(GameAction::PassPriority).is_err(),
        "{{B}}{{C}} can't pay {{B}}{{B}}"
    );

    let (mut payable, payable_spell, skirges, bear, others) =
        twin_skirge_board(2, SKIRGE_WITH_FREE_SIBLING);
    let action = auto_cast(&payable, payable_spell);
    assert!(offered(&payable, &action));
    assert_eq!(
        verdict(&payable, payable_spell, 64),
        TargetedCastVerdict::Completed
    );
    announce_to_payment(&mut payable, action, vec![TargetRef::Object(bear)]);
    for (skirge, other) in skirges.iter().zip(&others) {
        activate_paying(&mut payable, *skirge, |id| id == other, ManaType::Black)
            .expect("each Familiar discards one other card");
    }
    pay_and_resolve(&mut payable, payable_spell);
    let s = payable.state();
    assert_eq!(
        s.objects[&bear].zone,
        Zone::Graveyard,
        "Victim of Night destroyed it"
    );
    assert_eq!(s.objects[&payable_spell].zone, Zone::Graveyard);
    assert!(others
        .iter()
        .all(|id| s.objects[id].zone == Zone::Graveyard));
}

/// R3-A: two Citanul Stalwarts and `helpers` P0 Grizzly Bears; Victim of
/// Night; P1's Grizzly Bears.
fn twin_stalwart_board(helpers: usize) -> (GameRunner, ObjectId, [ObjectId; 2], ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let mut stalwart = || {
        sc.add_creature(P0, "Citanul Stalwart", 1, 1)
            .from_oracle_text(CITANUL_STALWART)
            .with_mana_cost(cost(vec![ManaCostShard::Green], 0))
            .id()
    };
    let stalwarts = [stalwart(), stalwart()];
    for _ in 0..helpers {
        sc.add_creature(P0, "Grizzly Bears", 2, 2);
    }
    let target = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = victim_of_night(&mut sc);
    (sc.build(), spell, stalwarts, target)
}

/// R3-A (CR 118.3): with one helper, two Stalwarts can't both tap an untapped
/// artifact or creature, so only one {B} is reachable. Cast anyway: the
/// second Stalwart has no helper left.
#[test]
fn r3a_two_tap_helper_sources_share_one_helper() {
    let (runner, spell, stalwarts, target) = twin_stalwart_board(1);
    let action = auto_cast(&runner, spell);
    assert_generated_not_offered(&runner, &action);
    let mut announced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut announced, action, vec![TargetRef::Object(target)]);
    activate_paying(
        &mut announced,
        stalwarts[0],
        |id| !stalwarts.contains(id),
        ManaType::Black,
    )
    .expect("the first Stalwart taps the helper");
    assert!(
        activate_paying(
            &mut announced,
            stalwarts[1],
            |id| !stalwarts.contains(id),
            ManaType::Black
        )
        .is_err(),
        "no untapped helper is left"
    );
    assert!(announced.act(GameAction::PassPriority).is_err());
    assert_eq!(verdict(&runner, spell, 64), TargetedCastVerdict::NotFound);
}

/// The bound decides R3-A's positive: two helpers make it payable, but the
/// "any color" prompts need more than the per-cast bound. Refused at 64
/// (no claim), completed at 256; cast by hand it completes.
#[test]
fn r3a_two_helpers_is_the_bound_discriminator() {
    let (mut runner, spell, stalwarts, target) = twin_stalwart_board(2);
    assert_eq!(TARGETED_CAST_OFFER_MAX_REDUCER_ATTEMPTS, 64);
    let action = auto_cast(&runner, spell);
    assert!(!offered(&runner, &action), "not witnessed within the bound");
    assert_eq!(
        verdict(&runner, spell, 64),
        TargetedCastVerdict::Indeterminate
    );
    assert_eq!(verdict(&runner, spell, 256), TargetedCastVerdict::Completed);
    announce_to_payment(&mut runner, action, vec![TargetRef::Object(target)]);
    for stalwart in stalwarts {
        let used = runner
            .state()
            .objects
            .values()
            .find(|o| o.name == "Grizzly Bears" && o.controller == P0 && !o.tapped);
        let helper = used.map(|o| o.id).expect("an untapped helper");
        activate_paying(&mut runner, stalwart, |id| *id == helper, ManaType::Black)
            .expect("a Stalwart taps a helper");
    }
    pay_and_resolve(&mut runner, spell);
    assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// Output read after the source's own cost (R5, R10-R13, R15) and
// mid-payment legality (R14, R16)
// ---------------------------------------------------------------------------

const SOLDEVI_ADNATE: &str =
    "{T}, Sacrifice a black or artifact creature: Add an amount of {B} equal to the sacrificed creature's mana value.";

/// R5 infrastructure fixture: Soldevi Adnate's output ("an amount of {B}
/// equal to the ... mana value" of the cost-paid object) behind "{T}, Exile a
/// card from your graveyard" instead of its sacrifice.
/// - No printed counterpart with an exile cost.
/// - Nearest printed card: Soldevi Adnate.
/// - Needed because a sacrifice source routes the cast through
///   `AutoExceptSacrificialMana`, where the offer verdict defers; the exile
///   cost keeps the cast on plain Auto, where the yield sized by the cost-paid
///   object is what the withdrawn proof over-credited.
///
/// Two such sources; the graveyard holds a mana value 1 card and one of
/// `second` mana value; Victim of Night ({B}{B}) in hand.
fn exile_sized_board(
    second: u32,
) -> (GameRunner, ObjectId, Vec<ObjectId>, [ObjectId; 2], ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let sources: Vec<ObjectId> = (0..2)
        .map(|_| {
            sc.add_creature(P0, "Graveyard Adnate", 1, 2)
                .from_oracle_text(SOLDEVI_ADNATE)
                .id()
        })
        .collect();
    let card_a = sc
        .add_creature_to_graveyard(P0, "Graveyard Card A", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let card_b = sc
        .add_creature_to_graveyard(P0, "Graveyard Card B", 2, 2)
        .with_mana_cost(ManaCost::generic(second))
        .id();
    let target = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = victim_of_night(&mut sc);
    let mut runner = sc.build();
    let exile_cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: Some(TargetFilter::Typed(
                    TypedFilter::card()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::InZone {
                            zone: Zone::Graveyard,
                        }]),
                )),
            },
        ],
    };
    for source in &sources {
        let object = runner.state_mut().objects.get_mut(source).unwrap();
        std::sync::Arc::make_mut(&mut object.abilities)[0].cost = Some(exile_cost.clone());
        std::sync::Arc::make_mut(&mut object.base_abilities)[0].cost = Some(exile_cost.clone());
    }
    (runner, spell, sources, [card_a, card_b], target)
}

/// R5 (CR 106.1 + CR 601.2h): graveyard cards of mana value 1 and 0 make at
/// most {B}, so Victim of Night's {B}{B} can't be paid: cast anyway, both
/// sources activate and the pool holds one {B}. With 1 and 1 it completes.
#[test]
fn r5_cost_sized_yield_is_judged_by_what_it_makes() {
    let (runner, spell, sources, cards, target) = exile_sized_board(0);
    let action = auto_cast(&runner, spell);
    assert_generated_not_offered(&runner, &action);
    let mut announced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut announced, action, vec![TargetRef::Object(target)]);
    for (source, card) in sources.iter().zip(cards) {
        activate_paying(&mut announced, *source, |id| *id == card, ManaType::Black)
            .expect("a source exiles a graveyard card");
    }
    assert_eq!(
        announced.state().players[0]
            .mana_pool
            .count_color(ManaType::Black),
        1
    );
    assert!(announced.act(GameAction::PassPriority).is_err());
    assert_eq!(verdict(&runner, spell, 64), TargetedCastVerdict::NotFound);

    let (mut runner, spell, sources, cards, target) = exile_sized_board(1);
    let action = auto_cast(&runner, spell);
    assert!(offered(&runner, &action));
    announce_to_payment(&mut runner, action, vec![TargetRef::Object(target)]);
    for (source, card) in sources.iter().zip(cards) {
        activate_paying(&mut runner, *source, |id| *id == card, ManaType::Black)
            .expect("a source exiles a graveyard card");
    }
    pay_and_resolve(&mut runner, spell);
    assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
}

/// "<cost>: Add <produced>." as a direct mana ability.
fn mana_ability(produced: ManaProduction, cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(cost)
}

fn fixed(colors: Vec<ManaColor>) -> ManaProduction {
    ManaProduction::Fixed {
        colors,
        contribution: ManaContribution::Base,
    }
}

fn pay_life(value: i32) -> AbilityCost {
    AbilityCost::PayLife {
        amount: QuantityExpr::Fixed { value },
    }
}

fn controller_life() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::LifeTotal {
            player: PlayerScope::Controller,
        },
    }
}

/// Battery infrastructure host: 0/3 creatures for P0 carrying `batteries`,
/// P0 at `life`, no other producer; Unsubstantiate ({1}{U}) in hand; P1's
/// Grizzly Bears. `setup` adds the rest of the board.
fn battery_board(
    life: i32,
    batteries: Vec<AbilityDefinition>,
    setup: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_life(P0, life);
    let sources = batteries
        .into_iter()
        .map(|ability| {
            sc.add_creature(P0, "Battery", 0, 3)
                .with_ability_definition(ability)
                .id()
        })
        .collect();
    setup(&mut sc);
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    (sc.build(), spell, sources, bears)
}

/// Refusal through the real payment path: cast anyway, activate each source
/// that still activates, and the payment is refused. `expect_pool` is what the
/// activations actually made, as (black, blue).
fn assert_battery_refused(
    runner: &GameRunner,
    spell: ObjectId,
    sources: &[ObjectId],
    bears: ObjectId,
    expect_pool: (usize, usize),
) {
    let action = auto_cast(runner, spell);
    assert_generated_not_offered(runner, &action);
    let mut announced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut announced, action, vec![TargetRef::Object(bears)]);
    for source in sources {
        let _ = activate_paying(&mut announced, *source, |_| true, ManaType::Blue);
    }
    let pool = &announced.state().players[0].mana_pool;
    assert_eq!(
        (
            pool.count_color(ManaType::Black),
            pool.count_color(ManaType::Blue)
        ),
        expect_pool,
        "reach: what the activations made"
    );
    assert!(
        announced.act(GameAction::PassPriority).is_err(),
        "the pool can't pay {{1}}{{U}}"
    );
    assert_eq!(verdict(runner, spell, 64), TargetedCastVerdict::NotFound);
}

/// Positive control for the battery fixtures: offered, and the offered action
/// completes by activating each battery and passing.
fn assert_battery_completes(
    mut runner: GameRunner,
    spell: ObjectId,
    sources: &[ObjectId],
    bears: ObjectId,
) {
    let action = auto_cast(&runner, spell);
    assert!(offered(&runner, &action));
    announce_to_payment(&mut runner, action, vec![TargetRef::Object(bears)]);
    for source in sources {
        activate_paying(&mut runner, *source, |_| true, ManaType::Blue).expect("a battery pays");
    }
    pay_and_resolve(&mut runner, spell);
    let s = runner.state();
    assert_eq!(s.objects[&bears].zone, Zone::Hand);
    assert_eq!(s.objects[&spell].zone, Zone::Graveyard);
}

/// R10 infrastructure battery: "Pay 3 life: Add an amount of {U} equal to
/// <count>."
/// - No printed counterpart sizes a pay-life mana ability by the life total.
/// - Nearest printed card: Treasonous Ogre (pay life for mana).
/// - Needed because the amount is read after the cost is paid (CR 602.2b), so
///   a proof that sizes it from the pre-cost state over-credits.
#[test]
fn r10_amount_read_after_its_own_life_payment() {
    let sized = |count| {
        mana_ability(
            ManaProduction::AnyOneColor {
                count,
                color_options: vec![ManaColor::Blue],
                contribution: ManaContribution::Base,
            },
            pay_life(3),
        )
    };
    let (runner, spell, sources, bears) = battery_board(4, vec![sized(controller_life())], |_| {});
    assert_battery_refused(&runner, spell, &sources, bears, (0, 1));
    let (control, spell, sources, bears) =
        battery_board(4, vec![sized(QuantityExpr::Fixed { value: 2 })], |_| {});
    assert_battery_completes(control, spell, &sources, bears);
}

/// R11 infrastructure fixture: a blue artifact creature with one charge
/// counter and "Remove a charge counter from this: Add <produced>."
/// - No printed counterpart reads its colors from permanents with charge
///   counters after removing its own.
/// - Nearest printed cards: Pentad Prism (remove a charge counter: add mana),
///   and Fellwar Stone's "any color among" reading.
/// - Needed because the colors are read after the counter is removed (CR
///   602.2b), and an empty color set makes no mana (CR 106.5).
fn counter_colored_board(produced: ManaProduction) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    use engine::types::ability::CounterCostSelection;
    use engine::types::counter::{CounterMatch, CounterType};
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let source = sc
        .add_creature(P0, "Charged Prism", 0, 1)
        .as_artifact_creature()
        .with_color(vec![ManaColor::Blue])
        .with_ability_definition(mana_ability(
            produced,
            AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                target: None,
                selection: CounterCostSelection::SingleObject,
            },
        ))
        .id();
    sc.with_counter(source, CounterType::Generic("charge".to_string()), 1);
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    (sc.build(), spell, source, bears)
}

/// R11: "Add two mana of any one color among artifacts you control with
/// charge counters" finds no color once its own last counter is gone, so it
/// makes nothing. Control: a fixed blue output for the same cost.
#[test]
fn r11_colors_read_after_their_own_counter_cost() {
    use engine::types::counter::{CounterMatch, CounterType};
    let (runner, spell, source, bears) =
        counter_colored_board(ManaProduction::AnyOneColorAmongPermanents {
            count: QuantityExpr::Fixed { value: 2 },
            filter: TargetFilter::Typed(
                TypedFilter::new(engine::types::ability::TypeFilter::Artifact)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Counters {
                        counters: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                        comparator: Comparator::GE,
                        count: QuantityExpr::Fixed { value: 1 },
                    }]),
            ),
            contribution: ManaContribution::Base,
        });
    assert_battery_refused(&runner, spell, &[source], bears, (0, 0));
    let (control, spell, source, bears) = counter_colored_board(ManaProduction::AnyOneColor {
        count: QuantityExpr::Fixed { value: 2 },
        color_options: vec![ManaColor::Blue],
        contribution: ManaContribution::Base,
    });
    assert_battery_completes(control, spell, &[source], bears);
}

/// R12 infrastructure battery: "Pay 3 life: Add {U}{U}. If your life total is
/// 1 or less, add {U} instead."
/// - No printed counterpart.
/// - Nearest printed card: Treasonous Ogre (pay life for mana); the "instead"
///   rider follows the ConditionInstead shape the parser builds.
/// - Needed because the condition is checked after the life is paid (CR
///   608.2c), so the root production over-credits.
#[test]
fn r12_mana_swapped_by_a_condition_after_its_cost() {
    use engine::types::ability::AbilityCondition;
    let instead = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: fixed(vec![ManaColor::Blue]),
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .condition(AbilityCondition::ConditionInstead {
        inner: Box::new(AbilityCondition::QuantityCheck {
            lhs: controller_life(),
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
    });
    let (runner, spell, sources, bears) =
        battery_board(4, vec![life_battery(Some(instead))], |_| {});
    assert_battery_refused(&runner, spell, &sources, bears, (0, 1));
    let (control, spell, sources, bears) = battery_board(4, vec![life_battery(None)], |_| {});
    assert_battery_completes(control, spell, &sources, bears);
}

/// "Pay 3 life: Add {U}{U}." with an optional sub-ability `rider` that the
/// ability follows after producing its mana (CR 608.2c).
fn life_battery(rider: Option<AbilityDefinition>) -> AbilityDefinition {
    let battery = mana_ability(fixed(vec![ManaColor::Blue, ManaColor::Blue]), pay_life(3));
    match rider {
        Some(rider) => battery.sub_ability(rider),
        None => battery,
    }
}
/// R13/R15 infrastructure artifact for P0: "If a permanent would produce
/// mana, it produces {B} instead", applicable while `condition` holds.
/// - No printed counterpart applies to non-land permanents.
/// - Nearest printed card: Contamination (lands only, no condition).
/// - Needed because the replacement rewrites the deposit after production
///   (CR 614.1a + CR 614.6), and (R15) can become applicable only after the
///   source's own cost.
fn black_mana_replacement(
    sc: &mut GameScenario,
    condition: Option<engine::types::ability::ReplacementCondition>,
) {
    use engine::types::ability::{ManaModification, ReplacementDefinition};
    use engine::types::replacements::ReplacementEvent;
    let mut replacement = ReplacementDefinition::new(ReplacementEvent::ProduceMana)
        .mana_modification(ManaModification::ReplaceWith {
            mana_type: ManaType::Black,
        });
    if let Some(condition) = condition {
        replacement = replacement.condition(condition);
    }
    sc.add_creature(P0, "Contamination Stand-in", 0, 1)
        .as_artifact()
        .with_replacement_definition(replacement);
}

/// R13 (CR 614.1a + CR 614.6): the replacement turns the battery's {U}{U}
/// into {B}{B}. Control: no replacement.
#[test]
fn r13_mana_production_replacement() {
    let battery = || mana_ability(fixed(vec![ManaColor::Blue, ManaColor::Blue]), pay_life(3));
    let (runner, spell, sources, bears) =
        battery_board(4, vec![battery()], |sc| black_mana_replacement(sc, None));
    assert_battery_refused(&runner, spell, &sources, bears, (2, 0));
    let (control, spell, sources, bears) = battery_board(4, vec![battery()], |_| {});
    assert_battery_completes(control, spell, &sources, bears);
}

/// R15: the replacement applies only at 1 life or less. At 4 life the
/// battery's own "Pay 3 life" makes it applicable, so {U}{U} arrives as
/// {B}{B}. Control: at 5 life it stays inapplicable.
#[test]
fn r15_replacement_made_applicable_by_the_sources_own_cost() {
    use engine::types::ability::ReplacementCondition;
    let battery = || mana_ability(fixed(vec![ManaColor::Blue, ManaColor::Blue]), pay_life(3));
    let at_most_one_life = || ReplacementCondition::OnlyIfQuantity {
        lhs: controller_life(),
        comparator: Comparator::LE,
        rhs: QuantityExpr::Fixed { value: 1 },
        active_player_req: None,
    };
    let (runner, spell, sources, bears) = battery_board(4, vec![battery()], |sc| {
        black_mana_replacement(sc, Some(at_most_one_life()))
    });
    assert_battery_refused(&runner, spell, &sources, bears, (2, 0));
    let (control, spell, sources, bears) = battery_board(5, vec![battery()], |sc| {
        black_mana_replacement(sc, Some(at_most_one_life()))
    });
    assert_battery_completes(control, spell, &sources, bears);
}

/// R14 infrastructure battery: "Pay 3 life: Add {U}. Activate only if your
/// life total is 4 or greater."
/// - No printed counterpart.
/// - Nearest printed card: Fanatic of Rhonas ("Ferocious — {T}: Add
///   {G}{G}{G}{G}. Activate only if you control a creature with power 4 or
///   greater.").
/// - Needed because the restriction is rechecked at each activation (CR
///   602.1b + CR 602.5), after the first source's payment.
///
/// At 6 life the second activation is illegal; at 7 both activate.
#[test]
fn r14_state_reading_activation_restriction() {
    use engine::types::ability::{ActivationRestriction, ParsedCondition};
    let restricted = || {
        mana_ability(fixed(vec![ManaColor::Blue]), pay_life(3)).activation_restrictions(vec![
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: controller_life(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                }),
            },
        ])
    };
    let (runner, spell, sources, bears) =
        battery_board(6, vec![restricted(), restricted()], |_| {});
    assert_battery_refused(&runner, spell, &sources, bears, (0, 1));
    let (rich, spell, sources, bears) = battery_board(7, vec![restricted(), restricted()], |_| {});
    assert_battery_completes(rich, spell, &sources, bears);
}

/// R16 infrastructure artifact for P0: "Activated abilities of creatures
/// can't be activated while your life total is 3 or less."
/// - No printed counterpart with a life condition.
/// - Nearest printed card: Linvala, Keeper of Silence (creatures' activated
///   abilities can't be activated).
/// - Needed because the prohibition becomes active mid-payment, after the
///   first battery's cost (CR 602.5 + CR 604.2).
///
/// At 6 life the second "Pay 3 life: Add {U}" battery can't be activated; at
/// 7 both activate.
#[test]
fn r16_activation_prohibition_reached_mid_payment() {
    use engine::types::ability::{StaticCondition, StaticDefinition};
    use engine::types::statics::{ActivationExemption, ProhibitionScope, StaticMode};
    let prohibition = |sc: &mut GameScenario| {
        sc.add_creature(P0, "Prohibition Stand-in", 0, 1)
            .as_artifact()
            .with_static_definition(
                StaticDefinition::new(StaticMode::CantBeActivated {
                    who: ProhibitionScope::AllPlayers,
                    source_filter: TargetFilter::Typed(TypedFilter::creature()),
                    exemption: ActivationExemption::None,
                    kind: None,
                })
                .condition(StaticCondition::QuantityComparison {
                    lhs: controller_life(),
                    comparator: Comparator::LE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }),
            );
    };
    let battery = || mana_ability(fixed(vec![ManaColor::Blue]), pay_life(3));
    let (runner, spell, sources, bears) = battery_board(6, vec![battery(), battery()], prohibition);
    assert_battery_refused(&runner, spell, &sources, bears, (0, 1));
    let (rich, spell, sources, bears) = battery_board(7, vec![battery(), battery()], prohibition);
    assert_battery_completes(rich, spell, &sources, bears);
}

// ---------------------------------------------------------------------------
// Expenditure: per cast and per filter pass
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CopiesBoard {
    /// Two Skirge Familiars; the other copies are discard fodder, so each
    /// cast completes.
    Completable,
    /// Two Citanul Stalwarts and two helpers: payable, but beyond the
    /// per-cast bound (see `r3a_two_helpers_is_the_bound_discriminator`).
    BeyondTheBound,
}

/// `copies` Victim of Night in hand, a 30-creature target domain for P1.
fn copies_board(copies: usize, kind: CopiesBoard) -> (GameRunner, Vec<ObjectId>) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    match kind {
        CopiesBoard::Completable => {
            skirge(&mut sc, SKIRGE_FAMILIAR);
            skirge(&mut sc, SKIRGE_FAMILIAR);
            if copies < 3 {
                other_cards(&mut sc, 2);
            }
        }
        CopiesBoard::BeyondTheBound => {
            for _ in 0..2 {
                sc.add_creature(P0, "Citanul Stalwart", 1, 1)
                    .from_oracle_text(CITANUL_STALWART)
                    .with_mana_cost(cost(vec![ManaCostShard::Green], 0));
                sc.add_creature(P0, "Grizzly Bears", 2, 2);
            }
        }
    }
    for i in 0..30 {
        sc.add_creature(P1, &format!("Target {i}"), 2, 2);
    }
    let spells = (0..copies).map(|_| victim_of_night(&mut sc)).collect();
    (sc.build(), spells)
}

/// The offer's spend is deterministic and bounded: at most 64 reducer
/// applications per qualifying cast and 256 per `legal_actions` pass, however
/// many casts qualify; exhaustion never offers.
#[test]
fn offer_witness_expenditure_is_bounded_per_cast_and_per_pass() {
    for kind in [CopiesBoard::Completable, CopiesBoard::BeyondTheBound] {
        for copies in [1usize, 6, 12] {
            let (runner, spells) = copies_board(copies, kind);
            perf_counters::reset_targeted_offer_witness_counters();
            let actions = legal_actions(runner.state());
            let counters = perf_counters::targeted_offer_witness_snapshot();
            let offered_count = spells
                .iter()
                .filter(|spell| {
                    actions.iter().any(|action| {
                        matches!(action, GameAction::CastSpell { object_id, .. } if object_id == *spell)
                    })
                })
                .count();
            eprintln!(
                "[{kind:?} x{copies}] offered {offered_count}/{copies}; witness calls {}, completed {}, attempts {}, clones {}",
                counters.calls, counters.completed, counters.reducer_attempts, counters.phase_owned_state_clones
            );
            assert!(counters.calls >= 1, "reach: the witness ran");
            assert!(counters.calls <= copies as u64);
            assert!(
                counters.reducer_attempts <= PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS as u64,
                "one pass spends at most the shared pool"
            );
            assert!(
                counters.reducer_attempts
                    <= counters.calls * TARGETED_CAST_OFFER_MAX_REDUCER_ATTEMPTS as u64
            );
            assert!(
                offered_count as u64 <= counters.completed + (copies as u64 - counters.calls),
                "only a certified run (or a verdict it shares through the legality memo) offers"
            );
            match kind {
                CopiesBoard::Completable => assert!(offered_count >= 1),
                CopiesBoard::BeyondTheBound => {
                    assert_eq!(offered_count, 0, "exhaustion never offers");
                    assert_eq!(counters.completed, 0);
                }
            }
        }
    }
}

/// Measurement instrument (not an assertion): wall time of `legal_actions` on
/// the expenditure boards. Run with
/// `cargo nextest run -p phase-engine --features proptest -E 'test(offer_witness_cost_measurement)' --run-ignored all --no-capture`
/// (add `--cargo-profile tool` or another optimized profile for production-like
/// numbers).
#[test]
#[ignore = "measurement instrument, not an assertion; see the doc comment"]
fn offer_witness_cost_measurement() {
    const RUNS: u32 = 10;
    for kind in [CopiesBoard::Completable, CopiesBoard::BeyondTheBound] {
        for copies in [1usize, 6, 12] {
            let (runner, _) = copies_board(copies, kind);
            let _ = legal_actions(runner.state());
            let start = std::time::Instant::now();
            for _ in 0..RUNS {
                std::hint::black_box(legal_actions(runner.state()));
            }
            eprintln!(
                "[{kind:?} x{copies}] legal_actions mean {:?}",
                start.elapsed() / RUNS
            );
        }
    }
    for (label, board) in [
        ("island", BlueSource::Island),
        ("nyx", BlueSource::NyxLotus),
    ] {
        let b = ogre_board(board);
        let start = std::time::Instant::now();
        for _ in 0..RUNS {
            std::hint::black_box(legal_actions(b.runner.state()));
        }
        eprintln!("[{label}] legal_actions mean {:?}", start.elapsed() / RUNS);
    }
}

// ---------------------------------------------------------------------------
// Branches the witness may not certify (P1-1 honesty, hidden information)
// ---------------------------------------------------------------------------

/// D3 (CR 119.4 + CR 704.5a): at 6 life two Treasonous Ogres pay Arc Trail's
/// {1}{R} down to 0 life; the cast completes and the state-based action then
/// ends the game, clearing the stack. A branch that loses the root without a
/// finalization the witness can recognize is unexplored, so the verdict makes
/// no claim (`Indeterminate`), never a refusal (`NotFound`). Not offered: a
/// known limit of the shared finalization predicate.
#[test]
fn completion_that_ends_the_game_is_no_claim_either_way() {
    let (runner, spell, ogres, bear) = arc_trail_board(6);
    let action = auto_cast(&runner, spell);
    assert!(generated(&runner, &action), "reach: the gate generates it");
    assert_eq!(
        verdict(&runner, spell, 64),
        TargetedCastVerdict::Indeterminate
    );
    let mut announced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(
        &mut announced,
        action,
        vec![TargetRef::Player(P1), TargetRef::Object(bear)],
    );
    for ogre in ogres {
        activate_paying(&mut announced, ogre, |_| true, ManaType::Red).expect("an Ogre pays");
    }
    announced
        .act(GameAction::PassPriority)
        .expect("CR 119.4: paying down to 0 life is allowed, and the cast completes");
    assert!(
        matches!(announced.state().waiting_for, WaitingFor::GameOver { .. }),
        "CR 704.5a: the caster then loses: {:?}",
        announced.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// Information safety through the offer (CR 601.2h + CR 400.2)
// ---------------------------------------------------------------------------

const VEXING_PUZZLEBOX: &str = "Whenever you roll one or more dice, put a number of charge counters on this artifact equal to the result.\n{T}: Add one mana of any color. Roll a d20.\n{T}, Remove 100 charge counters from this artifact: Search your library for an artifact card, put that card onto the battlefield, then shuffle.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlueFrom {
    /// Vexing Puzzlebox: "{T}: Add one mana of any color. Roll a d20."
    Puzzlebox,
    /// Control: a basic Island.
    Island,
}

/// P0 at 20 life: `blue` and Treasonous Ogre, no other mana; Unsubstantiate
/// in hand; P1 has Grizzly Bears.
fn die_roll_board(blue: BlueFrom) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let source = match blue {
        BlueFrom::Puzzlebox => sc
            .add_artifact_from_oracle(P0, "Vexing Puzzlebox", VEXING_PUZZLEBOX)
            .with_mana_cost(cost(vec![], 3))
            .id(),
        BlueFrom::Island => sc.add_basic_land(P0, ManaColor::Blue),
    };
    let ogre = sc
        .add_creature(P0, "Treasonous Ogre", 2, 3)
        .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
        .with_mana_cost(cost(vec![ManaCostShard::Red], 3))
        .id();
    let bear = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 1))
        .id();
    (sc.build(), spell, source, ogre, bear)
}

/// The index of `source`'s first mana ability.
fn mana_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(&*ability.effect, Effect::Mana { .. }))
        .expect("the source has a mana ability")
}

/// Activate `source`'s mana ability during payment, answer a colour prompt
/// with `color`, and return every event the activation emitted.
fn activate_collecting(
    runner: &mut GameRunner,
    source: ObjectId,
    color: ManaType,
) -> Vec<engine::types::events::GameEvent> {
    let ability_index = mana_ability_index(runner, source);
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the mana ability activates during payment")
        .events;
    if let WaitingFor::ChooseManaColor { .. } = runner.state().waiting_for {
        events.extend(
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(color),
                    count: 1,
                })
                .expect("the colour is chosen")
                .events,
        );
    }
    events
}

/// The offer is evaluated on scratch copies: the live state, including the
/// game's random generator, is unchanged, and the witness was consulted.
fn offer_with_witness_reach(runner: &GameRunner, action: &GameAction) -> bool {
    let before = serde_json::to_value(runner.state()).expect("the state serializes");
    let rng_before = runner.state().rng.get_word_pos();
    perf_counters::reset_targeted_offer_witness_counters();
    let offered = offered(runner, action);
    assert!(
        perf_counters::targeted_offer_witness_snapshot().calls >= 1,
        "reach: the offer consulted the witness"
    );
    assert_eq!(runner.state().rng.get_word_pos(), rng_before);
    assert_eq!(
        serde_json::to_value(runner.state()).expect("the state serializes"),
        before
    );
    offered
}

/// RNG leg (CR 601.2h + CR 706.1). Vexing Puzzlebox's blue and the Ogre's red
/// complete Unsubstantiate, but the Puzzlebox's mana ability rolls a d20,
/// which draws from the game's random generator. The witness never certifies
/// that branch, so it makes no claim and the cast isn't offered; cast by hand
/// it completes and the die is rolled (a deliberate under-offer). Control: an
/// Island instead is offered and completes.
#[test]
fn completion_that_rolls_a_die_is_not_certified() {
    let (runner, spell, puzzlebox, ogre, bear) = die_roll_board(BlueFrom::Puzzlebox);
    let action = auto_cast(&runner, spell);
    assert!(generated(&runner, &action), "reach: the cast is generated");
    assert!(
        !offer_with_witness_reach(&runner, &action),
        "a completion that needs a die roll isn't certified"
    );
    assert_eq!(
        verdict(&runner, spell, 64),
        TargetedCastVerdict::Indeterminate
    );

    let mut forced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut forced, action, vec![TargetRef::Object(bear)]);
    let events = activate_collecting(&mut forced, puzzlebox, ManaType::Blue);
    assert!(
        events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::DieRolled { sides: 20, .. }
        )),
        "reach: the Puzzlebox's mana ability rolled a d20"
    );
    activate_paying(&mut forced, ogre, |_| true, ManaType::Red).expect("the Ogre pays");
    pay_and_resolve(&mut forced, spell);
    assert_eq!(forced.state().objects[&bear].zone, Zone::Hand);

    let (mut control, spell, island, ogre, bear) = die_roll_board(BlueFrom::Island);
    let action = auto_cast(&control, spell);
    assert!(offer_with_witness_reach(&control, &action));
    announce_to_payment(&mut control, action, vec![TargetRef::Object(bear)]);
    activate_paying(&mut control, ogre, |_| true, ManaType::Red).expect("the Ogre pays");
    pay_and_resolve(&mut control, spell);
    assert!(control.state().objects[&island].tapped);
    assert_eq!(control.state().objects[&bear].zone, Zone::Hand);
}

/// Hidden-event leg (CR 601.2h + CR 701.20b). Infrastructure battery: "Pay 3
/// life: Add {U}{U}. Reveal the top card of your library."
/// - No printed counterpart.
/// - Nearest printed cards: Treasonous Ogre (pay life for mana) and
///   Metalworker (a mana ability that reveals).
/// - Needed because revealing moves no card (CR 701.20b), so CR 605.1a keeps
///   it a mana ability, yet it shows a card from a hidden zone.
///
/// The witness never certifies that branch, so it makes no claim and the cast
/// isn't offered; cast by hand it completes and the card is revealed.
/// Control: the same battery without the rider is offered and completes.
#[test]
fn completion_that_reveals_a_library_card_is_not_certified() {
    let reveal = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::RevealTop {
            player: TargetFilter::Controller,
            count: 1,
        },
    );
    let library = |sc: &mut GameScenario| {
        sc.with_library_top(P0, &["Library Card A", "Library Card B"]);
    };
    let (runner, spell, sources, bears) =
        battery_board(20, vec![life_battery(Some(reveal))], library);
    let action = auto_cast(&runner, spell);
    assert!(generated(&runner, &action), "reach: the cast is generated");
    assert!(
        !offer_with_witness_reach(&runner, &action),
        "a completion that reveals a library card isn't certified"
    );
    assert_eq!(
        verdict(&runner, spell, 64),
        TargetedCastVerdict::Indeterminate
    );

    let mut forced = GameRunner::from_state(runner.state().clone());
    announce_to_payment(&mut forced, action, vec![TargetRef::Object(bears)]);
    let events = activate_collecting(&mut forced, sources[0], ManaType::Blue);
    assert!(
        events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::CardsRevealed { .. }
        )),
        "reach: the mana ability revealed a card: {events:?}"
    );
    pay_and_resolve(&mut forced, spell);
    assert_eq!(forced.state().objects[&bears].zone, Zone::Hand);

    let (control, spell, sources, bears) = battery_board(20, vec![life_battery(None)], library);
    assert!(offer_with_witness_reach(
        &control,
        &auto_cast(&control, spell)
    ));
    assert_battery_completes(control, spell, &sources, bears);
}
