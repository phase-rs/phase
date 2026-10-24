//! "Put into [your | an opponent's] graveyard" triggers scope the card's OWNER, not
//! the controller it had when it died (loop card #41, Daretti, Scrap Savant's -10
//! emblem and the rest of its class).
//!
//! CR 400.3: an object that would go to a graveyard other than its owner's goes to
//! its owner's graveyard, so "put into your graveyard" is true exactly when the
//! moving card's OWNER is "you". CR 109.5: "you"/"your" on a triggered ability is
//! the controller of its source (an emblem's controller is the player who got it,
//! CR 114.2). A subject's own "you control" restriction (CR 603.10a look-back) is a
//! separate axis that stays on the controller.
//!
//! Every ownership/control split below is made real before the death: stolen cards
//! by casting Metallic Mastery / Hijack / Threaten (CR 613.1b), and cards an opponent controls
//! through `CardBuilder::controlled_by` (which also sets `base_controller`, so Layer
//! 2 does not hand control back before the death). The controller is asserted
//! immediately before each death. Test cards use their real Oracle text; the
//! vanilla artifacts and creatures that merely die carry no text. Three fixtures are
//! labelled: Volrath is a Vanguard card with no battlefield form, so its verbatim
//! ability is hosted on an enchantment; tokens are made with `is_token`; Bloodchief
//! Ascension's three quest counters are placed with `with_counter`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// --- Verbatim Oracle text (data/mtgjson/AtomicCards.json) -------------------------------------

const DARETTI: &str = "[+2]: Discard up to two cards, then draw that many cards.\n[\u{2212}2]: Sacrifice an artifact. If you do, return target artifact card from your graveyard to the battlefield.\n[\u{2212}10]: You get an emblem with \"Whenever an artifact is put into your graveyard from the battlefield, return that card to the battlefield at the beginning of the next end step.\"\nDaretti, Scrap Savant can be your commander.";
const SHATTER: &str = "Destroy target artifact.";
const MURDER: &str = "Destroy target creature.";
const SHATTERSTORM: &str = "Destroy all artifacts. They can't be regenerated.";
const METALLIC_MASTERY: &str = "Gain control of target artifact until end of turn. Untap that artifact. It gains haste until end of turn.";
const HIJACK: &str =
    "Gain control of target artifact or creature until end of turn. Untap it. It gains haste until end of turn.";
const MIND_ROT: &str = "Target player discards two cards.";
const VIRIDIAN_REVEL: &str =
    "Whenever an artifact is put into an opponent's graveyard from the battlefield, you may draw a card.";
const VORACIOUS_BROOD: &str = "This creature enters with a +1/+1 counter on it for each creature card in your graveyard.\nWhenever one or more creature cards are put into your graveyard from anywhere, put that many +1/+1 counters on this creature.";
const ULTRON: &str = "Flying\nWhenever Ultron enters or attacks, create a 2/2 colorless Robot Villain artifact creature token.\nWhenever another artifact is put into your graveyard from the battlefield or an artifact card is put into your graveyard from anywhere other than the battlefield, each opponent loses 1 life.";
const VOLRATH: &str = "Whenever a creature you control is put into your graveyard from the battlefield, you may put it on top of your library.";
const PITILESS_PLUNDERER: &str = "Whenever another creature you control dies, create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";
const PATRON: &str = "Rat offering (You may cast this spell any time you could cast an instant by sacrificing a Rat and paying the difference in mana costs between this and the sacrificed Rat. Mana cost includes color.)\nWhenever a permanent is put into an opponent's graveyard, that player loses 1 life.";
const BLOODCHIEF: &str = "At the beginning of each end step, if an opponent lost 2 or more life this turn, you may put a quest counter on this enchantment. (Damage causes loss of life.)\nWhenever a card is put into an opponent's graveyard from anywhere, if this enchantment has three or more quest counters on it, you may have that player lose 2 life. If you do, you gain 2 life.";
const DUSKMANTLE: &str = "{1}{U}{B}: Whenever a card is put into an opponent's graveyard from anywhere this turn, that player loses 1 life.\n{2}{U}{B}: Target player mills two cards.";
const LIABILITY: &str = "Whenever a nontoken permanent is put into a player's graveyard from the battlefield, that player loses 1 life.";
const MASSACRE_WURM: &str = "When this creature enters, creatures your opponents control get -2/-2 until end of turn.\nWhenever a creature an opponent controls dies, that player loses 2 life.";
const THREATEN: &str = "Untap target creature and gain control of it until end of turn. That creature gains haste until end of turn. (It can attack and {T} this turn.)";

// --- Harness ----------------------------------------------------------------------------------

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn pieces(state: &GameState, name: &str) -> Vec<(Zone, PlayerId, PlayerId)> {
    state
        .objects
        .values()
        .filter(|obj| obj.name == name)
        .map(|obj| (obj.zone, obj.owner, obj.controller))
        .collect()
}

/// The single object named `name`: (zone, owner, controller).
fn only(state: &GameState, name: &str) -> (Zone, PlayerId, PlayerId) {
    let found = pieces(state, name);
    assert_eq!(found.len(), 1, "expected exactly one {name}: {found:?}");
    found[0]
}

fn assert_in_graveyard_of(runner: &GameRunner, name: &str, owner: PlayerId) {
    let (zone, card_owner, _) = only(runner.state(), name);
    assert_eq!(zone, Zone::Graveyard, "{name} must be in a graveyard");
    assert_eq!(card_owner, owner, "{name} must be in its owner's graveyard");
}

fn assert_on_battlefield_under(runner: &GameRunner, name: &str, controller: PlayerId) {
    let (zone, _, card_controller) = only(runner.state(), name);
    assert_eq!(zone, Zone::Battlefield, "{name} must be on the battlefield");
    assert_eq!(card_controller, controller, "{name} controller");
}

/// Assert the controller of `id` right before a death (guards against a fixture whose
/// control change was silently undone).
fn assert_controller(runner: &GameRunner, id: ObjectId, expected: PlayerId) {
    let obj = &runner.state().objects[&id];
    assert_eq!(
        (obj.zone, obj.controller),
        (Zone::Battlefield, expected),
        "{} must be on the battlefield under {expected:?} before the death",
        obj.name
    );
}

