//! CR 607.2a + CR 607.3: "where X is the power of the exiled card" reads the
//! cards in exile linked to this object, not the per-turn "exiled with this
//! source this turn" ledger.
//!
//! Bishop of Binding (Oracle text, `data/mtgjson/AtomicCards.json`):
//!   "When this creature enters, exile target creature an opponent controls
//!    until this creature leaves the battlefield.
//!    Whenever this creature attacks, target Vampire gets +X/+X until end of
//!    turn, where X is the power of the exiled card."
//! Rulings (2018-01-19): "If there is no exiled card as Bishop of Binding's
//! last ability resolves, most likely because Bishop of Binding left the
//! battlefield while that ability was on the stack, X is 0." and "The value of
//! X is determined only as Bishop of Binding's second ability resolves."
//!
//! Redemptor Dreadnought (Oracle text, same source):
//!   "Fallen Warrior — As an additional cost to cast this spell, you may exile
//!    a creature card from your graveyard.
//!    Trample
//!    Plasma Incinerator — Whenever this creature attacks, if a card is exiled
//!    with it, it gets +X/+X until end of turn, where X is the power of the
//!    exiled card."
//!
//! Every Bishop row drives the real pipeline: casts, the ETB exile, turn
//! changes, declare-attackers, the attack trigger and stack resolution, then
//! reads the effective (post-layer) power and toughness. The Dreadnought rows
//! start on the battlefield; R4 seeds its link (a labelled reader row).
//!
//! The re-exile rows (R-B1, R-B2, R-B0) exile Bishop's card again with another
//! effect. CR 400.7 makes the card in exile a new object that Bishop's first
//! ability did not exile, so it is not "the exiled card" (CR 607.2a):
//!   Swords to Plowshares: "Exile target creature. Its controller gains life
//!    equal to its power."
//!   Relic of Progenitus: "{T}: Target player exiles a card from their
//!    graveyard.
//!    {1}, Exile this artifact: Exile all graveyards. Draw a card."
//!
//! Admonition Angel (Oracle text, same source):
//!   "Flying
//!    Landfall — Whenever a land you control enters, you may exile target
//!    nonland permanent other than this creature.
//!    When this creature leaves the battlefield, return all cards exiled with
//!    it to the battlefield under their owners' control."
//! Ruling (2010-03-01): "Admonition Angel's landfall ability and its last
//! ability are linked. The last ability refers only to cards exiled with its
//! landfall ability." The R-F rows drive the population of its
//! leaves-the-battlefield return (CR 607.2a + CR 603.10a).

use engine::game::players::linked_exile_cards_for_source;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turns::start_next_turn;
use engine::game::zones::move_to_zone;
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

/// Bishop of Binding's printed Oracle text.
const BISHOP_OF_BINDING: &str = "When this creature enters, exile target creature an opponent \
controls until this creature leaves the battlefield.\n\
Whenever this creature attacks, target Vampire gets +X/+X until end of turn, where X is the power \
of the exiled card.";

/// Redemptor Dreadnought's printed Oracle text.
const REDEMPTOR_DREADNOUGHT: &str = "Fallen Warrior — As an additional cost to cast this spell, \
you may exile a creature card from your graveyard.\n\
Trample\n\
Plasma Incinerator — Whenever this creature attacks, if a card is exiled with it, it gets +X/+X \
until end of turn, where X is the power of the exiled card.";

const UNSUMMON: &str = "Return target creature to its owner's hand.";

const SWORDS_TO_PLOWSHARES: &str =
    "Exile target creature. Its controller gains life equal to its power.";

const RELIC_OF_PROGENITUS: &str = "{T}: Target player exiles a card from their graveyard.\n\
{1}, Exile this artifact: Exile all graveyards. Draw a card.";

const ADMONITION_ANGEL: &str = "Flying\n\
Landfall — Whenever a land you control enters, you may exile target nonland permanent other than \
this creature.\n\
When this creature leaves the battlefield, return all cards exiled with it to the battlefield \
under their owners' control.";

const STRIONIC_RESONATOR: &str = "{2}, {T}: Copy target triggered ability you control. You may \
choose new targets for the copy. (A triggered ability uses the words \"when,\" \"whenever,\" or \
\"at.\")";

