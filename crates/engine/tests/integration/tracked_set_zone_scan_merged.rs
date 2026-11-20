//! CR 730.3c: "If an effect can find the new object that a merged permanent
//! becomes as it leaves the battlefield, it finds all of those objects." These
//! rows pin where that finding must NOT happen: readers whose tracked-set
//! reference is cause-bound to a different action (D4), reads through a carried
//! set rather than the resolution's own chain (the D7 gate), and a delayed
//! trigger's bound set. After the D8 revision (a return printed on the source binds the source's linked exile, CR 607.2a) Day's rows hold by
//! `ExiledBySource` scoping; the D4 guards are the Timothar row (one-card path) and the
//! Living Death row (zone scan).
//!
//! Every merge here is a SYNTHETIC STEP: `merge_object_onto` stages the merged
//! permanent instead of a real mutate cast (precedent:
//! `exile_attribution_10414.rs`). The cards carry their real Oracle text; the
//! rider is always Gemrazer.

use engine::game::combat::AttackTarget;
use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CastingPermission, Effect, TargetFilter, ThisWayCause};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GEMRAZER: &str = "Mutate {1}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nReach, trample\nWhenever this creature mutates, destroy target artifact or enchantment an opponent controls.";
const MURDER: &str = "Destroy target creature.";
const SYPHON_FLESH: &str = "Each other player sacrifices a creature of their choice. You create a 2/2 black Zombie creature token for each creature sacrificed this way.";
const TIMOTHAR: &str = "Ward\u{2014}Discard a card.\nWhenever another nontoken Vampire you control dies, you may pay {1} and exile it. If you do, create a 1/1 black Bat creature token with flying. It gains \"When this token deals combat damage to a player, sacrifice it and return the exiled card to the battlefield tapped.\"";
const CHILD_OF_NIGHT: &str = "Lifelink";
const LIVING_DEATH: &str = "Each player exiles all creature cards from their graveyard, then sacrifices all creatures they control, then puts all cards they exiled this way onto the battlefield.";
// Vanilla (textless) filler body: there is no Oracle text to quote.
const GRIZZLY_BEARS: &str = "";
const RENO_AND_RUDE: &str = "Menace\nWhenever Reno and Rude deals combat damage to a player, exile the top card of that player's library. Then you may sacrifice another creature or artifact. If you do, you may play the exiled card this turn, and mana of any type can be spent to cast it.";
const DAY_OF_THE_DRAGONS: &str = "When this enchantment enters, exile all creatures you control. Then create that many 5/5 red Dragon creature tokens with flying.\nWhen this enchantment leaves the battlefield, sacrifice all Dragons you control. Then return the exiled cards to the battlefield under your control.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const BRISTLEBUD_FARMER: &str = "Trample\nWhen this creature enters, create two Food tokens. (They're artifacts with \"{2}, {T}, Sacrifice this token: You gain 3 life.\")\nWhenever this creature attacks, you may sacrifice a Food. If you do, mill three cards. You may put a permanent card from among them into your hand.";
const FORTUNE_LOYAL_STEED: &str = "When Fortune enters, scry 2.\nWhenever Fortune attacks while saddled, at end of combat, exile it and up to one creature that saddled it this turn, then return those cards to the battlefield under their owner's control.\nSaddle 1";
const VOYAGER_STAFF: &str = "{2}, Sacrifice this artifact: Exile target creature. Return the exiled card to the battlefield under its owner's control at the beginning of the next end step.";

fn mana(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { generic, shards }
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Option<Zone> {
    runner.state().objects.get(&id).map(|object| object.zone)
}

/// Drive priority and the optional prompts until the stack is empty. Any other prompt is
/// unexpected and panics, so a wrong offer can never end a row early.
fn drain(runner: &mut GameRunner) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { index: 0 })
                    .expect("pay");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => {
                let before = runner.state().stack.len();
                runner.advance_until_stack_empty();
                if runner.state().stack.len() == before
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
            }
            other => panic!("drain: unexpected prompt {other:?}"),
        }
    }
}

fn tokens(state: &GameState, name: &str) -> Vec<ObjectId> {
    let mut found: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|object| object.is_token && object.name == name)
        })
        .collect();
    found.sort_by_key(|id| id.0);
    found
}