/// Trigger instances created and not yet resolved: on the stack, awaiting ordering, or
/// deferred.
fn trigger_instances(state: &GameState) -> usize {
    let on_stack = state
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .count();
    let pending = state
        .pending_trigger_order
        .as_ref()
        .map(|order| {
            order
                .groups
                .iter()
                .map(|group| group.triggers.len())
                .sum::<usize>()
        })
        .unwrap_or(0);
    on_stack + pending + state.deferred_triggers.len()
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

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn optional_prompt_pending(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    )
}

/// Pass priority until the stack is empty, ordering simultaneous triggers by identity.
/// Returns early (with the prompt still pending) at an optional-effect choice.
fn drain(runner: &mut GameRunner) {
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is accepted");
            }
            other => panic!("unexpected prompt while draining the stack: {other:?}"),
        }
    }
    panic!("stack did not drain");
}

fn accept_optional(runner: &mut GameRunner) {
    assert!(
        optional_prompt_pending(runner),
        "the optional \"you may\" prompt must be pending, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional effect is accepted");
    drain(runner);
}

/// Cast `spell` through the production pipeline and pass priority only until the spell
/// itself has resolved. Triggers it caused are left waiting (stack / ordering queue) so
/// the caller can count them.
fn cast_until_resolved(runner: &mut GameRunner, spell: ObjectId, targets: &[ObjectId]) {
    let mut cast = runner.cast(spell).target_objects(targets).commit();
    for _ in 0..8 {
        if !cast
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell)
        {
            return;
        }
        cast.act(GameAction::PassPriority)
            .expect("passing priority is accepted");
    }
    panic!("spell {spell:?} never left the stack");
}

/// Cast a control-change spell on `target`, settle, and assert `thief` controls it.
fn steal(runner: &mut GameRunner, spell: ObjectId, target: ObjectId, thief: PlayerId) {
    cast_until_resolved(runner, spell, &[target]);
    drain(runner);
    assert_controller(runner, target, thief);
}

/// Activate Daretti's -10 through the production activation pipeline and confirm the
/// emblem exists. Daretti then sits at 0 loyalty (CR 704.5i).
fn get_daretti_emblem(runner: &mut GameRunner, daretti: ObjectId) {
    let index = runner.state().objects[&daretti]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::CreateEmblem { .. }))
        .expect("Daretti has an emblem-creating loyalty ability");
    runner.activate(daretti, index).resolve();
    assert_eq!(runner.state().command_zone.len(), 1, "one emblem exists");
    let emblem = runner.state().command_zone[0];
    assert!(runner.state().objects[&emblem].is_emblem);
    assert_eq!(runner.state().objects[&emblem].controller, P0);
}

fn run_to_end_step_and_resolve(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "game must be waiting for priority, got {:?}",
        runner.state().waiting_for
    );
}

// --- Daretti, Scrap Savant -10 emblem ------------------------------------------------------------

/// D0 (positive): your own artifact dies -> the emblem fires and the card returns at the
/// next end step (CR 400.3, CR 603.7, CR 114.4).
#[test]
fn d0_own_artifact_dies_emblem_fires_and_card_returns() {
    let mut s = scenario();
    let daretti = s
        .add_planeswalker_from_oracle(P0, "Daretti, Scrap Savant", "Daretti", 10, DARETTI)
        .id();
    let relic = s.add_artifact_from_oracle(P0, "Relic Alpha", "").id();
    let shatter = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();

    get_daretti_emblem(&mut runner, daretti);
    assert_controller(&runner, relic, P0);
    cast_until_resolved(&mut runner, shatter, &[relic]);

    assert_in_graveyard_of(&runner, "Relic Alpha", P0);
    assert_eq!(trigger_instances(runner.state()), 1, "one emblem trigger");
    let source = runner.state().stack[0].source_id;
    assert!(
        runner.state().objects[&source].is_emblem,
        "the trigger comes from the emblem"
    );
    drain(&mut runner);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    run_to_end_step_and_resolve(&mut runner);
    assert_on_battlefield_under(&runner, "Relic Alpha", P0);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// D1: an artifact you control but do not own dies -> it goes to its owner's graveyard,
/// not yours, so the emblem does NOT fire (CR 400.3, CR 109.5).
#[test]
fn d1_stolen_artifact_does_not_trigger_the_emblem() {
    let mut s = scenario();
    let daretti = s
        .add_planeswalker_from_oracle(P0, "Daretti, Scrap Savant", "Daretti", 10, DARETTI)
        .id();
    let stolen = s.add_artifact_from_oracle(P1, "Relic Stolen", "").id();
    let own = s.add_artifact_from_oracle(P0, "Relic Own", "").id();
    let mastery = s
        .add_spell_to_hand_from_oracle(P0, "Metallic Mastery", false, METALLIC_MASTERY)
        .id();
    let shatter_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let shatter_own = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();

    get_daretti_emblem(&mut runner, daretti);
    steal(&mut runner, mastery, stolen, P0);

    cast_until_resolved(&mut runner, shatter_stolen, &[stolen]);
    assert_in_graveyard_of(&runner, "Relic Stolen", P1);
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "CR 400.3: it went to the OPPONENT's graveyard, so no emblem trigger"
    );
    assert!(runner.state().delayed_triggers.is_empty());

    // Reach guard: the same emblem fires for the artifact P0 owns.
    cast_until_resolved(&mut runner, shatter_own, &[own]);
    assert_in_graveyard_of(&runner, "Relic Own", P0);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);

    run_to_end_step_and_resolve(&mut runner);
    assert_on_battlefield_under(&runner, "Relic Own", P0);
    assert_in_graveyard_of(&runner, "Relic Stolen", P1);
}

