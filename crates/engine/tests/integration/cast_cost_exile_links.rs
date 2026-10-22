//! CR 607.2q: a card exiled to pay a cost of casting a permanent spell is linked
//! to that spell and to the permanent it becomes, and to no other object
//! (CR 400.7). Each test drives the real cast, combat, activation and cancel
//! pipelines. Printed cards use their verbatim Oracle text; the Keeper tests use
//! the synthetic `KEEPER` building block, and the kept-pile test seeds one link
//! directly (named in its doc). Builder settings that are not Oracle text are
//! named in a comment where a test uses them.

use engine::game::combat::AttackTarget;
use engine::game::players::linked_exile_cards_for_source;
use engine::game::scenario::{CastCommit, GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, ExileLink, ExileLinkKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FEAR_OF_ABDUCTION: &str =
    "As an additional cost to cast this spell, exile a creature you control.\n\
Flying\n\
When this creature enters, exile target creature an opponent controls.\n\
When this creature leaves the battlefield, put each card exiled with it into its owner's hand.";

const GOREX: &str = "As an additional cost to cast this spell, you may exile any number of creature cards from your graveyard. This spell costs {2} less to cast for each card exiled this way.\n\
Deathtouch\n\
Whenever Gorex attacks or dies, choose a card at random exiled with Gorex and put that card into its owner's hand.";

const ETHEREAL_FORAGER: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
Flying\n\
Whenever this creature attacks, you may return an instant or sorcery card exiled with this creature to its owner's hand.";

const MURKTIDE_REGENT: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
Flying\n\
This creature enters with a +1/+1 counter on it for each instant and sorcery card exiled with it.\n\
Whenever an instant or sorcery card leaves your graveyard, put a +1/+1 counter on this creature.";

const UNSUMMON: &str = "Return target creature to its owner's hand.";
const UNMAKE: &str = "Exile target creature.";
const CANCEL: &str = "Counter target spell.";
const REMAND: &str = "Counter target spell. If that spell is countered this way, put it into its owner's hand instead of into that player's graveyard.\n\
Draw a card.";
const ZOMBIFY: &str = "Return target creature card from your graveyard to the battlefield.";
const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.\n\
Rebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
const RELEASE_TO_THE_WIND: &str = "Exile target nonland permanent. For as long as that card remains exiled, its owner may cast it without paying its mana cost.";
const MURDER: &str = "Destroy target creature.";
const COFFIN_PURGE: &str = "Exile target card from a graveyard.\n\
Flashback {B} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

/// Synthetic building block: a permanent spell with an additional-cost exile and
/// an activated "exiled with this creature" reader (CR 607.2q's class shape).
const KEEPER: &str =
    "As an additional cost to cast this spell, exile a creature card from your graveyard.\n\
{T}: Put a card exiled with this creature into its owner's hand.";

/// `KEEPER` plus a targeted activated ability that never reads the pile.
const KEEPER_WITH_TAPPER: &str =
    "As an additional cost to cast this spell, exile a creature card from your graveyard.\n\
{T}: Put a card exiled with this creature into its owner's hand.\n\
{1}: Tap target creature.";

fn add_mana(runner: &mut GameRunner, mana_type: ManaType, amount: usize) {
    for _ in 0..amount {
        runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }
}

fn linked_pile(runner: &GameRunner, source: ObjectId) -> Vec<ObjectId> {
    linked_exile_cards_for_source(runner.state(), source)
        .iter()
        .map(|snapshot| snapshot.exiled_id)
        .collect()
}

fn zone(runner: &GameRunner, object: ObjectId) -> Zone {
    runner.state().objects[&object].zone
}

fn plus_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Pass priority until the stack is empty, accepting every "you may" effect.
fn resolve_stack_accepting(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional effect");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the top of the stack");
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("resolve_stack_accepting does not handle {other:?}"),
        }
    }
    panic!("the stack did not empty");
}

/// Advance to declare attackers and attack the opponent with `attacker`.
fn attack_with(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the attacker");
}

/// Gorex in hand. Builder settings that are not Oracle text: mana cost {0}, and
/// `Keyword::Haste` so it can attack the turn it enters.
fn add_gorex(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Gorex, the Tombshell", 4, 4, GOREX)
        .with_mana_cost(ManaCost::zero())
        .with_keyword(Keyword::Haste)
        .id()
}