/// SYNTHETIC STEP: merge `rider` onto `host` with `merge_object_onto`; the
/// rider leaves hand and battlefield first, as a resolving mutate spell would.
fn stage(runner: &mut GameRunner, rider: ObjectId, host: ObjectId, side: MergeSide) {
    let state = runner.state_mut();
    for player in state.players.iter_mut() {
        player.hand.retain(|&id| id != rider);
    }
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();
    merge_object_onto(state, rider, host, side, &mut events);
    state.battlefield.retain(|&id| id != rider);
    assert_eq!(
        state.objects[&host].merged_components.len(),
        2,
        "reach guard: the survivor is a merged permanent of two components"
    );
}

/// Clear summoning sickness for an object the scenario placed this turn.
fn ready(runner: &mut GameRunner, id: ObjectId) {
    let turn = runner.state().turn_number;
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.summoning_sick = false;
    object.entered_battlefield_turn = Some(turn.saturating_sub(1));
}

/// Timothar, Baron of Bats with the Bat token merged over Gemrazer. The Bat's
/// "sacrifice it and return the exiled card" reads `TrackedSetFiltered` bound
/// to `Exiled` (D8). The Bat is sacrificed (set member cause `Sacrificed`) and
/// its component Gemrazer lands in the graveyard; the reader's `caused_by`
/// differs from the member's stamp, so Gemrazer must not be found (D4; CR 608.2c
/// and CR 614.6: "the exiled card" is what the exile produced, not what the
/// sacrifice produced) and stays in the graveyard.
///
/// SYNTHETIC STEPS: the merge is staged with `merge_object_onto`, and the Bat's
/// summoning sickness is cleared directly.
///
/// Revert sensitivity: a component path that ignores `caused_by` finds Gemrazer
/// (a graveyard-zone object the pool scans) and returns it to the battlefield.
#[test]
fn timothar_bat_token_topped_does_not_return_the_merged_component() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Timothar, Baron of Bats", 4, 4, TIMOTHAR)
        .with_subtypes(vec!["Vampire", "Noble"]);
    let vampire = scenario
        .add_creature_from_oracle(P0, "Child of Night", 2, 1, CHILD_OF_NIGHT)
        .with_subtypes(vec!["Vampire"])
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(cost(1, vec![ManaCostShard::Black, ManaCostShard::Black]))
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Black, 6));
    let mut runner = scenario.build();
    runner
        .cast(murder)
        .target_object(vampire)
        .accept_optional()
        .resolve();
    drain(&mut runner);
    let bats = tokens(runner.state(), "Bat");
    assert_eq!(bats.len(), 1, "reach guard: Timothar's trigger made a Bat");
    let bat = bats[0];
    stage(&mut runner, gemrazer, bat, MergeSide::Bottom);
    ready(&mut runner, bat);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bat, AttackTarget::Player(P1))])
        .expect("declare");
    let _ = runner.combat_damage();
    drain(&mut runner);

    assert_eq!(
        zone_of(&runner, bat),
        None,
        "reach guard: the Bat dealt combat damage, was sacrificed and ceased"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "D4: Gemrazer was not exiled by the Bat's source, so it is not 'the exiled card'"
    );
}