/// Bishop of Binding (cost-free, in hand), a 2/2 Vampire for P0, and the
/// opponent's 4/4 "Opp Brute" for its ETB to exile.
fn bishop_board(scenario: &mut GameScenario, haste: bool) -> (ObjectId, ObjectId, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    let bishop = {
        let mut builder = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Bishop of Binding",
            1,
            1,
            BISHOP_OF_BINDING,
        );
        builder.with_mana_cost(ManaCost::zero());
        if haste {
            builder.with_keyword(Keyword::Haste);
        }
        builder.id()
    };
    let vampire = {
        let mut builder = scenario.add_creature(P0, "Vampire Ally", 2, 2);
        builder.with_subtypes(vec!["Vampire"]);
        builder.id()
    };
    let brute = scenario.add_creature(P1, "Opp Brute", 4, 4).id();
    (bishop, vampire, brute)
}

fn linked_pile(state: &GameState, source: ObjectId) -> Vec<ObjectId> {
    linked_exile_cards_for_source(state, source)
        .iter()
        .map(|linked| linked.exiled_id)
        .collect()
}

fn has_until_source_leaves_link(state: &GameState, exiled: ObjectId, source: ObjectId) -> bool {
    state.exile_links.iter().any(|link| {
        link.exiled_id == exiled
            && link.source_id == source
            && matches!(link.kind, ExileLinkKind::UntilSourceLeaves { .. })
    })
}

/// Moves to the controller's next turn (opponent's turn, then P0's), and checks
/// the per-turn ledger is empty there, so the ledger cannot be what X reads.
fn to_controllers_next_turn(runner: &mut GameRunner, source: ObjectId) {
    let mut events = Vec::new();
    start_next_turn(runner.state_mut(), &mut events);
    start_next_turn(runner.state_mut(), &mut events);
    assert_eq!(runner.state().active_player, P0, "setup: P0's next turn");
    assert_eq!(
        runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&source),
        None,
        "reach guard: the per-turn ledger is empty on the later turn"
    );
}

fn declare_attack(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_phase(Phase::DeclareAttackers);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
}

fn settled(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty()
}