/// Keeper in hand. Builder settings that are not Oracle text: mana cost {0}, and
/// `Keyword::Haste` so its {T} ability can be activated the turn it enters.
fn add_keeper(scenario: &mut GameScenario, oracle: &str) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Synthetic Keeper", 2, 2, oracle)
        .with_mana_cost(ManaCost::zero())
        .with_keyword(Keyword::Haste)
        .id()
}

/// A {0}-cost (builder setting) instant or sorcery in P0's hand.
fn add_free_spell(
    scenario: &mut GameScenario,
    name: &str,
    is_instant: bool,
    oracle: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, is_instant, oracle)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Murktide Regent in hand at its printed cost, plus three instants in the
/// graveyard to delve.
struct MurktideBoard {
    runner: GameRunner,
    murktide: ObjectId,
    instants: [ObjectId; 3],
    extra: Vec<ObjectId>,
}

fn murktide_board(extra: &[(&str, bool, &str)]) -> MurktideBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let murktide = scenario
        .add_creature_to_hand_from_oracle(P0, "Murktide Regent", 3, 3, MURKTIDE_REGENT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 5,
        })
        .id();
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C"]);
    let instants = [
        scenario
            .add_spell_to_graveyard(P0, "Lightning Bolt", true)
            .id(),
        scenario.add_spell_to_graveyard(P0, "Shock", true).id(),
        scenario.add_spell_to_graveyard(P0, "Opt", true).id(),
    ];
    let extra = extra
        .iter()
        .map(|(name, is_instant, oracle)| add_free_spell(&mut scenario, name, *is_instant, oracle))
        .collect();
    MurktideBoard {
        runner: scenario.build(),
        murktide,
        instants,
        extra,
    }
}

/// Add the blue mana a Murktide cast that delves `delved` cards pays from the
/// pool: its {5}{U}{U} less one generic per delved card.
fn fund_murktide(runner: &mut GameRunner, delved: usize) {
    add_mana(runner, ManaType::Blue, 7 - delved);
}

// ---------------------------------------------------------------------------
// Red at base: the cast-cost pile reaches the permanent the spell becomes.
// ---------------------------------------------------------------------------

/// R1. CR 607.2q + CR 603.10a: Fear of Abduction's leaves-the-battlefield
/// trigger returns the creature exiled to pay its additional cost.
#[test]
fn fear_of_abduction_returns_the_creature_exiled_for_its_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Builder setting that is not Oracle text: mana cost {0}.
    let fear = scenario
        .add_creature_to_hand_from_oracle(P0, "Fear of Abduction", 5, 5, FEAR_OF_ABDUCTION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let fodder = scenario.add_creature(P0, "My Fodder", 2, 2).id();
    let brute = scenario.add_creature(P1, "Opp Brute", 4, 4).id();
    let unsummon = add_free_spell(&mut scenario, "Unsummon", true, UNSUMMON);
    let mut runner = scenario.build();

    runner
        .cast(fear)
        .pay_cost_with(&[fodder])
        .target_objects(&[brute])
        .resolve();
    assert_eq!(zone(&runner, fodder), Zone::Exile);
    assert_eq!(zone(&runner, brute), Zone::Exile);

    runner.cast(unsummon).target_objects(&[fear]).resolve();

    // Reach guard: the enters-trigger pile returns at base too.
    assert_eq!(zone(&runner, brute), Zone::Hand);
    assert_eq!(
        zone(&runner, fodder),
        Zone::Hand,
        "CR 607.2q: the cost-exiled creature is exiled with Fear of Abduction"
    );
}

/// R2. CR 607.2q: Gorex's attack trigger chooses among the cards exiled to pay
/// its cost.
#[test]
fn gorex_attack_trigger_returns_a_card_exiled_for_its_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gorex = add_gorex(&mut scenario);
    let giant = scenario
        .add_creature_to_graveyard(P0, "Graveyard Giant", 5, 5)
        .id();
    let mut runner = scenario.build();

    runner
        .cast(gorex)
        .accept_optional()
        .pay_cost_with(&[giant])
        .resolve();
    assert_eq!(zone(&runner, gorex), Zone::Battlefield);
    assert_eq!(
        linked_pile(&runner, gorex),
        vec![giant],
        "CR 607.2q: the permanent Gorex became reads its cast-cost pile"
    );

    attack_with(&mut runner, gorex);
    resolve_stack_accepting(&mut runner);

    assert_eq!(zone(&runner, giant), Zone::Hand);
}