/// Day of the Dragons under Rest in Peace, with Gemrazer merged under the
/// Dragon token. Disenchant destroys Day; its leaves-the-battlefield ability
/// sacrifices the Dragon, and Rest in Peace exiles the sacrificed survivor and
/// each component instead of putting them into a graveyard: CR 730.3 puts each
/// component into its zone, and Rest in Peace's replacement applies to each card
/// or token (CR 614.1a + CR 614.6). Day's "return the exiled cards" reads `ExiledBySource` with origin Exile (CR 607.2a), so the scan reaches Exile, where Gemrazer sits. Gemrazer was sacrificed with the Dragon and put into exile by Rest in Peace's replacement (CR 614.6), not by Day's linked ETB, so no exile link names Day as its source and it stays in exile. Child of Night, which the ETB exiled, returns.
///
/// SYNTHETIC STEP: the merge is staged with `merge_object_onto`.
///
/// Revert sensitivity: a link of the sacrifice's redirected exile to Day (`should_track_exiled_by_source` over the resolving LTB) returns Gemrazer. Reverting D4 no longer turns this row red (measured); the zone-scan D4 guard is `living_death_under_rest_in_peace_keeps_the_merged_component_exiled`.
#[test]
fn day_of_the_dragons_under_rest_in_peace_keeps_the_merged_component_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let day = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Day of the Dragons", DAY_OF_THE_DRAGONS)
        .with_mana_cost(cost(
            4,
            vec![
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
        ))
        .id();
    let child = scenario
        .add_creature_from_oracle(P0, "Child of Night", 2, 1, CHILD_OF_NIGHT)
        .with_subtypes(vec!["Vampire"])
        .id();
    let gemrazer = scenario
        .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
        .with_mana_cost(cost(1, vec![ManaCostShard::White]))
        .id();
    let mut pool = mana(ManaType::Blue, 7);
    pool.extend(mana(ManaType::White, 2));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner.cast(day).resolve();
    drain(&mut runner);
    let dragons = tokens(runner.state(), "Dragon");
    assert_eq!(dragons.len(), 1, "reach guard: Day made one Dragon");
    let dragon = dragons[0];
    stage(&mut runner, gemrazer, dragon, MergeSide::Bottom);

    runner.cast(disenchant).target_object(day).resolve();
    drain(&mut runner);

    assert_eq!(
        zone_of(&runner, day),
        Some(Zone::Exile),
        "reach guard: Rest in Peace exiled the destroyed Day, so its leave trigger ran"
    );
    assert_eq!(
        zone_of(&runner, dragon),
        None,
        "reach guard: the sacrificed Dragon token ceased (CR 704.5d)"
    );
    assert_eq!(
        zone_of(&runner, child),
        Some(Zone::Battlefield),
        "CR 607.2a: the card Day's ETB exiled returns"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Exile),
        "CR 607.2a: Gemrazer was exiled by Rest in Peace, not by Day's ETB, so it is not one of the exiled cards"
    );
}

/// L1 — Living Death under Rest in Peace (P1's), with P0's Grizzly Bears on the battlefield
/// and Gemrazer merged under it, and P0's Child of Night card in the graveyard. Living Death
/// exiles each player's creature cards from their graveyard, sacrifices all creatures, then
/// "puts all cards they exiled this way onto the battlefield". The reader is
/// `ChangeZoneAll { origin: None }` over `TrackedSetFiltered { caused_by: Exiled }`, which the
/// linked-exile binding leaves unchanged because the chain holds an exile publisher. Rest in
/// Peace redirects the sacrifice of the Bears to exile and, with it, its component Gemrazer
/// (CR 730.3 puts each component into its zone with the survivor, CR 614.6 replaces the
/// graveyard move). The set's member (the Bears) is stamped `Sacrificed`, because the cause is
/// the action, not the landing zone; Gemrazer inherits that stamp (D4), so the reader does not
/// find it even though it sits in exile (CR 730.3c + CR 608.2c). Child of Night, exiled this
/// way, returns.
///
/// SYNTHETIC STEP: the merge is staged with `merge_object_onto`.
///
/// Revert sensitivity: a component path that ignores `caused_by` (`member_cause_matches`)
/// finds Gemrazer in the scan and puts it onto the battlefield (measured red).
#[test]
fn living_death_under_rest_in_peace_keeps_the_merged_component_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let bears = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, GRIZZLY_BEARS)
        .id();
    let gemrazer = scenario
        .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    let child = scenario
        .add_creature_to_graveyard(P0, "Child of Night", 2, 1)
        .id();
    let living_death = scenario
        .add_spell_to_hand_from_oracle(P0, "Living Death", false, LIVING_DEATH)
        .with_mana_cost(cost(3, vec![ManaCostShard::Black, ManaCostShard::Black]))
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Black, 5));
    let mut runner = scenario.build();
    stage(&mut runner, gemrazer, bears, MergeSide::Bottom);

    runner.cast(living_death).resolve();
    drain(&mut runner);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "the resolution completed: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        zone_of(&runner, child),
        Some(Zone::Battlefield),
        "reach guard: the reader ran and returned the member stamped Exiled"
    );
    assert_eq!(
        zone_of(&runner, bears),
        Some(Zone::Exile),
        "the survivor is a Sacrificed member, and Rest in Peace redirected it to exile"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Exile),
        "D4: Gemrazer's member was sacrificed, not exiled this way, so it is not put onto the battlefield (CR 730.3c + CR 608.2c + CR 614.6)"
    );
}