/// Answers target prompts with `targets` in order, accepts optional ("you may")
/// effects, and passes priority until `stop` holds. Panics on any prompt the
/// rows below do not expect.
fn drive_until(runner: &mut GameRunner, targets: &[ObjectId], stop: impl Fn(&GameState) -> bool) {
    let mut targets = targets.iter().copied();
    for _ in 0..64 {
        if stop(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::TargetSelection { .. }
            | WaitingFor::CopyRetarget { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(
                    targets.next().expect("a declared target for this prompt"),
                )),
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }
    panic!("drive_until did not reach its stop condition");
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &runner.state().objects[&id];
    (object.power, object.toughness)
}

/// R1, CR 607.2a: on a later turn the per-turn ledger is empty, but the 4/4 is
/// still the card exiled with Bishop, so X = 4 and the Vampire is 6/6.
#[test]
fn later_turn_attack_reads_the_etb_linked_card() {
    let mut scenario = GameScenario::new();
    let (bishop, vampire, brute) = bishop_board(&mut scenario, false);
    let mut runner = scenario.build();
    runner.cast(bishop).target_object(brute).resolve();
    assert_eq!(runner.state().objects[&brute].zone, Zone::Exile);
    to_controllers_next_turn(&mut runner, bishop);
    assert!(
        has_until_source_leaves_link(runner.state(), brute, bishop),
        "reach guard: the ETB's until-leaves link survives the turn change"
    );
    assert_eq!(linked_pile(runner.state(), bishop), vec![brute]);

    declare_attack(&mut runner, bishop);
    drive_until(&mut runner, &[vampire], settled);

    assert_eq!(
        power_toughness(&runner, vampire),
        (Some(6), Some(6)),
        "CR 607.2a: X is the power of the card exiled with Bishop (4) on any \
         later turn, so the Vampire is 2/2 + 4/4"
    );
}

/// R2, CR 607.3 + CR 707.10: a copied ETB exiles a second card; "the exiled
/// card" then means both, and X is the sum of their powers (4 + 3).
#[test]
fn two_linked_cards_sum_their_power() {
    let mut scenario = GameScenario::new();
    let (bishop, vampire, brute) = bishop_board(&mut scenario, true);
    let elf = scenario.add_creature(P1, "Opp Elf", 3, 3).id();
    let resonator = scenario
        .add_artifact_from_oracle(P0, "Strionic Resonator", STRIONIC_RESONATOR)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    // Bishop resolves; its ETB goes on the stack targeting the 4/4.
    runner.cast(bishop).commit();
    drive_until(&mut runner, &[brute], |state| {
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state
                .stack
                .last()
                .is_some_and(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
    });
    let etb = runner.state().stack.last().expect("Bishop's ETB").id;

    // CR 707.10: copy the ETB; the copy retargets the opponent's 3/3.
    runner.activate(resonator, 0).target_object(etb).resolve();
    drive_until(&mut runner, &[elf], settled);
    for exiled in [brute, elf] {
        assert_eq!(runner.state().objects[&exiled].zone, Zone::Exile);
        assert!(
            has_until_source_leaves_link(runner.state(), exiled, bishop),
            "reach guard: each ETB instance links its card to Bishop"
        );
    }

    declare_attack(&mut runner, bishop);
    drive_until(&mut runner, &[vampire], settled);

    assert_eq!(
        power_toughness(&runner, vampire),
        (Some(9), Some(9)),
        "CR 607.3: with two cards exiled with Bishop, X is the sum of their \
         powers (4 + 3), so the Vampire is 2/2 + 7/7"
    );
}

/// R3, ruling + CR 607.2a + CR 610.3: Bishop leaves with its attack trigger on
/// the stack, the 4/4 returns, and there is no exiled card, so X = 0.
#[test]
fn source_leaving_with_trigger_on_the_stack_reads_no_exiled_card() {
    let mut scenario = GameScenario::new();
    let (bishop, vampire, brute) = bishop_board(&mut scenario, true);
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(bishop).target_object(brute).resolve();
    assert_eq!(runner.state().objects[&brute].zone, Zone::Exile);

    declare_attack(&mut runner, bishop);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "setup: Bishop's attack trigger is on the stack"
    );

    // Unsummon Bishop in response; the outcome resolves Unsummon, then the
    // attack trigger.
    let outcome = runner.cast(unsummon).target_object(bishop).resolve();
    assert_eq!(runner.state().objects[&bishop].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&brute].zone,
        Zone::Battlefield,
        "positive: CR 610.3 returns the 4/4 when Bishop leaves"
    );
    assert!(settled(runner.state()), "the attack trigger resolved");
    // Positive resolution guard: Bishop's pump resolved (an `EffectResolved`
    // of kind Pump from Bishop in the resolving actions' events), so the 2/2
    // below is X = 0, not a removed or fizzled trigger.
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::Pump, source_id, .. }
                if *source_id == bishop
        )),
        "positive: Bishop's attack trigger resolved its pump"
    );

    assert_eq!(
        power_toughness(&runner, vampire),
        (Some(2), Some(2)),
        "ruling + CR 607.2a: the returned 4/4 is no longer an exiled card, so \
         X is 0 and the Vampire stays 2/2"
    );
}

/// R4, seeded-link reader row: Redemptor Dreadnought's X and its intervening-if
/// read the same linked pile on a later turn. The `TrackedBySource` link is
/// seeded (the state a cast-cost exile must produce, CR 607.2q); it stands in
/// for that writer until it lands.
#[test]
fn dreadnought_condition_and_x_read_the_linked_card_on_a_later_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreadnought = scenario
        .add_creature_from_oracle(P0, "Redemptor Dreadnought", 4, 4, REDEMPTOR_DREADNOUGHT)
        .id();
    let giant = scenario
        .add_creature_to_exile(P0, "Exiled Giant", 5, 5)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: giant,
        source_id: dreadnought,
        kind: ExileLinkKind::TrackedBySource,
    });
    to_controllers_next_turn(&mut runner, dreadnought);
    assert_eq!(linked_pile(runner.state(), dreadnought), vec![giant]);

    declare_attack(&mut runner, dreadnought);
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == dreadnought
        )),
        "reach guard: CR 603.4 — the intervening-if is true, so the attack \
         trigger is on the stack"
    );
    drive_until(&mut runner, &[], settled);

    assert_eq!(
        power_toughness(&runner, dreadnought),
        (Some(9), Some(9)),
        "CR 607.2a: X is the power of the card exiled with the Dreadnought (5)"
    );
}

/// G2, CR 603.4: a card in exile that is not linked to the Dreadnought makes
/// the intervening-if false, so the attack trigger never goes on the stack.
#[test]
fn dreadnought_with_no_linked_card_does_not_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreadnought = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Redemptor Dreadnought",
            4,
            4,
            REDEMPTOR_DREADNOUGHT,
        );
        builder.with_keyword(Keyword::Haste);
        builder.id()
    };
    let unlinked = scenario
        .add_creature_to_exile(P0, "Unlinked Giant", 5, 5)
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&unlinked].zone, Zone::Exile);
    assert!(linked_pile(runner.state(), dreadnought).is_empty());

    declare_attack(&mut runner, dreadnought);
    assert!(
        runner.state().stack.is_empty(),
        "CR 603.4: no card is exiled with it, so the ability does not trigger"
    );
    assert_eq!(power_toughness(&runner, dreadnought), (Some(4), Some(4)));
}