/// R3. CR 607.2q + CR 702.66a: Ethereal Forager's attack trigger can return a
/// card it delved.
#[test]
fn ethereal_forager_returns_a_delved_sorcery() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Builder setting that is not Oracle text: `Keyword::Haste` so it can attack
    // the turn it enters.
    let forager = scenario
        .add_creature_to_hand_from_oracle(P0, "Ethereal Forager", 4, 4, ETHEREAL_FORAGER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 4,
        })
        .with_keyword(Keyword::Haste)
        .id();
    let divination = scenario
        .add_spell_to_graveyard(P0, "Divination", false)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Blue, 5);

    let committed = runner.cast(forager).delve_with(&[divination]).commit();
    // Reach guard: the delve exile is linked while Forager is on the stack.
    assert!(committed
        .state()
        .exile_links
        .iter()
        .any(|link| link.exiled_id == divination && link.source_id == forager));
    committed.resolve();
    assert_eq!(zone(&runner, forager), Zone::Battlefield);

    attack_with(&mut runner, forager);
    resolve_stack_accepting(&mut runner);

    assert_eq!(
        zone(&runner, divination),
        Zone::Hand,
        "CR 607.2q: the delved sorcery is exiled with Ethereal Forager"
    );
}

/// R4. CR 701.6a + CR 400.7: a countered Murktide that is reanimated is a new
/// object; its countered spell's delve pile is not its own.
#[test]
fn countered_then_reanimated_murktide_enters_without_counters() {
    let mut board = murktide_board(&[("Cancel", true, CANCEL), ("Zombify", false, ZOMBIFY)]);
    let (murktide, [bolt, shock, _]) = (board.murktide, board.instants);
    let (cancel, zombify) = (board.extra[0], board.extra[1]);
    let runner = &mut board.runner;
    fund_murktide(runner, 2);

    let mut committed = runner.cast(murktide).delve_with(&[bolt, shock]).commit();
    committed.cast(cancel).target_objects(&[murktide]).resolve();
    assert_eq!(zone(runner, murktide), Zone::Graveyard);

    runner.cast(zombify).target_objects(&[murktide]).resolve();

    assert_eq!(zone(runner, murktide), Zone::Battlefield);
    assert_eq!(
        plus_counters(runner, murktide),
        0,
        "CR 400.7 + CR 607.2q: the reanimated Murktide is not the permanent the spell became"
    );
}

/// R5. CR 400.7 + CR 607.2q: a remanded Murktide recast with one delved card
/// counts only the new spell's delve.
#[test]
fn remanded_murktide_recast_counts_only_its_new_delve() {
    let mut board = murktide_board(&[("Remand", true, REMAND)]);
    let (murktide, [bolt, shock, opt]) = (board.murktide, board.instants);
    let remand = board.extra[0];
    let runner = &mut board.runner;
    fund_murktide(runner, 2);

    let mut committed = runner.cast(murktide).delve_with(&[bolt, shock]).commit();
    committed.cast(remand).target_objects(&[murktide]).resolve();
    assert_eq!(zone(runner, murktide), Zone::Hand);

    fund_murktide(runner, 1);
    runner.cast(murktide).delve_with(&[opt]).resolve();

    // Reach guard: the recast delved the third instant.
    assert_eq!(zone(runner, opt), Zone::Exile);
    assert_eq!(zone(runner, murktide), Zone::Battlefield);
    assert_eq!(
        plus_counters(runner, murktide),
        1,
        "CR 607.2q: only the recast spell's delve is exiled with this Murktide"
    );
}

/// R6. CR 113.7a + CR 607.2q: an activated reader whose source is exiled in
/// response still reads the departed permanent's cast-cost pile.
#[test]
fn activated_reader_reads_its_cast_cost_pile_after_its_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let keeper = add_keeper(&mut scenario, KEEPER);
    let giant = scenario
        .add_creature_to_graveyard(P0, "Graveyard Giant", 5, 5)
        .id();
    let unmake = add_free_spell(&mut scenario, "Unmake", true, UNMAKE);
    let mut runner = scenario.build();

    runner.cast(keeper).pay_cost_with(&[giant]).resolve();
    runner
        .act(GameAction::ActivateAbility {
            source_id: keeper,
            ability_index: 0,
        })
        .expect("activate the reader");
    assert_eq!(runner.state().stack.len(), 1);

    runner.cast(unmake).target_objects(&[keeper]).resolve();

    // Reach guard: the activation resolved after its source was exiled.
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(&runner, keeper), Zone::Exile);
    assert_eq!(
        zone(&runner, giant),
        Zone::Hand,
        "CR 113.7a + CR 607.2q: the departed permanent's ability reads its pile"
    );
}