/// D2: your artifact that an opponent controls dies -> it goes to YOUR graveyard, so the
/// emblem fires and the card returns under your control (CR 400.3, CR 603.7e, CR 110.2a).
#[test]
fn d2_own_artifact_under_opponent_control_triggers_the_emblem() {
    let mut s = scenario();
    let daretti = s
        .add_planeswalker_from_oracle(P0, "Daretti, Scrap Savant", "Daretti", 10, DARETTI)
        .id();
    let taken = s
        .add_artifact_from_oracle(P0, "Relic Taken", "")
        .controlled_by(P1)
        .id();
    let shatter = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();

    get_daretti_emblem(&mut runner, daretti);
    assert_controller(&runner, taken, P1);
    cast_until_resolved(&mut runner, shatter, &[taken]);

    assert_in_graveyard_of(&runner, "Relic Taken", P0);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "CR 400.3: it went to MY graveyard, so the emblem triggers"
    );
    drain(&mut runner);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    run_to_end_step_and_resolve(&mut runner);
    assert_on_battlefield_under(&runner, "Relic Taken", P0);
}

/// D3 (preservation): an opponent's own artifact dies -> no trigger.
#[test]
fn d3_opponents_own_artifact_does_not_trigger_the_emblem() {
    let mut s = scenario();
    let daretti = s
        .add_planeswalker_from_oracle(P0, "Daretti, Scrap Savant", "Daretti", 10, DARETTI)
        .id();
    let theirs = s.add_artifact_from_oracle(P1, "Relic Theirs", "").id();
    let own = s.add_artifact_from_oracle(P0, "Relic Own", "").id();
    let shatter_theirs = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let shatter_own = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();

    get_daretti_emblem(&mut runner, daretti);
    cast_until_resolved(&mut runner, shatter_theirs, &[theirs]);
    assert_in_graveyard_of(&runner, "Relic Theirs", P1);
    assert_eq!(trigger_instances(runner.state()), 0);
    assert!(runner.state().delayed_triggers.is_empty());

    // Reach guard: the emblem is live and fires for P0's own artifact.
    cast_until_resolved(&mut runner, shatter_own, &[own]);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
}

/// D4: a token artifact you own dies -> the emblem triggers (CR 111.7: a token that
/// changes zones triggers abilities before it ceases to exist), but nothing returns
/// (CR 111.8, CR 704.5d).
#[test]
fn d4_token_artifact_triggers_but_nothing_returns() {
    let mut s = scenario();
    let daretti = s
        .add_planeswalker_from_oracle(P0, "Daretti, Scrap Savant", "Daretti", 10, DARETTI)
        .id();
    let token = s.add_artifact_from_oracle(P0, "Relic Token", "").id();
    let shatter = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();
    // Fixture: this relic is a token.
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;

    get_daretti_emblem(&mut runner, daretti);
    cast_until_resolved(&mut runner, shatter, &[token]);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "the token's death still triggers the emblem"
    );
    drain(&mut runner);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    run_to_end_step_and_resolve(&mut runner);
    assert!(
        pieces(runner.state(), "Relic Token")
            .iter()
            .all(|(zone, _, _)| *zone != Zone::Battlefield),
        "a token that left the battlefield never comes back"
    );
    assert!(runner.state().delayed_triggers.is_empty());
}

/// D5 (preservation): an artifact CARD put into your graveyard from your hand is not a
/// battlefield death -> no new trigger (CR 700.4 origin).
#[test]
fn d5_artifact_card_discarded_from_hand_does_not_trigger_the_emblem() {
    let mut s = scenario();
    let daretti = s
        .add_planeswalker_from_oracle(P0, "Daretti, Scrap Savant", "Daretti", 10, DARETTI)
        .id();
    let field = s.add_artifact_from_oracle(P0, "Relic Field", "").id();
    let shatter = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mind_rot = s
        .add_spell_to_hand_from_oracle(P0, "Mind Rot", false, MIND_ROT)
        .id();
    let held = s
        .add_artifact_to_hand_from_oracle(P0, "Relic Held", "")
        .id();
    let filler = s.add_card_to_hand(P0, "Filler Card");
    let mut runner = s.build();

    get_daretti_emblem(&mut runner, daretti);
    // Reach guard: the emblem is live and fires for a battlefield death.
    cast_until_resolved(&mut runner, shatter, &[field]);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    // Hand is now exactly [Mind Rot, Relic Held, Filler Card]; casting Mind Rot leaves two
    // cards, so "discards two cards" has no choice.
    assert_eq!(hand_len(&runner, P0), 3);

    runner
        .cast(mind_rot)
        .target_player(P0)
        .discard(&[held, filler])
        .resolve();
    assert_in_graveyard_of(&runner, "Relic Held", P0);
    assert_in_graveyard_of(&runner, "Filler Card", P0);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the discard created no second delayed return"
    );
    assert_eq!(trigger_instances(runner.state()), 0);

    run_to_end_step_and_resolve(&mut runner);
    assert_on_battlefield_under(&runner, "Relic Field", P0);
    assert_in_graveyard_of(&runner, "Relic Held", P0);
}

/// D6: simultaneous deaths with mixed ownership in ONE event (CR 603.2c, CR 603.10a).
/// A: P0 owns+controls. B: P1 owns, P0 stole it. C: P0 owns, P1 controls. D: P1
/// owns+controls. Two triggers either way, but the IDENTITIES differ: the emblem must
/// return A and C (the cards that went to P0's graveyard), not A and B.
#[test]
fn d6_simultaneous_deaths_return_the_cards_that_went_to_your_graveyard() {
    let mut s = scenario();
    let daretti = s
        .add_planeswalker_from_oracle(P0, "Daretti, Scrap Savant", "Daretti", 10, DARETTI)
        .id();
    let a = s.add_artifact_from_oracle(P0, "Relic A", "").id();
    let b = s.add_artifact_from_oracle(P1, "Relic B", "").id();
    let c = s
        .add_artifact_from_oracle(P0, "Relic C", "")
        .controlled_by(P1)
        .id();
    let d = s.add_artifact_from_oracle(P1, "Relic D", "").id();
    let mastery = s
        .add_spell_to_hand_from_oracle(P0, "Metallic Mastery", false, METALLIC_MASTERY)
        .id();
    let wipe = s
        .add_spell_to_hand_from_oracle(P0, "Shatterstorm", false, SHATTERSTORM)
        .id();
    let mut runner = s.build();

    get_daretti_emblem(&mut runner, daretti);
    steal(&mut runner, mastery, b, P0);
    assert_controller(&runner, a, P0);
    assert_controller(&runner, b, P0);
    assert_controller(&runner, c, P1);
    assert_controller(&runner, d, P1);

    cast_until_resolved(&mut runner, wipe, &[]);
    assert_in_graveyard_of(&runner, "Relic A", P0);
    assert_in_graveyard_of(&runner, "Relic B", P1);
    assert_in_graveyard_of(&runner, "Relic C", P0);
    assert_in_graveyard_of(&runner, "Relic D", P1);
    assert_eq!(
        trigger_instances(runner.state()),
        2,
        "one emblem trigger per card that went to P0's graveyard"
    );
    drain(&mut runner);
    assert_eq!(runner.state().delayed_triggers.len(), 2);

    run_to_end_step_and_resolve(&mut runner);
    assert_on_battlefield_under(&runner, "Relic A", P0);
    assert_on_battlefield_under(&runner, "Relic C", P0);
    assert_in_graveyard_of(&runner, "Relic B", P1);
    assert_in_graveyard_of(&runner, "Relic D", P1);
}