/// The plain-board sibling of the Rest in Peace row: without Rest in Peace
/// Gemrazer lands in the graveyard. Day's return has the explicit origin Exile,
/// so the scan is `[Exile]` and the graveyard component is never evaluated: this
/// board holds by origin, not by D4, and pins no phase-2 code. It documents the
/// outcome.
///
/// SYNTHETIC STEP: the merge is staged with `merge_object_onto`.
#[test]
fn day_of_the_dragons_plain_board_keeps_the_merged_component_in_the_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let day = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Day of the Dragons", DAY_OF_THE_DRAGONS)
        .with_mana_cost(cost(
            4,
            vec![
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
        ))
        .id();
    scenario
        .add_creature_from_oracle(P0, "Child of Night", 2, 1, CHILD_OF_NIGHT)
        .with_subtypes(vec!["Vampire"]);
    let gemrazer = scenario
        .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
        .with_mana_cost(cost(1, vec![ManaCostShard::White]))
        .id();
    let mut pool = mana(ManaType::Blue, 7);
    pool.extend(mana(ManaType::White, 2));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner.cast(day).resolve();
    drain(&mut runner);
    let dragons = tokens(runner.state(), "Dragon");
    assert_eq!(dragons.len(), 1, "reach guard: Day made one Dragon");
    let dragon = dragons[0];
    stage(&mut runner, gemrazer, dragon, MergeSide::Bottom);

    runner.cast(disenchant).target_object(day).resolve();
    drain(&mut runner);

    assert_eq!(
        zone_of(&runner, dragon),
        None,
        "reach guard: the sacrificed Dragon token ceased (CR 704.5d)"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "Gemrazer was put into the graveyard by the sacrifice and is not returned"
    );
}

/// Bristlebud Farmer with a Food token merged over Gemrazer and an empty
/// library. The attack trigger sacrifices the Food, then "mill three cards" with
/// nothing to mill; "put a permanent card from among them into your hand" reads
/// the Mill's own fresh, empty set (CR 608.2c), not the Food sacrifice's set, so
/// Gemrazer, in the graveyard as a component of the sacrificed Food, is not
/// offered and stays there.
///
/// SYNTHETIC STEPS: the merge is staged with `merge_object_onto` (the second
/// Food is removed so a single Food is available to sacrifice), and the Farmer's
/// summoning sickness is cleared directly. This row holds by the Mill's fresh
/// set and pins the outcome rather than a phase-2 gate.
#[test]
fn bristlebud_farmer_empty_library_does_not_offer_the_merged_component() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let farmer = scenario
        .add_creature_to_hand_from_oracle(P0, "Bristlebud Farmer", 5, 5, BRISTLEBUD_FARMER)
        .with_mana_cost(cost(2, vec![ManaCostShard::Green, ManaCostShard::Green]))
        .with_subtypes(vec!["Plant", "Druid"])
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Green, 4));
    let mut runner = scenario.build();
    runner.cast(farmer).resolve();
    drain(&mut runner);
    let foods = tokens(runner.state(), "Food");
    assert_eq!(
        foods.len(),
        2,
        "reach guard: the Farmer made two Food tokens"
    );
    {
        let state = runner.state_mut();
        state.battlefield.retain(|id| *id != foods[1]);
        state.objects.remove(&foods[1]);
    }
    assert!(
        runner.state().players[0].library.is_empty(),
        "reach guard: the library is empty, so the Mill publishes an empty set"
    );
    stage(&mut runner, gemrazer, foods[0], MergeSide::Bottom);
    ready(&mut runner, farmer);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(farmer, AttackTarget::Player(P1))])
        .expect("declare");
    for _ in 0..30 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("Bristlebud Farmer: unexpected prompt {other:?}"),
        }
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "the attack trigger's resolution completed, so no prompt offered Gemrazer: {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        zone_of(&runner, foods[0]),
        None,
        "reach guard: the attack trigger sacrificed the Food (it ceased)"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "Gemrazer is not offered to the Mill's empty 'from among them' set"
    );
}