/// R7. CR 113.7a + CR 607.2q: Gorex's attack trigger resolves after Gorex is
/// exiled and still returns its cast-cost card.
#[test]
fn gorex_attack_trigger_reads_its_cast_cost_pile_after_gorex_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gorex = add_gorex(&mut scenario);
    let giant = scenario
        .add_creature_to_graveyard(P0, "Graveyard Giant", 5, 5)
        .id();
    let unmake = add_free_spell(&mut scenario, "Unmake", true, UNMAKE);
    let mut runner = scenario.build();

    runner
        .cast(gorex)
        .accept_optional()
        .pay_cost_with(&[giant])
        .resolve();
    attack_with(&mut runner, gorex);
    // Reach guard: the attack trigger is on the stack before the response.
    assert_eq!(runner.state().stack.len(), 1);

    runner.cast(unmake).target_objects(&[gorex]).resolve();

    assert_eq!(zone(&runner, gorex), Zone::Exile);
    assert_eq!(
        zone(&runner, giant),
        Zone::Hand,
        "CR 113.7a + CR 607.2q: the trigger reads the departed Gorex's pile"
    );
}

/// Gorex(A) is exiled by Release to the Wind and recast from exile, either
/// exiling B for its cost or declining the cost. A {0} Murder waits in P0's
/// hand. Returns `(runner, gorex, a, b, murder)` after the recast resolves.
fn gorex_recast_from_exile(exile_b: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gorex = add_gorex(&mut scenario);
    let a = scenario
        .add_creature_to_graveyard(P0, "Old Giant A", 5, 5)
        .id();
    let b = scenario
        .add_creature_to_graveyard(P0, "New Giant B", 3, 3)
        .id();
    let release = add_free_spell(
        &mut scenario,
        "Release to the Wind",
        true,
        RELEASE_TO_THE_WIND,
    );
    let murder = add_free_spell(&mut scenario, "Murder", true, MURDER);
    let mut runner = scenario.build();

    runner
        .cast(gorex)
        .accept_optional()
        .pay_cost_with(&[a])
        .resolve();
    runner.cast(release).target_objects(&[gorex]).resolve();
    assert_eq!(zone(&runner, gorex), Zone::Exile);

    if exile_b {
        runner
            .cast(gorex)
            .accept_optional()
            .pay_cost_with(&[b])
            .resolve();
    } else {
        runner.cast(gorex).decline_optional().resolve();
    }
    // Reach guard: the recast resolved into a permanent.
    assert_eq!(zone(&runner, gorex), Zone::Battlefield);
    (runner, gorex, a, b, murder)
}

/// R8. CR 400.7 + CR 607.2q: Gorex recast from exile reads only the new
/// spell's cost pile, never the departed Gorex's.
#[test]
fn gorex_recast_from_exile_reads_only_its_new_cost_pile() {
    let (mut runner, gorex, a, b, _) = gorex_recast_from_exile(true);
    // Gorex chooses at random, so the pile itself is the discriminator.
    assert_eq!(
        linked_pile(&runner, gorex),
        vec![b],
        "CR 607.2q: only cards exiled to pay the recast spell's cost"
    );

    attack_with(&mut runner, gorex);
    resolve_stack_accepting(&mut runner);

    assert_eq!(zone(&runner, b), Zone::Hand);
    assert_eq!(zone(&runner, a), Zone::Exile);
}