// --- Siblings of the class -----------------------------------------------------------------------

/// S1: "an artifact is put into an opponent's graveyard from the battlefield" (Viridian
/// Revel) fires for an opponent-owned artifact you controlled when it died.
#[test]
fn s1_opponent_graveyard_trigger_fires_for_a_stolen_opponent_artifact() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Viridian Revel", VIRIDIAN_REVEL);
    let stolen = s.add_artifact_from_oracle(P1, "Relic Stolen", "").id();
    s.add_card_to_library_top(P0, "Library Filler");
    let mastery = s
        .add_spell_to_hand_from_oracle(P0, "Metallic Mastery", false, METALLIC_MASTERY)
        .id();
    let shatter = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();

    steal(&mut runner, mastery, stolen, P0);
    cast_until_resolved(&mut runner, shatter, &[stolen]);
    assert_in_graveyard_of(&runner, "Relic Stolen", P1);
    let hand_before = hand_len(&runner, P0);
    assert_eq!(trigger_instances(runner.state()), 1, "Revel triggers");
    drain(&mut runner);
    accept_optional(&mut runner);
    assert_eq!(hand_len(&runner, P0), hand_before + 1, "P0 drew a card");
}

/// S2: ...and does NOT fire for an artifact you own that an opponent controls (it went to
/// YOUR graveyard).
#[test]
fn s2_opponent_graveyard_trigger_ignores_own_artifact_under_opponent_control() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Viridian Revel", VIRIDIAN_REVEL);
    let taken = s
        .add_artifact_from_oracle(P0, "Relic Taken", "")
        .controlled_by(P1)
        .id();
    let theirs = s.add_artifact_from_oracle(P1, "Relic Theirs", "").id();
    s.add_card_to_library_top(P0, "Library Filler");
    let shatter_taken = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let shatter_theirs = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();

    assert_controller(&runner, taken, P1);
    cast_until_resolved(&mut runner, shatter_taken, &[taken]);
    assert_in_graveyard_of(&runner, "Relic Taken", P0);
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "it went to P0's own graveyard, not an opponent's"
    );
    drain(&mut runner);
    assert!(!optional_prompt_pending(&runner));

    // Reach guard: Revel is live and fires when an opponent's own artifact dies.
    cast_until_resolved(&mut runner, shatter_theirs, &[theirs]);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
    assert!(optional_prompt_pending(&runner));
}

/// S3: the batched "one or more creature cards are put into your graveyard from anywhere"
/// (Voracious Brood) reads the owner. A creature P1 owns that P0 controlled dies: no
/// trigger instance. A creature P0 owns that P1 controlled dies: +1 counter.
#[test]
fn s3_batched_your_graveyard_trigger_reads_the_owner() {
    let mut s = scenario();
    let brood = s
        .add_creature_from_oracle(P0, "Voracious Brood", 1, 1, VORACIOUS_BROOD)
        .id();
    let stolen = s
        .add_creature(P1, "Bear Stolen", 2, 2)
        .controlled_by(P0)
        .id();
    let taken = s
        .add_creature(P0, "Bear Taken", 2, 2)
        .controlled_by(P1)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_taken = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    assert_controller(&runner, stolen, P0);
    cast_until_resolved(&mut runner, murder_stolen, &[stolen]);
    assert_in_graveyard_of(&runner, "Bear Stolen", P1);
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "the card went to P1's graveyard: no Brood trigger instance is created"
    );
    drain(&mut runner);
    assert_eq!(
        runner.state().objects[&brood]
            .counters
            .get(&CounterType::Plus1Plus1),
        None
    );

    assert_controller(&runner, taken, P1);
    cast_until_resolved(&mut runner, murder_taken, &[taken]);
    assert_in_graveyard_of(&runner, "Bear Taken", P0);
    assert_eq!(trigger_instances(runner.state()), 1, "Brood triggers");
    drain(&mut runner);
    assert_eq!(
        runner.state().objects[&brood]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "Brood gets one +1/+1 counter"
    );
}

/// S4: the disjunctive clause path (Ultron the Annihilator) reads the owner.
#[test]
fn s4_disjunctive_your_graveyard_trigger_reads_the_owner() {
    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Ultron the Annihilator", 4, 4, ULTRON)
        .as_artifact_creature();
    let stolen = s.add_artifact_from_oracle(P1, "Relic Stolen", "").id();
    let taken = s
        .add_artifact_from_oracle(P0, "Relic Taken", "")
        .controlled_by(P1)
        .id();
    let mastery = s
        .add_spell_to_hand_from_oracle(P0, "Metallic Mastery", false, METALLIC_MASTERY)
        .id();
    let shatter_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let shatter_taken = s
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, SHATTER)
        .id();
    let mut runner = s.build();

    steal(&mut runner, mastery, stolen, P0);
    let p1_life = life(&runner, P1);
    cast_until_resolved(&mut runner, shatter_stolen, &[stolen]);
    assert_in_graveyard_of(&runner, "Relic Stolen", P1);
    drain(&mut runner);
    assert_eq!(
        life(&runner, P1),
        p1_life,
        "a card in P1's graveyard is not an artifact put into P0's graveyard"
    );

    assert_controller(&runner, taken, P1);
    cast_until_resolved(&mut runner, shatter_taken, &[taken]);
    assert_in_graveyard_of(&runner, "Relic Taken", P0);
    drain(&mut runner);
    assert_eq!(
        life(&runner, P1),
        p1_life - 1,
        "the card went to P0's graveyard, so each opponent loses 1 life"
    );
}