/// G5, re-entry smoke row (CR 400.7 + CR 610.3): Bishop exiles A, is bounced
/// (A returns) and recast; its new ETB exiles B. On the next turn X reads only
/// B. This is not an incarnation discriminator: A has left exile.
#[test]
fn reentered_bishop_reads_only_its_new_exiled_card() {
    let mut scenario = GameScenario::new();
    let (bishop, vampire, brute) = bishop_board(&mut scenario, false);
    let elf = scenario.add_creature(P1, "Opp Elf", 3, 3).id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(bishop).target_object(brute).resolve();
    assert_eq!(runner.state().objects[&brute].zone, Zone::Exile);
    runner.cast(unsummon).target_object(bishop).resolve();
    assert_eq!(runner.state().objects[&bishop].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&brute].zone, Zone::Battlefield);

    runner.cast(bishop).target_object(elf).resolve();
    assert_eq!(runner.state().objects[&elf].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&brute].zone, Zone::Battlefield);

    to_controllers_next_turn(&mut runner, bishop);
    declare_attack(&mut runner, bishop);
    drive_until(&mut runner, &[vampire], settled);

    assert_eq!(
        power_toughness(&runner, vampire),
        (Some(5), Some(5)),
        "X is the power of the card the current Bishop exiled (3), so the \
         Vampire is 2/2 + 3/3"
    );
}

/// Bishop's pump resolved (an `EffectResolved` of kind Pump from Bishop), so a
/// 2/2 Vampire is X = 0, not a removed or fizzled trigger.
fn bishop_pump_resolved(events: &[GameEvent], bishop: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::Pump, source_id, .. }
                if *source_id == bishop
        )
    })
}

/// Adds one colorless mana to P0's pool for Relic of Progenitus's `{1}`.
fn add_one_colorless(runner: &mut GameRunner) {
    runner
        .state_mut()
        .add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        )
        .expect("P0's pool takes the mana");
}

/// R-B1, ruling + CR 607.2a + CR 400.7: Bishop leaves with its attack trigger
/// on the stack, so the 4/4 returns (CR 610.3); Swords to Plowshares then
/// exiles it again. The card in exile is a new object that Bishop's first
/// ability did not exile, so there is no exiled card and X = 0, the same
/// answer as R3, where the 4/4 stays out of exile.
#[test]
fn card_exiled_again_after_the_source_left_is_not_the_exiled_card() {
    let mut scenario = GameScenario::new();
    let (bishop, vampire, brute) = bishop_board(&mut scenario, true);
    // Unsummon and Swords are cost-free here (a test setting; printed {U} and
    // {W}), and `bishop_board` gives Bishop haste so it attacks this turn.
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let swords = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_TO_PLOWSHARES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(bishop).target_object(brute).resolve();
    assert_eq!(runner.state().objects[&brute].zone, Zone::Exile);
    let exiled_by_bishop = runner.state().objects[&brute].incarnation;

    declare_attack(&mut runner, bishop);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "setup: Bishop's attack trigger is on the stack"
    );

    // Unsummon Bishop in response and resolve only Unsummon.
    runner.cast(unsummon).target_object(bishop).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&bishop].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&brute].zone,
        Zone::Battlefield,
        "setup: CR 610.3 returns the 4/4 when Bishop leaves"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "setup: the attack trigger is still on the stack"
    );

    // Swords exiles the 4/4 again; the outcome resolves Swords, then the trigger.
    let outcome = runner.cast(swords).target_object(brute).resolve();
    let brute_now = &runner.state().objects[&brute];
    assert_eq!(brute_now.zone, Zone::Exile, "setup: Swords exiled the 4/4");
    assert_ne!(
        brute_now.incarnation, exiled_by_bishop,
        "setup: CR 400.7 — the 4/4 in exile is a new object"
    );
    assert!(settled(runner.state()), "the attack trigger resolved");
    assert!(
        bishop_pump_resolved(outcome.events(), bishop),
        "positive: Bishop's attack trigger resolved its pump"
    );

    assert_eq!(
        power_toughness(&runner, vampire),
        (Some(2), Some(2)),
        "ruling + CR 607.2a + CR 400.7: the 4/4 Swords exiled is not the card \
         Bishop exiled, so there is no exiled card, X is 0 and the Vampire stays 2/2"
    );
}