/// R9. CR 607.2a + CR 607.2q + CR 400.7: a kept `TrackedBySource` pile on a
/// source that re-enters through a new cast is dropped at entry, while the new
/// spell's cost pile survives. The kept pile X is seeded directly (synthetic;
/// a real kept pile comes from Mechtitan Core's self-exile).
#[test]
fn kept_tracked_pile_does_not_join_the_new_cast_cost_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let keeper = add_keeper(&mut scenario, KEEPER);
    let a = scenario
        .add_creature_to_graveyard(P0, "First Giant A", 5, 5)
        .id();
    let b = scenario
        .add_creature_to_graveyard(P0, "Second Giant B", 3, 3)
        .id();
    let x = scenario.add_creature_to_exile(P0, "Kept Pile X", 1, 1).id();
    let release = add_free_spell(
        &mut scenario,
        "Release to the Wind",
        true,
        RELEASE_TO_THE_WIND,
    );
    let mut runner = scenario.build();

    runner.cast(keeper).pay_cost_with(&[a]).resolve();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: x,
        source_id: keeper,
        kind: ExileLinkKind::TrackedBySource,
    });
    runner.cast(release).target_objects(&[keeper]).resolve();
    // Reach guard: the seeded pile is kept across the exit to exile (Mechtitan
    // Core's keep), so it is live when the recast exiles B.
    let keeper_links: Vec<(ObjectId, ExileLinkKind)> = runner
        .state()
        .exile_links
        .iter()
        .filter(|link| link.source_id == keeper)
        .map(|link| (link.exiled_id, link.kind.clone()))
        .collect();
    assert_eq!(keeper_links, vec![(x, ExileLinkKind::TrackedBySource)]);

    runner.cast(keeper).pay_cost_with(&[b]).resolve();
    assert_eq!(
        linked_pile(&runner, keeper),
        vec![b],
        "CR 400.7 + CR 607.2q: the re-entered Keeper's pile is its new cost pile only"
    );

    runner.activate(keeper, 0).resolve();

    assert_eq!(zone(&runner, b), Zone::Hand);
    assert_eq!(zone(&runner, x), Zone::Exile);
    assert_eq!(zone(&runner, a), Zone::Exile);
}

/// R10. CR 733.1 + CR 607.2q: cancelling an activation of the permanent leaves
/// the permanent's cast-cost pile in place; only an undone spell cast ends one.
#[test]
fn cancelled_activation_keeps_the_permanents_cast_cost_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let keeper = add_keeper(&mut scenario, KEEPER_WITH_TAPPER);
    let a = scenario
        .add_creature_to_graveyard(P0, "Graveyard Giant", 5, 5)
        .id();
    scenario.add_creature(P1, "Opp Brute", 4, 4);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();

    runner.cast(keeper).pay_cost_with(&[a]).resolve();
    // Reach guard: the cost link was born and reached the permanent.
    assert_eq!(linked_pile(&runner, keeper), vec![a]);

    runner
        .act(GameAction::ActivateAbility {
            source_id: keeper,
            ability_index: 1,
        })
        .expect("activate {1}: Tap target creature.");
    // Reach guard: CR 601.2c + CR 602.2b — targets precede costs, so the
    // activation is still uncommitted at its target prompt.
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => {
            assert!(pending_cast.activation_ability_index.is_some());
            assert!(!pending_cast.activation_cost_committed);
        }
        other => panic!("expected the activation's TargetSelection, got {other:?}"),
    }
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the activation");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    assert_eq!(
        linked_pile(&runner, keeper),
        vec![a],
        "CR 733.1: an activation cancel does not undo the permanent's cast"
    );
}

/// Gorex on the battlefield with a {0} Murder and a {0} Coffin Purge in P0's
/// hand.
struct GorexBoard {
    runner: GameRunner,
    gorex: ObjectId,
    pile: Vec<ObjectId>,
    murder: ObjectId,
    coffin_purge: ObjectId,
}

/// Cast Gorex exiling one graveyard creature card per name in `pile` for its
/// cost, or declining the cost when `pile` is empty.
fn gorex_board(pile: &[&str]) -> GorexBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gorex = add_gorex(&mut scenario);
    let pile: Vec<ObjectId> = pile
        .iter()
        .map(|name| scenario.add_creature_to_graveyard(P0, name, 5, 5).id())
        .collect();
    let murder = add_free_spell(&mut scenario, "Murder", true, MURDER);
    let coffin_purge = add_free_spell(&mut scenario, "Coffin Purge", true, COFFIN_PURGE);
    let mut runner = scenario.build();

    if pile.is_empty() {
        runner.cast(gorex).decline_optional().resolve();
    } else {
        runner
            .cast(gorex)
            .accept_optional()
            .pay_cost_with(&pile)
            .resolve();
    }
    // Reach guard: the cast resolved into a permanent whose pile is its cost.
    assert_eq!(zone(&runner, gorex), Zone::Battlefield);
    assert_eq!(linked_pile(&runner, gorex), pile);
    GorexBoard {
        runner,
        gorex,
        pile,
        murder,
        coffin_purge,
    }
}