// --- Subject restriction ("you control") still decides the look-back --------------------------

/// V1: "a creature you control is put into your graveyard from the battlefield" (Volrath,
/// hosted on a labelled fixture enchantment) fires when you own and control it.
#[test]
fn v1_subject_you_control_fires_for_your_own_creature() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Volrath", VOLRATH);
    let own = s.add_creature(P0, "Bear Own", 2, 2).id();
    let murder = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    cast_until_resolved(&mut runner, murder, &[own]);
    assert_in_graveyard_of(&runner, "Bear Own", P0);
    assert_eq!(trigger_instances(runner.state()), 1, "Volrath triggers");
    drain(&mut runner);
    accept_optional(&mut runner);
    let top = runner.state().players[0].library[0];
    assert_eq!(runner.state().objects[&top].name, "Bear Own");
    assert_eq!(runner.state().objects[&top].zone, Zone::Library);
}

/// V2 (preservation): ...does NOT fire for a creature you own that an opponent controls
/// (it fails "you control").
#[test]
fn v2_subject_you_control_ignores_your_creature_under_opponent_control() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Volrath", VOLRATH);
    let taken = s
        .add_creature(P0, "Bear Taken", 2, 2)
        .controlled_by(P1)
        .id();
    let own = s.add_creature(P0, "Bear Own", 2, 2).id();
    let murder_taken = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_own = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    assert_controller(&runner, taken, P1);
    cast_until_resolved(&mut runner, murder_taken, &[taken]);
    assert_in_graveyard_of(&runner, "Bear Taken", P0);
    assert_eq!(trigger_instances(runner.state()), 0);
    drain(&mut runner);
    assert!(!optional_prompt_pending(&runner));

    // Reach guard: Volrath is live and fires for a creature P0 owns and controls.
    cast_until_resolved(&mut runner, murder_own, &[own]);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
    assert!(optional_prompt_pending(&runner));
}

/// V3: ...does NOT fire for a creature you control but do not own (it fails "your
/// graveyard").
#[test]
fn v3_subject_you_control_ignores_a_stolen_creature() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Volrath", VOLRATH);
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let own = s.add_creature(P0, "Bear Own", 2, 2).id();
    let hijack = s
        .add_spell_to_hand_from_oracle(P0, "Hijack", false, HIJACK)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_own = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    steal(&mut runner, hijack, stolen, P0);
    cast_until_resolved(&mut runner, murder_stolen, &[stolen]);
    assert_in_graveyard_of(&runner, "Bear Stolen", P1);
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "it went to P1's graveyard, not P0's"
    );
    drain(&mut runner);
    assert!(!optional_prompt_pending(&runner));

    // Reach guard: Volrath is live and fires for a creature P0 owns and controls.
    cast_until_resolved(&mut runner, murder_own, &[own]);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
    assert!(optional_prompt_pending(&runner));
}

/// K1 (preservation): the ordinary dies look-back is untouched: "another creature you
/// control dies" fires for a creature that was stolen (CR 700.4, CR 603.10a).
#[test]
fn k1_creature_you_control_dies_still_fires_for_a_stolen_creature() {
    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Pitiless Plunderer", 1, 4, PITILESS_PLUNDERER);
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let hijack = s
        .add_spell_to_hand_from_oracle(P0, "Hijack", false, HIJACK)
        .id();
    let murder = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    let treasures = |runner: &GameRunner| {
        pieces(runner.state(), "Treasure")
            .iter()
            .filter(|(zone, _, controller)| *zone == Zone::Battlefield && *controller == P0)
            .count()
    };
    assert_eq!(treasures(&runner), 0);
    steal(&mut runner, hijack, stolen, P0);
    cast_until_resolved(&mut runner, murder, &[stolen]);
    assert_in_graveyard_of(&runner, "Bear Stolen", P1);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
    assert_eq!(treasures(&runner), 1, "P0 got a Treasure token");
}

// --- "that player" on an owner-scoped graveyard head names the card's OWNER ----------------------
//
// Loop card #41, U2. "Whenever a permanent is put into an opponent's graveyard, that
// player loses 1 life" (Patron of the Nezumi, Bloodchief Ascension, Duskmantle Guildmage,
// Liability, The Dark Barony): CR 400.3 makes the player whose graveyard it is the moving
// card's OWNER, and by the rules of English (CR 608.2c) "that player" is that same player,
// never the controller the card had when it died (a thief, or a controller-by-effect).
// The controller reading stays correct where the head names no graveyard player: Massacre
// Wurm's "a creature an opponent controls dies, that player ..." (CR 603.10a look-back);
// R8 pins it. Every row asserts BOTH players' life deltas and ends on an empty stack.

fn lives(runner: &GameRunner) -> (i32, i32) {
    (life(runner, P0), life(runner, P1))
}

/// The life totals moved by exactly `delta` = (P0, P1) since `before`.
fn assert_life_delta(runner: &GameRunner, before: (i32, i32), delta: (i32, i32), what: &str) {
    let now = lives(runner);
    assert_eq!(
        (now.0 - before.0, now.1 - before.1),
        delta,
        "{what}: life delta (P0, P1) from {before:?} to {now:?}"
    );
}

/// Nothing left to resolve: empty stack, no queued triggers, and a priority window.
fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert_eq!(trigger_instances(runner.state()), 0, "no queued triggers");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "must be waiting for priority, got {:?}",
        runner.state().waiting_for
    );
}