/// The Bishop attack line shared by R-B2 and R-B0: Bishop (haste) exiles the
/// 4/4 and attacks. With `re_exile`, the 4/4 then leaves exile for the
/// graveyard; either way Relic of Progenitus's second ability ("Exile all
/// graveyards") resolves above the attack trigger, then the trigger resolves.
/// Returns the runner and the Vampire.
fn bishop_relic_line(re_exile: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    // `bishop_board` gives Bishop haste (a test setting) so it attacks this turn.
    let (bishop, vampire, brute) = bishop_board(&mut scenario, true);
    let relic = scenario
        .add_artifact_from_oracle(P0, "Relic of Progenitus", RELIC_OF_PROGENITUS)
        .id();
    // A library card for Relic's "Draw a card."
    scenario.add_card_to_library_top(P0, "Library A");
    let mut runner = scenario.build();
    runner.cast(bishop).target_object(brute).resolve();
    assert_eq!(runner.state().objects[&brute].zone, Zone::Exile);
    let exiled_by_bishop = runner.state().objects[&brute].incarnation;

    declare_attack(&mut runner, bishop);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "setup: Bishop's attack trigger is on the stack"
    );

    if re_exile {
        // Stand-in for Pull from Eternity's resolution (P1): the parser does not
        // yet produce Pull's exiled-card target, so the production zone mover
        // moves the card. This step is not Oracle text.
        let mut events = Vec::new();
        move_to_zone(runner.state_mut(), brute, Zone::Graveyard, &mut events);
        assert_eq!(runner.state().objects[&brute].zone, Zone::Graveyard);
    }

    add_one_colorless(&mut runner);
    let outcome = runner.activate(relic, 1).resolve();
    assert!(
        settled(runner.state()),
        "Relic's ability and the trigger resolved"
    );
    assert_eq!(
        runner.state().objects[&relic].zone,
        Zone::Exile,
        "setup: Relic was exiled to pay its cost"
    );
    let brute_now = &runner.state().objects[&brute];
    assert_eq!(brute_now.zone, Zone::Exile);
    if re_exile {
        assert_ne!(
            brute_now.incarnation, exiled_by_bishop,
            "setup: CR 400.7 — the 4/4 Relic exiled is a new object"
        );
    } else {
        assert_eq!(
            brute_now.incarnation, exiled_by_bishop,
            "setup: the 4/4 never left exile"
        );
    }
    assert!(
        bishop_pump_resolved(outcome.events(), bishop),
        "positive: Bishop's attack trigger resolved its pump"
    );
    (runner, vampire)
}

/// R-B2, CR 607.2a + CR 400.7: Bishop stays on the battlefield, but the 4/4
/// leaves exile and Relic of Progenitus exiles it again. The card in exile is a
/// new object that Bishop's first ability did not exile, so X = 0.
#[test]
fn card_exiled_again_by_another_effect_is_not_the_exiled_card() {
    let (runner, vampire) = bishop_relic_line(true);
    assert_eq!(
        power_toughness(&runner, vampire),
        (Some(2), Some(2)),
        "CR 607.2a + CR 400.7: the 4/4 Relic exiled is not the card Bishop \
         exiled, so X is 0 and the Vampire stays 2/2"
    );
}

/// R-B0, guard for R-B2: on the same line with the 4/4 never leaving exile, the
/// 4/4 is still the card Bishop exiled, so X = 4.
#[test]
fn card_that_never_left_exile_is_still_the_exiled_card() {
    let (runner, vampire) = bishop_relic_line(false);
    assert_eq!(
        power_toughness(&runner, vampire),
        (Some(6), Some(6)),
        "CR 607.2a: X is the power of the card exiled with Bishop (4), so the \
         Vampire is 2/2 + 4/4"
    );
}