/// Cast Murder on Gorex and pass priority until Murder has resolved, leaving
/// Gorex's dies trigger as the only stack item.
fn murder_gorex_until_its_dies_trigger(
    runner: &mut GameRunner,
    murder: ObjectId,
    gorex: ObjectId,
) -> CastCommit<'_> {
    let mut committed = runner.cast(murder).target_objects(&[gorex]).commit();
    for _ in 0..8 {
        let stack = &committed.state().stack;
        if stack.len() == 1 && matches!(stack[0].kind, StackEntryKind::TriggeredAbility { .. }) {
            break;
        }
        committed
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Murder");
    }
    // Reach guard: Gorex died and its dies trigger is the only stack item.
    let stack = &committed.state().stack;
    assert_eq!(stack.len(), 1);
    assert!(matches!(
        stack[0].kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert_eq!(stack[0].source_id, gorex);
    assert_eq!(committed.state().objects[&gorex].zone, Zone::Graveyard);
    committed
}

/// R11. CR 603.10a + CR 607.2q + CR 608.2c: Gorex's dies trigger returns the
/// card exiled to pay its cost, and Gorex stays in the graveyard.
#[test]
fn gorex_dies_trigger_returns_a_card_exiled_for_its_cost() {
    let mut board = gorex_board(&["Graveyard Giant A"]);
    let (gorex, a, murder) = (board.gorex, board.pile[0], board.murder);
    let runner = &mut board.runner;

    runner.cast(murder).target_objects(&[gorex]).resolve();

    assert_eq!(zone(runner, gorex), Zone::Graveyard);
    assert_eq!(
        zone(runner, a),
        Zone::Hand,
        "CR 608.2c: \"that card\" is the card chosen from among those exiled with Gorex"
    );
}

/// R12. CR 608.2c: "choose a card at random" returns exactly one card of a
/// two-card pile.
#[test]
fn gorex_dies_trigger_returns_exactly_one_card_of_its_pile() {
    let mut board = gorex_board(&["Graveyard Giant A", "Graveyard Giant B"]);
    let (gorex, murder) = (board.gorex, board.murder);
    let pile = board.pile.clone();
    let runner = &mut board.runner;
    // Reach guard: both cards are exiled with Gorex.
    assert_eq!(pile.len(), 2);

    runner.cast(murder).target_objects(&[gorex]).resolve();

    assert_eq!(zone(runner, gorex), Zone::Graveyard);
    let zones: Vec<Zone> = pile.iter().map(|&card| zone(runner, card)).collect();
    assert_eq!(
        zones.iter().filter(|&&z| z == Zone::Hand).count(),
        1,
        "one card is chosen, got {zones:?}"
    );
    assert_eq!(
        zones.iter().filter(|&&z| z == Zone::Exile).count(),
        1,
        "the other card stays exiled, got {zones:?}"
    );
}

/// R13. CR 607.2a + CR 607.2q: a pile card the attack trigger already returned
/// is no longer exiled with Gorex, so the dies trigger in the same combat
/// returns the other one.
#[test]
fn gorex_dies_trigger_after_its_attack_trigger_returns_the_rest_of_the_pile() {
    let mut board = gorex_board(&["Graveyard Giant A", "Graveyard Giant B"]);
    let (gorex, murder) = (board.gorex, board.murder);
    let pile = board.pile.clone();
    let runner = &mut board.runner;

    attack_with(runner, gorex);
    resolve_stack_accepting(runner);
    // Reach guard: the attack trigger returned exactly one card.
    assert_eq!(
        pile.iter()
            .filter(|&&card| zone(runner, card) == Zone::Hand)
            .count(),
        1
    );

    runner.cast(murder).target_objects(&[gorex]).resolve();

    assert_eq!(zone(runner, gorex), Zone::Graveyard);
    for &card in &pile {
        assert_eq!(zone(runner, card), Zone::Hand);
    }
}

/// R14. CR 113.7a: Gorex's dies trigger resolves after Gorex leaves the
/// graveyard and still returns the card exiled with it.
#[test]
fn gorex_dies_trigger_returns_its_card_after_gorex_leaves_the_graveyard() {
    let mut board = gorex_board(&["Graveyard Giant A"]);
    let (gorex, a, murder, coffin_purge) =
        (board.gorex, board.pile[0], board.murder, board.coffin_purge);
    let runner = &mut board.runner;

    let mut committed = murder_gorex_until_its_dies_trigger(runner, murder, gorex);
    committed
        .cast(coffin_purge)
        .target_objects(&[gorex])
        .resolve();

    assert_eq!(zone(runner, gorex), Zone::Exile);
    assert_eq!(
        zone(runner, a),
        Zone::Hand,
        "CR 113.7a: the trigger acts after its source left the graveyard"
    );
}

// ---------------------------------------------------------------------------
// Guards: readings that hold at base and with the design.
// ---------------------------------------------------------------------------

/// G1. CR 607.2q: Murktide enters with a counter for each instant it delved.
#[test]
fn murktide_enters_with_a_counter_for_each_delved_instant() {
    let mut board = murktide_board(&[]);
    let (murktide, [bolt, shock, _]) = (board.murktide, board.instants);
    let runner = &mut board.runner;
    fund_murktide(runner, 2);

    runner.cast(murktide).delve_with(&[bolt, shock]).resolve();

    assert_eq!(zone(runner, murktide), Zone::Battlefield);
    assert_eq!(plus_counters(runner, murktide), 2);
}

/// G2. CR 400.7 + CR 607.2q: a flickered Murktide returns as a new object that
/// no spell became, so it enters without counters.
#[test]
fn flickered_murktide_returns_without_counters() {
    let mut board = murktide_board(&[("Ephemerate", true, EPHEMERATE)]);
    let (murktide, [bolt, shock, _]) = (board.murktide, board.instants);
    let ephemerate = board.extra[0];
    let runner = &mut board.runner;
    fund_murktide(runner, 2);

    runner.cast(murktide).delve_with(&[bolt, shock]).resolve();
    // Reach guard: the permanent the spell became read its delve pile.
    assert_eq!(plus_counters(runner, murktide), 2);

    runner
        .cast(ephemerate)
        .target_objects(&[murktide])
        .resolve();

    assert_eq!(zone(runner, murktide), Zone::Battlefield);
    assert_eq!(plus_counters(runner, murktide), 0);
}

/// G3. CR 701.6a + CR 400.7: a countered Gorex that is reanimated has no pile.
#[test]
fn countered_then_reanimated_gorex_has_no_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gorex = add_gorex(&mut scenario);
    let a = scenario
        .add_creature_to_graveyard(P0, "Old Giant A", 5, 5)
        .id();
    let cancel = add_free_spell(&mut scenario, "Cancel", true, CANCEL);
    let zombify = add_free_spell(&mut scenario, "Zombify", false, ZOMBIFY);
    let mut runner = scenario.build();

    let mut committed = runner
        .cast(gorex)
        .accept_optional()
        .pay_cost_with(&[a])
        .commit();
    committed.cast(cancel).target_objects(&[gorex]).resolve();
    runner.cast(zombify).target_objects(&[gorex]).resolve();
    // Reach guard: the reanimated Gorex is on the battlefield and attacks.
    assert_eq!(zone(&runner, gorex), Zone::Battlefield);

    attack_with(&mut runner, gorex);
    assert_eq!(runner.state().stack.len(), 1);
    resolve_stack_accepting(&mut runner);

    assert_eq!(zone(&runner, a), Zone::Exile);
}