fn assert_optional_prompt_for(runner: &GameRunner, expected: PlayerId) {
    let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for else {
        panic!(
            "the optional \"you may\" prompt must be pending, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, expected, "who is asked \"you may have ...\"");
}

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

/// Murder `victim` through the production pipeline and return after the spell resolved,
/// with the triggers it caused left waiting.
fn murder(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId, owner: PlayerId) {
    cast_until_resolved(runner, spell, &[victim]);
    let name = runner
        .state()
        .objects
        .get(&victim)
        .map(|obj| obj.name.clone());
    if let Some(name) = name {
        assert_in_graveyard_of(runner, &name, owner);
    }
}

/// R1 (+R1b guard): Patron of the Nezumi. An opponent's card that died under YOUR control
/// went to the opponent's graveyard (CR 400.3), so the OWNER loses the life.
#[test]
fn r1_patron_stolen_opponent_card_makes_its_owner_lose_life() {
    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Patron of the Nezumi", 6, 6, PATRON);
    let own = s.add_creature(P1, "Bear Own", 2, 2).id();
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_own = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    // R1b reach guard: a card P1 owns and controls dies -> P1 loses 1 (green either way).
    let before = lives(&runner);
    assert_controller(&runner, own, P1);
    murder(&mut runner, murder_own, own, P1);
    assert_eq!(trigger_instances(runner.state()), 1, "Patron triggers");
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -1), "R1b P1/P1 card");

    // R1: P1's card under P0's control dies -> it went to P1's graveyard, P1 loses 1.
    steal(&mut runner, threaten, stolen, P0);
    let before = lives(&runner);
    murder(&mut runner, murder_stolen, stolen, P1);
    assert_eq!(trigger_instances(runner.state()), 1, "Patron triggers");
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -1), "R1 stolen card: the OWNER loses");
}

/// R2 (preservation): a card YOU own that an opponent controls dies -> it went to YOUR
/// graveyard, so Patron does not trigger at all (CR 400.3).
#[test]
fn r2_patron_own_card_under_opponent_control_does_not_trigger() {
    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Patron of the Nezumi", 6, 6, PATRON);
    let taken = s
        .add_creature(P0, "Bear Taken", 2, 2)
        .controlled_by(P1)
        .id();
    let theirs = s.add_creature(P1, "Bear Theirs", 2, 2).id();
    let murder_taken = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_theirs = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    assert_controller(&runner, taken, P1);
    let before = lives(&runner);
    murder(&mut runner, murder_taken, taken, P0);
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "it went to P0's own graveyard, not an opponent's"
    );
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(
        &runner,
        before,
        (0, 0),
        "R2 own card under opponent control",
    );

    // Reach guard: Patron is live and fires for a card P1 owns and controls.
    let before = lives(&runner);
    murder(&mut runner, murder_theirs, theirs, P1);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -1), "R2 reach guard");
}

/// R3: a stolen opponent TOKEN. Patron counts tokens ("a permanent", CR 110.1); a token
/// ceases to exist as an SBA after it triggers (CR 111.7, CR 704.5d), so by the time the
/// trigger resolves there is no live object to read -- the owner (the token's creator,
/// CR 111.2) must come from the zone-change record.
#[test]
fn r3_patron_stolen_opponent_token_makes_its_owner_lose_life() {
    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Patron of the Nezumi", 6, 6, PATRON);
    let token = s.add_creature(P1, "Rat Token", 1, 1).id();
    let own = s.add_creature(P1, "Bear Own", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_token = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_own = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();
    // Fixture: this creature is a token.
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;

    // R1b-style reach guard: the trigger is live for a card P1 owns and controls.
    let before = lives(&runner);
    murder(&mut runner, murder_own, own, P1);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain(&mut runner);
    assert_life_delta(&runner, before, (0, -1), "R3 reach guard");

    steal(&mut runner, threaten, token, P0);
    let before = lives(&runner);
    cast_until_resolved(&mut runner, murder_token, &[token]);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "the token's death triggers Patron (CR 111.7)"
    );
    assert!(
        !runner.state().objects.contains_key(&token),
        "the token has ceased to exist (CR 704.5d): the trigger cannot read a live object"
    );
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -1), "R3 stolen token: its owner loses");
}

/// R4: simultaneous deaths with mixed ownership (CR 603.2c, CR 603.10a). Four artifacts
/// die to one Shatterstorm: A (P1 owns, P1 controls), B (P1 owns, stolen by P0),
/// C (P0 owns, P1 controls), D (P0 owns, P0 controls). A and B went to P1's graveyard.
#[test]
fn r4_patron_simultaneous_mixed_ownership_deaths() {
    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Patron of the Nezumi", 6, 6, PATRON);
    let a = s.add_artifact_from_oracle(P1, "Relic A", "").id();
    let b = s.add_artifact_from_oracle(P1, "Relic B", "").id();
    let c = s
        .add_artifact_from_oracle(P0, "Relic C", "")
        .controlled_by(P1)
        .id();
    let d = s.add_artifact_from_oracle(P0, "Relic D", "").id();
    let mastery = s
        .add_spell_to_hand_from_oracle(P0, "Metallic Mastery", false, METALLIC_MASTERY)
        .id();
    let wipe = s
        .add_spell_to_hand_from_oracle(P0, "Shatterstorm", false, SHATTERSTORM)
        .id();
    let mut runner = s.build();

    steal(&mut runner, mastery, b, P0);
    assert_controller(&runner, a, P1);
    assert_controller(&runner, c, P1);
    assert_controller(&runner, d, P0);
    let before = lives(&runner);

    cast_until_resolved(&mut runner, wipe, &[]);
    assert_in_graveyard_of(&runner, "Relic A", P1);
    assert_in_graveyard_of(&runner, "Relic B", P1);
    assert_in_graveyard_of(&runner, "Relic C", P0);
    assert_in_graveyard_of(&runner, "Relic D", P0);
    assert_eq!(
        trigger_instances(runner.state()),
        2,
        "one Patron trigger per artifact that went to P1's graveyard"
    );
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -2), "R4 mixed simultaneous deaths");
}

fn bloodchief_board_with_quest_counters(counters: u32) -> (GameScenario, ObjectId) {
    let mut s = scenario();
    let ascension = s
        .add_enchantment_from_oracle(P0, "Bloodchief Ascension", BLOODCHIEF)
        .id();
    s.with_counter(ascension, CounterType::Generic("quest".into()), counters);
    (s, ascension)
}

fn bloodchief_board() -> (GameScenario, ObjectId) {
    bloodchief_board_with_quest_counters(3)
}