/// Fortune, Loyal Steed's saddled-attack delayed trigger reads a set a prior
/// effect published, bound as a concrete id carried into the resolution, never
/// the resolution's own chain: such a read is not admitted to find components
/// (the D7 gate). The carried set here is the single-cause fallback set Syphon
/// Flesh published for P1's token-topped merged survivor, with Gemrazer a
/// component in the graveyard.
///
/// SYNTHETIC STEPS: P1's token (a plain creature flagged as a token) is merged
/// over Gemrazer with `merge_object_onto`, and Fortune and its saddler have
/// summoning sickness cleared directly. Only Gemrazer's zone is asserted: where
/// Fortune and the saddler end up is a separate known defect.
///
/// Revert sensitivity: a gate that admits carried ids would let the reader's bare
/// `TrackedSet` leg find Gemrazer through the survivor's pin and exile it.
#[test]
fn fortune_carried_set_does_not_find_the_merged_component() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fortune = scenario
        .add_creature_from_oracle(P0, "Fortune, Loyal Steed", 2, 4, FORTUNE_LOYAL_STEED)
        .with_subtypes(vec!["Beast", "Mount"])
        .id();
    let saddler = scenario.add_creature(P0, "Rider", 1, 1).id();
    let syphon = scenario
        .add_spell_to_hand_from_oracle(P0, "Syphon Flesh", false, SYPHON_FLESH)
        .with_mana_cost(cost(4, vec![ManaCostShard::Black]))
        .id();
    let p1_token = scenario
        .add_creature(P1, "Soldier", 1, 1)
        .with_subtypes(vec!["Soldier"])
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P1, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Black, 5));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&p1_token)
        .unwrap()
        .is_token = true;
    stage(&mut runner, gemrazer, p1_token, MergeSide::Bottom);
    runner.cast(syphon).resolve();
    drain(&mut runner);
    assert_eq!(
        zone_of(&runner, p1_token),
        None,
        "reach guard: Syphon Flesh sacrificed P1's merged token, which ceased"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "reach guard: the component is in the graveyard before the saddled attack"
    );

    ready(&mut runner, fortune);
    ready(&mut runner, saddler);
    runner
        .act(GameAction::SaddleMount {
            mount_id: fortune,
            creature_ids: vec![],
        })
        .expect("enter saddle");
    runner
        .act(GameAction::SaddleMount {
            mount_id: fortune,
            creature_ids: vec![saddler],
        })
        .expect("pay saddle");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&fortune].is_saddled,
        "reach guard: Fortune is saddled, so the attack trigger will fire"
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(fortune, AttackTarget::Player(P1))])
        .expect("declare");
    drain(&mut runner);
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "reach guard: the attack trigger created its end-of-combat delayed trigger"
    );
    let _ = runner.combat_damage();
    for _ in 0..30 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty()
                    && runner.state().phase == Phase::PostCombatMain =>
            {
                break;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("Fortune: unexpected prompt {other:?}"),
        }
    }
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach guard: the end-of-combat delayed trigger fired and was consumed"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
            && runner.state().phase == Phase::PostCombatMain,
        "the delayed trigger's resolution completed: {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "CR 730.3c does not reach a carried set: Gemrazer stays in the graveyard"
    );
}