/// G4. CR 607.2q: Gorex recast from exile declining its cost paid no cost, so
/// the departed Gorex's pile is not its own.
#[test]
fn gorex_recast_from_exile_declining_its_cost_has_no_pile() {
    let (mut runner, gorex, a, b, _) = gorex_recast_from_exile(false);

    attack_with(&mut runner, gorex);
    // Reach guard: the attack trigger is on the stack.
    assert_eq!(runner.state().stack.len(), 1);
    resolve_stack_accepting(&mut runner);

    assert_eq!(zone(&runner, a), Zone::Exile);
    assert_eq!(zone(&runner, b), Zone::Graveyard);
}

/// G5. CR 607.2q: Murktide recast from exile without delving enters without
/// counters.
#[test]
fn murktide_recast_from_exile_without_delve_has_no_counters() {
    let mut board = murktide_board(&[("Release to the Wind", true, RELEASE_TO_THE_WIND)]);
    let (murktide, [bolt, shock, _]) = (board.murktide, board.instants);
    let release = board.extra[0];
    let runner = &mut board.runner;
    fund_murktide(runner, 2);

    runner.cast(murktide).delve_with(&[bolt, shock]).resolve();
    runner.cast(release).target_objects(&[murktide]).resolve();
    assert_eq!(zone(runner, murktide), Zone::Exile);

    fund_murktide(runner, 0);
    runner.cast(murktide).resolve();

    assert_eq!(zone(runner, murktide), Zone::Battlefield);
    assert_eq!(plus_counters(runner, murktide), 0);
}