/// R5 (+R5b guard): Bloodchief Ascension with three quest counters. "You may have that
/// player lose 2 life. If you do, you gain 2 life": P0 is asked (CR 608.2d); the player
/// who loses is the OWNER of the card that went to the opponent's graveyard.
#[test]
fn r5_bloodchief_stolen_opponent_card_makes_its_owner_lose_life() {
    let (mut s, _ascension) = bloodchief_board();
    let own = s.add_creature(P1, "Bear Own", 2, 2).id();
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_own = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    // R5b reach guard: P1 owns and controls the card.
    let before = lives(&runner);
    murder(&mut runner, murder_own, own, P1);
    assert_eq!(trigger_instances(runner.state()), 1, "Bloodchief triggers");
    drain(&mut runner);
    assert_optional_prompt_for(&runner, P0);
    accept_optional(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (2, -2), "R5b P1/P1 card");

    // R5: P1's card under P0's control.
    steal(&mut runner, threaten, stolen, P0);
    let before = lives(&runner);
    murder(&mut runner, murder_stolen, stolen, P1);
    assert_eq!(trigger_instances(runner.state()), 1, "Bloodchief triggers");
    drain(&mut runner);
    assert_optional_prompt_for(&runner, P0);
    accept_optional(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (2, -2), "R5 stolen card: the OWNER loses");
}

/// R9a (intervening "if", both halves in one board): the owner-keyed head
/// ("put into an opponent's graveyard") is paired with the condition "if this enchantment
/// has three or more quest counters on it" (CR 603.4: checked when the ability would
/// trigger).
///
/// First death, two quest counters: a P1-owned card stolen by P0 dies into P1's graveyard.
/// The condition is false, so nothing is put on the stack, nobody is asked, and neither
/// life total moves. On its own this half cannot tell the head matching from the head not
/// matching, because both give zero triggers.
///
/// Second death, after a third quest counter is added: a second stolen P1-owned card dies
/// the same way. Now the condition holds, so exactly one trigger appears, P0 is asked,
/// and accepting makes the card's OWNER lose 2 life while P0 gains 2. That one trigger
/// proves the owner-keyed head does match a card P0 controlled when it died, so the row
/// fails if either the owner scope or the condition path regresses. R5 is the same
/// three-counter board without the preceding two-counter death.
#[test]
fn r9a_bloodchief_two_quest_counters_do_not_trigger_the_third_does() {
    let (mut s, ascension) = bloodchief_board_with_quest_counters(2);
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let stolen_again = s.add_creature(P1, "Bear Stolen Again", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let threaten_again = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_again = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    // Two quest counters: the intervening "if" is false.
    steal(&mut runner, threaten, stolen, P0);
    let before = lives(&runner);
    // `murder` asserts the card went to its owner's (P1's) graveyard.
    murder(&mut runner, murder_stolen, stolen, P1);
    assert_in_graveyard_of(&runner, "Bear Stolen", P1);
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "two quest counters: the intervening \"if\" is false, nothing triggers"
    );
    drain(&mut runner);
    assert!(
        !optional_prompt_pending(&runner),
        "nobody is asked \"you may have that player lose 2 life\""
    );
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, 0), "R9a two quest counters");

    // The third quest counter, placed the way `with_counter` places the board's counters.
    *runner
        .state_mut()
        .objects
        .get_mut(&ascension)
        .expect("Bloodchief Ascension is on the battlefield")
        .counters
        .entry(CounterType::Generic("quest".into()))
        .or_insert(0) += 1;
    assert_eq!(
        runner.state().objects[&ascension].counters[&CounterType::Generic("quest".into())],
        3,
        "Bloodchief Ascension now has three quest counters"
    );

    // Three quest counters: the same route now triggers, so the head did match the owner.
    steal(&mut runner, threaten_again, stolen_again, P0);
    let before = lives(&runner);
    murder(&mut runner, murder_again, stolen_again, P1);
    assert_in_graveyard_of(&runner, "Bear Stolen Again", P1);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "three quest counters: Bloodchief triggers for the stolen opponent-owned card"
    );
    drain(&mut runner);
    assert_optional_prompt_for(&runner, P0);
    accept_optional(&mut runner);
    assert_settled(&runner);
    assert_life_delta(
        &runner,
        before,
        (2, -2),
        "R9a three quest counters: the OWNER loses",
    );
}

/// R9b (decline, "you may"): with three quest counters the ability triggers and P0 is asked
/// (CR 608.2d). Declining means the opponent-owned card's owner does not lose 2 life, and
/// "If you do, you gain 2 life" does not happen, so neither life total moves. The reach guard
/// is that the prompt was offered to P0 for a stolen card that went to P1's graveyard.
#[test]
fn r9b_bloodchief_decline_changes_no_life_total() {
    let (mut s, _ascension) = bloodchief_board();
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    steal(&mut runner, threaten, stolen, P0);
    let before = lives(&runner);
    murder(&mut runner, murder_stolen, stolen, P1);
    assert_eq!(trigger_instances(runner.state()), 1, "Bloodchief triggers");
    drain(&mut runner);
    // Reach guard: the optional prompt was offered, to the controller of the Ascension.
    assert_optional_prompt_for(&runner, P0);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional effect is accepted");
    drain(&mut runner);
    assert!(
        !optional_prompt_pending(&runner),
        "the optional prompt is resolved by declining"
    );
    assert_settled(&runner);
    assert_in_graveyard_of(&runner, "Bear Stolen", P1);
    assert_life_delta(&runner, before, (0, 0), "R9b declined: no loss, no gain");
}

fn duskmantle_board() -> (GameScenario, ObjectId) {
    let mut s = scenario();
    let guildmage = s
        .add_creature_from_oracle(P0, "Duskmantle Guildmage", 2, 2, DUSKMANTLE)
        .id();
    // {1}{U}{B}, paid from the pool (auto-tap is not modelled by the activation driver).
    s.with_mana_pool(
        P0,
        vec![
            mana(ManaType::Blue),
            mana(ManaType::Black),
            mana(ManaType::Red),
        ],
    );
    (s, guildmage)
}

/// The index of Duskmantle Guildmage's delayed-trigger ability ({1}{U}{B}), found by its
/// effect, never by position.
fn duskmantle_delayed_ability(runner: &GameRunner, guildmage: ObjectId) -> usize {
    runner.state().objects[&guildmage]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::CreateDelayedTrigger { .. }))
        .expect("{1}{U}{B} creates a delayed trigger")
}