/// R-RR. Reno and Rude: "Menace\nWhenever Reno and Rude deals combat damage
/// to a player, exile the top card of that player's library. Then you may
/// sacrifice another creature or artifact. If you do, you may play the
/// exiled card this turn, and mana of any type can be spent to cast it."
///
/// Board: a Soldier token with Gemrazer merged under it; Rest in Peace ("When
/// this enchantment enters, exile all graveyards.\nIf a card or token would
/// be put into a graveyard from anywhere, exile it instead."); the damaged
/// player's library is empty. The trigger exiles nothing and sacrifices the
/// token (CR 701.21a); Rest in Peace exiles the token and its component
/// instead (CR 614.1a, CR 730.3). The set holds only the token.
///
/// CR 730.3c + CR 608.2c: "the exiled card" is what the library exile
/// produced; the line has two publishers (`ExileTop`, `Sacrifice`), so the
/// grant's chain rung refuses the finding (D7, B1) and Gemrazer, an exiled
/// card the trigger never exiled from the library, gains no permission.
///
/// Before this change: Gemrazer has no `PlayFromExile` (raw grant).
/// Mutation (a grant reader skips the chain rung and the
/// publisher count and keeps only the stamp veto): Gemrazer gains `PlayFromExile`.
///
/// SYNTHETIC STEPS: the token is a plain creature flagged as a token, the merge
/// is staged with `merge_object_onto`, and Reno and Rude's summoning sickness is
/// cleared directly.
#[test]
fn reno_and_rude_grant_does_not_reach_a_sacrificed_tokens_component() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);
    let reno = scenario
        .add_creature_from_oracle(P0, "Reno and Rude", 2, 1, RENO_AND_RUDE)
        .with_subtypes(vec!["Human", "Assassin"])
        .id();
    let token = scenario
        .add_creature(P0, "Soldier", 1, 1)
        .with_subtypes(vec!["Soldier"])
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    stage(&mut runner, gemrazer, token, MergeSide::Bottom);
    ready(&mut runner, reno);
    assert!(
        runner.state().players[1].library.is_empty(),
        "reach guard: the damaged player's library is empty"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(reno, AttackTarget::Player(P1))])
        .expect("Reno and Rude attacks");
    let _ = runner.combat_damage();
    // The trigger's "you may sacrifice" prompt is P0's. Of the prompts that
    // accept `DecideOptionalEffect { accept: true }` (the optional-effect,
    // opponent-may and tribute choices), only the optional-effect choice is
    // answered by the ability's controller, so the actor assertion and the
    // accepted answer together identify the prompt.
    assert_eq!(
        runner.state().waiting_for.acting_player(),
        Some(P0),
        "the trigger's optional sacrifice is offered to its controller"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("sacrifice the token");

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty(),
        "the trigger resolved: {:?}",
        state.waiting_for
    );
    let set = state
        .chain_tracked_set_id
        .expect("reach guard: the trigger published its chain set");
    assert_eq!(
        state.tracked_object_sets.get(&set),
        Some(&vec![token]),
        "reach guard: the set holds only the sacrificed token"
    );
    assert_eq!(
        state
            .tracked_set_member_causes
            .get(&set)
            .and_then(|causes| causes.get(&token))
            .copied(),
        Some(ThisWayCause::Sacrificed),
        "reach guard: the token is stamped Sacrificed (CR 701.21a)"
    );
    assert!(
        !state.objects.contains_key(&token),
        "CR 704.5d: the token ceased to exist"
    );
    let gem = &state.objects[&gemrazer];
    assert_eq!(
        gem.zone,
        Zone::Exile,
        "CR 614.1a: Rest in Peace exiled the component instead"
    );
    assert_eq!(
        gem.split_from_merge_survivor,
        Some(ObjectIncarnationRef {
            object_id: token,
            incarnation: 1,
        }),
        "reach guard: Gemrazer is pinned to the token's successor"
    );
    assert!(
        !gem.casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::PlayFromExile { .. })),
        "CR 730.3c + CR 608.2c: two publishers, so the grant does not find Gemrazer: {:?}",
        gem.casting_permissions
    );
}

/// Voyager Staff's delayed return is bound to the creature it exiled (CR
/// 603.7c): the delayed trigger carries a concrete `TrackedSet` id for that
/// creature and its recorded incarnation, and releases the set once it returns
/// it. The binding never reads the resolution's own chain, so the CR 730.3c
/// finding cannot widen it.
///
/// A plain creature (no merge) pins the unchanged binding shape exactly.
///
/// Revert sensitivity: an admission that let the bound read find components
/// would write a `TrackedSetFiltered` and skip the release, so the delayed
/// effect would differ from the bare `TrackedSet` shape and the set maps would
/// keep the id.
#[test]
fn voyager_staff_delayed_return_binds_the_bare_set_and_releases_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let staff = scenario
        .add_artifact_from_oracle(P0, "Voyager Staff", VOYAGER_STAFF)
        .id();
    let x = scenario
        .add_creature(P1, "Creature X", 2, 2)
        .with_subtypes(vec!["Beast"])
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));
    let mut runner = scenario.build();
    runner.activate(staff, 0).target_object(x).resolve();
    drain(&mut runner);

    assert_eq!(
        zone_of(&runner, x),
        Some(Zone::Exile),
        "reach guard: the staff exiled the creature"
    );
    let delayed = &runner.state().delayed_triggers;
    assert_eq!(delayed.len(), 1, "one delayed return is installed");
    let set_id = match &delayed[0].ability.effect {
        Effect::ChangeZoneAll {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::TrackedSet { id },
            ..
        } => *id,
        other => panic!("expected the bare TrackedSet return, got {other:?}"),
    };
    assert_eq!(
        delayed[0].ability.target_incarnations,
        vec![ObjectIncarnationRef::of(x, 1)],
        "the delayed return is bound to the exiled creature's incarnation"
    );

    runner.advance_to_end_step();
    drain(&mut runner);

    let state = runner.state();
    assert_eq!(
        state.objects[&x].zone,
        Zone::Battlefield,
        "the delayed trigger returned the creature"
    );
    assert_eq!(state.objects[&x].incarnation, 2);
    assert!(!state.tracked_object_sets.contains_key(&set_id));
    assert!(!state.tracked_set_member_causes.contains_key(&set_id));
    assert!(!state.tracked_set_participants.contains_key(&set_id));
    assert!(!state.tracked_set_member_origins.contains_key(&set_id));
}