/// G6. CR 733.1: a cancelled Gorex cast's cost exile is not a cost of the
/// spell cast after it.
#[test]
fn cancelled_gorex_cast_pile_does_not_reach_the_next_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Builder setting that is not Oracle text: `Keyword::Haste` so it can attack
    // the turn it enters. Gorex keeps its printed cost so the cast stops at mana
    // payment.
    let gorex = scenario
        .add_creature_to_hand_from_oracle(P0, "Gorex, the Tombshell", 4, 4, GOREX)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 6,
        })
        .with_keyword(Keyword::Haste)
        .id();
    let a = scenario
        .add_creature_to_graveyard(P0, "Old Giant A", 5, 5)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Black, 8);

    let card_id = runner.state().objects[&gorex].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: gorex,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting Gorex");
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("pay the optional exile cost");
    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("exile A for the cost");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner.act(GameAction::CancelCast).expect("cancel the cast");

    runner.cast(gorex).decline_optional().resolve();
    // Reach guard: the recast resolved into a permanent that attacks.
    assert_eq!(zone(&runner, gorex), Zone::Battlefield);
    // CR 733.1: the cancel reverses the cast and cancels its payments, so A is
    // back in the graveyard and no cost link survives for the recast.
    assert_eq!(zone(&runner, a), Zone::Graveyard);
    assert!(
        linked_pile(&runner, gorex).is_empty(),
        "CR 733.1: the cancelled cast's cost link does not reach the recast permanent"
    );

    attack_with(&mut runner, gorex);
    assert_eq!(runner.state().stack.len(), 1);
    resolve_stack_accepting(&mut runner);

    assert_ne!(zone(&runner, a), Zone::Hand);
}

// ---------------------------------------------------------------------------
// Dies-trigger hostile fixtures: no pile, or a prior incarnation's pile.
// ---------------------------------------------------------------------------

/// G7. CR 609.3: with nothing exiled with Gorex, its dies trigger chooses
/// nothing and Gorex stays in the graveyard.
#[test]
fn gorex_dies_trigger_with_an_empty_pile_moves_nothing() {
    let mut board = gorex_board(&[]);
    let (gorex, murder) = (board.gorex, board.murder);
    let runner = &mut board.runner;

    murder_gorex_until_its_dies_trigger(runner, murder, gorex).resolve();

    assert!(runner.state().stack.is_empty());
    assert_eq!(
        zone(runner, gorex),
        Zone::Graveyard,
        "CR 609.3: \"that card\" names no card, so nothing moves"
    );
}

/// G8. CR 400.7 + CR 607.2q: Gorex recast from exile declining its cost has no
/// pile when it dies; the departed Gorex's card stays exiled.
#[test]
fn recast_gorex_dies_trigger_does_not_read_the_departed_gorexs_pile() {
    let (mut runner, gorex, a, _, murder) = gorex_recast_from_exile(false);
    // Reach guard: A is still exiled when the recast Gorex dies.
    assert_eq!(zone(&runner, a), Zone::Exile);

    runner.cast(murder).target_objects(&[gorex]).resolve();

    assert_eq!(zone(&runner, a), Zone::Exile);
    assert_eq!(zone(&runner, gorex), Zone::Graveyard);
}

/// G9. CR 400.7 + CR 607.2q: Gorex recast from exile paying with B returns B
/// when it dies, never the departed Gorex's A.
#[test]
fn recast_gorex_dies_trigger_reads_only_its_new_cost_pile() {
    let (mut runner, gorex, a, b, murder) = gorex_recast_from_exile(true);
    // Reach guard: only B is exiled with the recast Gorex.
    assert_eq!(linked_pile(&runner, gorex), vec![b]);

    runner.cast(murder).target_objects(&[gorex]).resolve();

    assert_eq!(zone(&runner, b), Zone::Hand);
    assert_eq!(zone(&runner, a), Zone::Exile);
    assert_eq!(zone(&runner, gorex), Zone::Graveyard);
}