/// R6b: Duskmantle Guildmage's delayed trigger is live and reads the event. Two cards
/// discarded into P1's graveyard (hand -> graveyard, P1 owns them) -> two triggers; P1
/// loses 2 and P0, who activated the ability, loses nothing (the delayed trigger's
/// creation must not capture a referent: CR 603.7, CR 603.7e).
#[test]
fn r6b_duskmantle_delayed_trigger_reads_the_discarded_cards_owner() {
    let (mut s, guildmage) = duskmantle_board();
    let held_a = s.add_card_to_hand(P1, "Hand A");
    let held_b = s.add_card_to_hand(P1, "Hand B");
    let mind_rot = s
        .add_spell_to_hand_from_oracle(P0, "Mind Rot", false, MIND_ROT)
        .id();
    let mut runner = s.build();
    let delayed = duskmantle_delayed_ability(&runner, guildmage);

    let before = lives(&runner);
    runner.activate(guildmage, delayed).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the {{1}}{{U}}{{B}} activation created one delayed trigger"
    );
    assert_life_delta(
        &runner,
        before,
        (0, 0),
        "R6b creation of the delayed trigger",
    );

    runner
        .cast(mind_rot)
        .target_player(P1)
        .discard(&[held_a, held_b])
        .resolve();
    drain(&mut runner);
    assert_in_graveyard_of(&runner, "Hand A", P1);
    assert_in_graveyard_of(&runner, "Hand B", P1);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -2), "R6b two discarded cards");
}

/// R6: ...and a card P1 owns that died under P0's control went to P1's graveyard, so P1
/// (the owner), not the thief, loses the life.
#[test]
fn r6_duskmantle_delayed_trigger_stolen_opponent_card_makes_its_owner_lose_life() {
    let (mut s, guildmage) = duskmantle_board();
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();
    let delayed = duskmantle_delayed_ability(&runner, guildmage);

    runner.activate(guildmage, delayed).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    steal(&mut runner, threaten, stolen, P0);
    let before = lives(&runner);
    murder(&mut runner, murder_stolen, stolen, P1);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "the delayed trigger fires for the card put into P1's graveyard"
    );
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -1), "R6 stolen card: the OWNER loses");
}

/// R7a: Liability ("a player's graveyard"): a card P0 owns and controls -> P0 loses 1.
#[test]
fn r7a_liability_own_card_makes_its_owner_lose_life() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Liability", LIABILITY);
    let own = s.add_creature(P0, "Bear Own", 2, 2).id();
    let murder_own = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    let before = lives(&runner);
    murder(&mut runner, murder_own, own, P0);
    assert_eq!(trigger_instances(runner.state()), 1, "Liability triggers");
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (-1, 0), "R7a P0/P0 card");
}

/// R7b: a card P1 owns that P0 controls (stolen) went to P1's graveyard -> P1 loses 1.
#[test]
fn r7b_liability_stolen_card_makes_its_owner_lose_life() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Liability", LIABILITY);
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    steal(&mut runner, threaten, stolen, P0);
    let before = lives(&runner);
    murder(&mut runner, murder_stolen, stolen, P1);
    assert_eq!(trigger_instances(runner.state()), 1, "Liability triggers");
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -1), "R7b stolen card: the OWNER loses");
}

/// R7c: a card P0 owns that P1 controls went to P0's graveyard -> P0 loses 1.
#[test]
fn r7c_liability_own_card_under_opponent_control_makes_its_owner_lose_life() {
    let mut s = scenario();
    s.add_enchantment_from_oracle(P0, "Liability", LIABILITY);
    let taken = s
        .add_creature(P0, "Bear Taken", 2, 2)
        .controlled_by(P1)
        .id();
    let murder_taken = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    assert_controller(&runner, taken, P1);
    let before = lives(&runner);
    murder(&mut runner, murder_taken, taken, P0);
    assert_eq!(trigger_instances(runner.state()), 1, "Liability triggers");
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(
        &runner,
        before,
        (-1, 0),
        "R7c own card under opponent control",
    );
}

/// R8 (preservation): Massacre Wurm's "a creature an opponent controls dies, that player
/// loses 2 life" keeps the CONTROLLER reading (CR 603.10a look-back; the antecedent of
/// "that player" is the controlling opponent, CR 608.2c). The head names no graveyard
/// player, so the owner reading must not leak into it.
#[test]
fn r8_massacre_wurm_keeps_the_controller_reading() {
    let mut s = scenario();
    // Placed on the battlefield: its enters trigger does not run.
    s.add_creature_from_oracle(P0, "Massacre Wurm", 6, 5, MASSACRE_WURM);
    let own = s.add_creature(P1, "Bear Own", 2, 2).id();
    let taken = s
        .add_creature(P0, "Bear Taken", 2, 2)
        .controlled_by(P1)
        .id();
    let stolen = s.add_creature(P1, "Bear Stolen", 2, 2).id();
    let threaten = s
        .add_spell_to_hand_from_oracle(P0, "Threaten", false, THREATEN)
        .id();
    let murder_own = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_taken = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let murder_stolen = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();

    // (a) a creature P1 owns and controls dies -> P1 loses 2.
    let before = lives(&runner);
    murder(&mut runner, murder_own, own, P1);
    assert_eq!(trigger_instances(runner.state()), 1, "Wurm triggers");
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, -2), "R8a P1/P1 creature");

    // (b) a creature P0 owns that P1 controls dies: it went to P0's graveyard, but P1 was
    // the controlling opponent, so the CONTROLLER pays.
    assert_controller(&runner, taken, P1);
    let before = lives(&runner);
    murder(&mut runner, murder_taken, taken, P0);
    assert_eq!(trigger_instances(runner.state()), 1, "Wurm triggers");
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(
        &runner,
        before,
        (0, -2),
        "R8b own card under opponent control",
    );

    // (c) a creature P1 owns that P0 controls dies: P0 controlled it, so Wurm does not
    // trigger.
    steal(&mut runner, threaten, stolen, P0);
    let before = lives(&runner);
    murder(&mut runner, murder_stolen, stolen, P1);
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "Wurm's controller controlled the creature: no trigger"
    );
    drain(&mut runner);
    assert_settled(&runner);
    assert_life_delta(&runner, before, (0, 0), "R8c stolen creature");
}