/// The Admonition Angel line shared by R-F and R-F0: a land enters, the
/// landfall ability exiles the opponent's 4/4, and Unsummon returns the Angel
/// to hand, so its leaves-the-battlefield trigger waits on the stack. With
/// `re_exile`, the 4/4 then leaves exile for the graveyard; either way Relic of
/// Progenitus's second ability ("Exile all graveyards") resolves above the
/// trigger, then the trigger resolves. Returns the runner and the 4/4.
fn admonition_line(re_exile: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let angel = scenario
        .add_creature(P0, "Admonition Angel", 6, 6)
        .from_oracle_text_with_keywords(&["Flying", "Landfall"], ADMONITION_ANGEL)
        .id();
    let brute = scenario.add_creature(P1, "Opp Brute", 4, 4).id();
    let land = scenario.add_land_to_hand(P0, "Plains").id();
    // Unsummon is cost-free here (a test setting; printed {U}).
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let relic = scenario
        .add_artifact_from_oracle(P0, "Relic of Progenitus", RELIC_OF_PROGENITUS)
        .id();
    // A library card for Relic's "Draw a card."
    scenario.add_card_to_library_top(P0, "Library A");
    let mut runner = scenario.build();

    // Landfall: exile the 4/4 (the "you may" is accepted).
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing the land is accepted");
    drive_until(&mut runner, &[brute], |state| {
        settled(state) && state.objects[&brute].zone == Zone::Exile
    });
    assert_eq!(
        linked_pile(runner.state(), angel),
        vec![brute],
        "reach guard: the landfall exile is linked to the Angel"
    );
    let exiled_by_angel = runner.state().objects[&brute].incarnation;

    // The Angel leaves; its leaves-the-battlefield trigger waits on the stack.
    runner.cast(unsummon).target_object(angel).commit();
    drive_until(&mut runner, &[], |state| {
        state.objects[&angel].zone == Zone::Hand
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(
        runner.state().stack.len(),
        1,
        "setup: one entry on the stack"
    );
    assert!(
        matches!(
            runner.state().stack[0].kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == angel
        ),
        "setup: the Angel's leaves-the-battlefield trigger is waiting"
    );
    assert_eq!(runner.state().objects[&brute].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&brute].incarnation, exiled_by_angel);

    if re_exile {
        // Stand-in for Pull from Eternity's resolution (P1): the parser does not
        // yet produce Pull's exiled-card target, so the production zone mover
        // moves the card. This step is not Oracle text.
        let mut events = Vec::new();
        move_to_zone(runner.state_mut(), brute, Zone::Graveyard, &mut events);
        assert_eq!(runner.state().objects[&brute].zone, Zone::Graveyard);
    }

    add_one_colorless(&mut runner);
    let outcome = runner.activate(relic, 1).resolve();
    assert!(
        settled(runner.state()),
        "Relic's ability and the trigger resolved"
    );
    assert_eq!(
        runner.state().objects[&relic].zone,
        Zone::Exile,
        "setup: Relic was exiled to pay its cost"
    );
    if re_exile {
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Graveyard), to: Zone::Exile, .. }
                    if *object_id == brute
            )),
            "setup: Relic exiled the 4/4 from the graveyard"
        );
        assert_ne!(
            runner.state().objects[&brute].incarnation,
            exiled_by_angel,
            "setup: CR 400.7 — the 4/4 Relic exiled is a new object"
        );
    }
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::ChangeZoneAll, source_id, .. }
                if *source_id == angel
        )),
        "reach: the Angel's leaves-the-battlefield return resolved"
    );
    (runner, brute)
}

/// R-F, ruling + CR 607.2a + CR 400.7: with the Angel's leaves-the-battlefield
/// trigger on the stack, the 4/4 leaves exile and Relic of Progenitus exiles it
/// again. That card is a new object the landfall ability did not exile, so
/// "all cards exiled with it" is empty and the 4/4 stays in exile.
#[test]
fn leaves_the_battlefield_return_skips_a_card_exiled_again() {
    let (runner, brute) = admonition_line(true);
    assert_eq!(
        runner.state().objects[&brute].zone,
        Zone::Exile,
        "CR 607.2a + CR 400.7: the 4/4 Relic exiled is not a card exiled with \
         the Angel, so nothing returns"
    );
}

/// R-F0, guard for R-F: on the same line with the 4/4 never leaving exile, it
/// is the card the landfall ability exiled, so it returns under its owner's
/// control.
#[test]
fn leaves_the_battlefield_return_brings_back_the_linked_card() {
    let (runner, brute) = admonition_line(false);
    let brute_now = &runner.state().objects[&brute];
    assert_eq!(
        brute_now.zone,
        Zone::Battlefield,
        "CR 607.2a: the card exiled with the Angel returns"
    );
    assert_eq!(brute_now.controller, P1, "under its owner's control");
}
