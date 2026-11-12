//! "When the last <type> counter is removed from <subject> [while it's
//! exiled], ..." triggers fire only on the removal that leaves none of that
//! kind (CR 122.1), and a printed last-time-counter trigger on a suspend card
//! sits beside the keyword's own free-cast trigger (CR 702.62a).
//!
//! Oracle text below is verbatim from Scryfall `cards/named`.

use engine::ai_support::legal_actions;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORCISH_MINE: &str = "Enchant land\nThis Aura enters with three ore counters on it.\nAt the beginning of your upkeep and whenever enchanted land becomes tapped, remove an ore counter from this Aura.\nWhen the last ore counter is removed from this Aura, destroy enchanted land and this Aura deals 2 damage to that land's controller.";

const RIFTMARKED_KNIGHT: &str = "Protection from black; flanking (Whenever a creature without flanking blocks this creature, the blocking creature gets -1/-1 until end of turn.)\nSuspend 3—{1}{W}{W}\nWhen the last time counter is removed from this card while it's exiled, create a 2/2 black Knight creature token with flanking, protection from white, and haste.";

const VAMPIRE_HEXMAGE: &str =
    "First strike\nSacrifice this creature: Remove all counters from target permanent.";

const LOTUS_FIELD: &str = "Hexproof\nThis land enters tapped.\nWhen this land enters, sacrifice two lands.\n{T}: Add three mana of any one color.";

const DISENCHANT: &str = "Destroy target artifact or enchantment.";

const STRIP_MINE: &str = "{T}: Add {C}.\n{T}, Sacrifice this land: Destroy target land.";

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.";

fn counters(state: &GameState, id: ObjectId, kind: &CounterType) -> u32 {
    state.objects[&id].counters.get(kind).copied().unwrap_or(0)
}

fn life(state: &GameState, player: engine::types::player::PlayerId) -> i32 {
    state.players[player.0 as usize].life
}

fn attach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&attachment)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    let host_obj = runner.state_mut().objects.get_mut(&host).unwrap();
    if !host_obj.attachments.contains(&attachment) {
        host_obj.attachments.push(attachment);
    }
    runner.state_mut().layers_dirty.mark_full();
}

/// From P1's end step into P0's upkeep.
fn enter_p0_upkeep(runner: &mut GameRunner) {
    {
        let state = runner.state_mut();
        state.phase = Phase::End;
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().phase, Phase::Upkeep, "reach: P0's upkeep");
    assert_eq!(runner.state().active_player, P0, "reach: P0's turn");
}

/// Resolve the whole stack. CR 115.10a: nothing on these boards asks for a
/// target once the stack is resolving — Orcish Mine's "enchanted land" is
/// affected, not targeted — so any target prompt is a failure.
fn settle(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the stack did not resolve");
}

/// From P1's end step into P0's upkeep, then resolve everything that
/// triggered there.
fn run_p0_upkeep(runner: &mut GameRunner) {
    enter_p0_upkeep(runner);
    settle(runner);
}

/// From P1's end step into P0's upkeep, then pass priority until Orcish
/// Mine's last-counter trigger waits on the stack (its upkeep removal took the
/// last ore counter) with P0 holding priority.
fn mine_last_counter_trigger_on_stack(runner: &mut GameRunner, mine: ObjectId) {
    let ore = parse_counter_type("ore");
    enter_p0_upkeep(runner);
    for _ in 0..64 {
        let state = runner.state();
        if counters(state, mine, &ore) == 0
            && stack_triggers_from(state, mine) == 1
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            return;
        }
        assert!(
            !(state.stack.is_empty() && counters(state, mine, &ore) == 0),
            "the last-counter trigger never reached the stack"
        );
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the last-counter trigger did not reach the stack");
}

/// P0 passes with Orcish Mine's last-counter trigger on the stack, so P1 may
/// respond.
fn p1_gets_priority(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "reach: P1 holds priority with the trigger on the stack"
    );
}

/// CR 122.1: Orcish Mine's last-counter trigger waits for the removal that
/// leaves no ore counters. The first upkeep removal (2 -> 1) does nothing
/// more; the second (1 -> 0) destroys the enchanted land and deals 2 damage
/// to that land's controller (CR 120.3a), and the Aura then goes to the
/// graveyard as a state-based action (CR 704.5m).
#[test]
fn orcish_mine_destroys_land_and_damages_controller_on_last_ore_counter() {
    let ore = parse_counter_type("ore");
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc.add_basic_land(P1, ManaColor::Red);
    let mine = sc
        .add_creature(P0, "Orcish Mine", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], ORCISH_MINE)
        .id();
    // The enters-with-three-counters replacement is not under test.
    sc.with_counter(mine, ore.clone(), 2);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    sc.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    let mut runner = sc.build();
    attach(&mut runner, mine, land);

    run_p0_upkeep(&mut runner);
    // Reach guard + threshold discriminator: the upkeep trigger removed one
    // ore counter, and a non-last removal does not fire the last-counter
    // trigger.
    assert_eq!(counters(runner.state(), mine, &ore), 1, "first removal");
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mine].zone, Zone::Battlefield);
    assert_eq!(life(runner.state(), P1), 20);

    run_p0_upkeep(&mut runner);
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "the enchanted land is destroyed"
    );
    assert_eq!(
        life(runner.state(), P1),
        18,
        "2 damage to the land's controller"
    );
    assert_eq!(
        life(runner.state(), P0),
        20,
        "the Aura's controller is spared"
    );
    assert_eq!(runner.state().objects[&mine].zone, Zone::Graveyard);
}

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

/// Orcish Mine with one ore counter left, attached to `land`, so P0's next
/// upkeep removal takes the last one.
fn mine_on(sc: &mut GameScenario) -> ObjectId {
    let mine = sc
        .add_creature(P0, "Orcish Mine", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], ORCISH_MINE)
        .id();
    // The enters-with-three-counters replacement is not under test.
    sc.with_counter(mine, parse_counter_type("ore"), 1);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    sc.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    mine
}

/// CR 115.10a + CR 303.4f: Orcish Mine put onto an opponent's hexproof Lotus
/// Field (an Aura put onto the battlefield other than by resolving targets
/// nothing). "Destroy enchanted land" names the land without targeting it, so
/// hexproof (CR 702.11b) can't stop the trigger: no target prompt, the land is
/// destroyed, and its controller is dealt 2 damage.
#[test]
fn orcish_mine_destroys_an_opponents_hexproof_land_without_targeting_it() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc.add_land_from_oracle(P1, "Lotus Field", LOTUS_FIELD).id();
    let mine = mine_on(&mut sc);
    let mut runner = sc.build();
    attach(&mut runner, mine, land);
    assert!(
        runner.state().objects[&land].has_keyword(&Keyword::Hexproof),
        "reach: Lotus Field has hexproof"
    );

    // Reach: the upkeep removal took the last ore counter and the Mine's
    // trigger went on the stack without asking for a target.
    mine_last_counter_trigger_on_stack(&mut runner, mine);
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "the hexproof enchanted land is destroyed"
    );
    assert_eq!(life(runner.state(), P1), 18, "2 damage to its controller");
    assert_eq!(life(runner.state(), P0), 20);
}

/// CR 608.2h: Orcish Mine is disenchanted with its trigger on the stack. Its
/// own last known information says which land it enchanted, and that land
/// hasn't moved, so the land is still destroyed and its controller damaged.
#[test]
fn orcish_mine_disenchanted_in_response_still_destroys_its_land() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc.add_basic_land(P1, ManaColor::Red);
    let mine = mine_on(&mut sc);
    let disenchant = sc
        .add_spell_to_hand_from_oracle(P1, "Disenchant", true, DISENCHANT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = sc.build();
    attach(&mut runner, mine, land);

    mine_last_counter_trigger_on_stack(&mut runner, mine);
    p1_gets_priority(&mut runner);
    let _ = runner.cast(disenchant).target_object(mine).commit();
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&mine].zone,
        Zone::Graveyard,
        "reach: Disenchant destroyed the Mine"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "the land it last enchanted is destroyed"
    );
    assert_eq!(life(runner.state(), P1), 18);
}

/// Who controls the enchanted land when Strip Mine destroys it.
#[derive(Clone, Copy)]
enum LandController {
    Owner,
    /// P1 owns the land and P0 controls it.
    Opponent,
}

/// CR 608.2h + CR 704.5m: with the trigger on the stack, P1 sacrifices Strip
/// Mine to destroy the enchanted land; the unattached Mine then goes to the
/// graveyard. "Destroy enchanted land" does nothing (the land is gone), but
/// "that land's controller" is read from the land's last known information.
/// Returns (P0 life, P1 life).
fn orcish_mine_land_strip_mined(controller: LandController) -> (i32, i32) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc.add_basic_land(P1, ManaColor::Red);
    let bystander = sc.add_basic_land(P0, ManaColor::Green);
    let strip = sc.add_land_from_oracle(P1, "Strip Mine", STRIP_MINE).id();
    let mine = mine_on(&mut sc);
    let mut runner = sc.build();
    if let LandController::Opponent = controller {
        let object = runner.state_mut().objects.get_mut(&land).unwrap();
        object.base_controller = Some(P0);
        object.controller = P0;
        runner.state_mut().layers_dirty.mark_full();
    }
    attach(&mut runner, mine, land);
    let destroy = runner.state().objects[&strip]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::Destroy { .. }))
        .expect("Strip Mine's destroy ability");

    mine_last_counter_trigger_on_stack(&mut runner, mine);
    p1_gets_priority(&mut runner);
    runner
        .activate(strip, destroy)
        .target_object(land)
        .resolve();
    settle(&mut runner);
    let s = runner.state();
    assert_eq!(s.objects[&land].zone, Zone::Graveyard, "reach: land gone");
    assert_eq!(s.objects[&mine].zone, Zone::Graveyard, "reach: CR 704.5m");
    assert!(
        s.zone_changes_this_turn
            .iter()
            .any(|r| r.object_id == mine && r.attached_to.is_none()),
        "reach: the Mine left attached to nothing"
    );
    assert!(
        s.zone_changes_this_turn
            .iter()
            .any(|r| r.object_id == land && r.attachments.iter().any(|a| a.object_id == mine)),
        "reach: the land's departure lists the Mine"
    );
    assert_eq!(
        s.objects[&bystander].zone,
        Zone::Battlefield,
        "no other land is destroyed"
    );
    (life(s, P0), life(s, P1))
}

#[test]
fn orcish_mine_land_strip_mined_in_response_damages_its_last_controller() {
    assert_eq!(
        orcish_mine_land_strip_mined(LandController::Owner),
        (20, 18)
    );
}

/// CR 608.2h: the damage follows the land's controller as it last existed on
/// the battlefield (P0), not its owner.
#[test]
fn orcish_mine_stolen_land_strip_mined_damages_the_controller_not_the_owner() {
    assert_eq!(
        orcish_mine_land_strip_mined(LandController::Opponent),
        (18, 20)
    );
}

/// CR 400.7 + CR 608.2h: P1 flickers the enchanted land in response. The land
/// that returns is a new object the Mine never enchanted, so it is not
/// destroyed; the damage goes to the controller of the land the Mine last
/// enchanted.
#[test]
fn orcish_mine_land_flickered_in_response_destroys_nothing() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc.add_basic_land(P1, ManaColor::Red);
    let second = sc.add_basic_land(P1, ManaColor::Green);
    let mine = mine_on(&mut sc);
    let flicker = sc
        .add_spell_to_hand_from_oracle(P1, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = sc.build();
    attach(&mut runner, mine, land);
    let incarnation = runner.state().objects[&land].incarnation;

    mine_last_counter_trigger_on_stack(&mut runner, mine);
    p1_gets_priority(&mut runner);
    let _ = runner
        .cast(flicker)
        .target_objects(&[land, second])
        .commit();
    settle(&mut runner);
    let s = runner.state();
    assert_ne!(
        s.objects[&land].incarnation, incarnation,
        "reach: the land returned as a new object"
    );
    assert_eq!(s.objects[&mine].zone, Zone::Graveyard, "reach: CR 704.5m");
    assert_eq!(
        s.objects[&land].zone,
        Zone::Battlefield,
        "the returned land is not the enchanted land"
    );
    assert_eq!(life(s, P1), 18, "2 damage to the last controller");
    assert_eq!(life(s, P0), 20);
}

/// Answer only trigger-ordering prompts (identity order); never pass priority.
/// CR 115.10a: Orcish Mine's trigger targets nothing, so it asks for no target
/// as it is put on the stack.
fn place_pending_triggers(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            _ => return,
        }
    }
    panic!("trigger placement did not settle");
}

/// Triggered abilities from `source` currently on the stack.
fn stack_triggers_from(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == source
            )
        })
        .count()
}

/// CR 603.2c + CR 122.1 (regression guard; passes at base and with the
/// per-batch "the last counter" fold): Vampire Hexmage removes all three ore
/// counters from Orcish Mine in ONE removal event, which removes "the last"
/// ore counter once, so the Mine triggers exactly once — a per-counter split
/// must never reach a threshold definition. Life alone is not a
/// discriminator (later copies would find no enchanted land and deal nothing),
/// so the stack count carries the assertion.
#[test]
fn last_counter_trigger_fires_once_when_several_counters_leave_at_once() {
    let ore = parse_counter_type("ore");
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc.add_basic_land(P1, ManaColor::Red);
    let mine = sc
        .add_creature(P0, "Orcish Mine", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], ORCISH_MINE)
        .id();
    let hexmage = sc
        .add_creature_from_oracle(P0, "Vampire Hexmage", 2, 1, VAMPIRE_HEXMAGE)
        .id();
    // The enters-with-three-counters replacement is not under test.
    sc.with_counter(mine, ore.clone(), 3);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    sc.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    let mut runner = sc.build();
    attach(&mut runner, mine, land);

    runner
        .act(GameAction::ActivateAbility {
            source_id: hexmage,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Vampire Hexmage");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(mine)),
            })
            .expect("target Orcish Mine");
    }
    // Pass priority only until Hexmage's ability has resolved.
    for _ in 0..64 {
        let hexmage_on_stack = runner.state().stack.iter().any(|entry| {
            matches!(entry.kind, StackEntryKind::ActivatedAbility { .. })
                && entry.source_id == hexmage
        });
        if !hexmage_on_stack {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                place_pending_triggers(&mut runner);
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    place_pending_triggers(&mut runner);

    // Reach guard: every ore counter is gone, in one removal.
    assert_eq!(
        counters(runner.state(), mine, &ore),
        0,
        "reach guard: 3 -> 0"
    );
    assert_eq!(
        stack_triggers_from(runner.state(), mine),
        1,
        "removing the last ore counter fires the Mine once"
    );

    // Resolve everything.
    for _ in 0..64 {
        if runner.state().stack.is_empty() {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                place_pending_triggers(&mut runner);
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "the enchanted land is destroyed"
    );
    assert_eq!(
        life(runner.state(), P1),
        18,
        "2 damage to the land's controller"
    );
}

/// Drives a suspended card's last-counter upkeep through both triggers it
/// fires, accepting the keyword's "you may cast it" prompt.
fn drive_suspend_last_counter(runner: &mut GameRunner) -> bool {
    let mut accepted = false;
    for _ in 0..300 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the free cast");
                accepted = true;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::Priority { .. } if accepted => return true,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    accepted
}

/// CR 702.62a + CR 603.3b: removing Riftmarked Knight's last time counter in
/// exile fires both the keyword's free-cast trigger and the printed token
/// trigger. Both resolve: the Knight is cast without paying its mana cost and
/// enters with haste, and the printed trigger creates its Knight token.
#[test]
fn riftmarked_knight_last_time_counter_casts_it_and_creates_token() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let knight = sc
        .add_creature_to_hand(P0, "Riftmarked Knight", 2, 2)
        .from_oracle_text_with_keywords(&["Flanking", "Suspend", "Protection"], RIFTMARKED_KNIGHT)
        .id();
    let mut runner = sc.build();

    for mana in [ManaType::White, ManaType::White, ManaType::Colorless] {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
    let suspend_idx = legal_actions(runner.state())
        .into_iter()
        .find_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == knight => Some(ability_index),
            _ => None,
        })
        .expect("Riftmarked Knight offers its Suspend activation from hand");
    runner
        .activate(knight, suspend_idx)
        .target_object(knight)
        .resolve();
    assert_eq!(runner.state().objects[&knight].zone, Zone::Exile);
    assert_eq!(counters(runner.state(), knight, &CounterType::Time), 3);

    // One counter left; the next upkeep removes the last one.
    runner
        .state_mut()
        .objects
        .get_mut(&knight)
        .unwrap()
        .counters
        .insert(CounterType::Time, 1);
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    assert!(
        drive_suspend_last_counter(&mut runner),
        "the keyword's free-cast prompt must appear"
    );
    let s = runner.state();
    assert_eq!(
        s.objects[&knight].zone,
        Zone::Battlefield,
        "the Knight was cast from exile and resolved"
    );
    assert!(
        s.objects[&knight].has_keyword(&Keyword::Haste),
        "a creature cast with suspend has haste"
    );
    let tokens = s
        .battlefield
        .iter()
        .filter(|id| {
            let o = &s.objects[*id];
            o.is_token && o.controller == P0 && o.card_types.subtypes.iter().any(|t| t == "Knight")
        })
        .count();
    assert_eq!(tokens, 1, "the printed trigger creates one Knight token");
}

const REGENERATIONS_RESTORED: &str = "Vanishing 12 (This enchantment enters with twelve time counters on it. At the beginning of your upkeep, remove a time counter from it. When the last is removed, sacrifice it.)\nWhenever one or more time counters are removed from this enchantment, scry 1 and you gain 1 life. Then if this enchantment has no time counters on it, exile it. When you do, take an extra turn after this one.";

/// What happened while P0's upkeep resolved for Regenerations Restored.
struct RegenerationsUpkeep {
    scried: bool,
    ordering_prompted: bool,
}

/// Drive P0's upkeep through the public action pipeline. When both Regenerations
/// Restored's own trigger and Vanishing's sacrifice trigger are put on the stack
/// together (CR 603.3b), the controller orders them: `regen_resolves_first`
/// places the "Whenever one or more time counters are removed" trigger on top.
fn run_regenerations_upkeep(
    runner: &mut GameRunner,
    regen_resolves_first: bool,
) -> RegenerationsUpkeep {
    {
        let state = runner.state_mut();
        state.phase = Phase::End;
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().phase, Phase::Upkeep, "reach: P0's upkeep");
    assert_eq!(runner.state().active_player, P0, "reach: P0's turn");
    let mut report = RegenerationsUpkeep {
        scried: false,
        ordering_prompted: false,
    };
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return report,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::ScryChoice { cards, .. } => {
                assert_eq!(cards.len(), 1, "scry 1 reveals exactly one card");
                report.scried = true;
                // Keep the card on top.
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("answer scry");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                report.ordering_prompted = true;
                let regen = triggers
                    .iter()
                    .position(|t| t.description.starts_with("Whenever one or more time"))
                    .expect("the Regenerations Restored trigger is being ordered");
                // Index 0 is placed first (resolves last, CR 603.3b + CR 405.2).
                let mut order: Vec<usize> = (0..triggers.len()).filter(|&i| i != regen).collect();
                if regen_resolves_first {
                    order.push(regen);
                } else {
                    order.insert(0, regen);
                }
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the only legal target");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the upkeep stack did not resolve");
}

fn regenerations_scenario(time_counters: u32) -> (GameRunner, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let regen = sc
        .add_creature(P0, "Regenerations Restored", 0, 0)
        .as_enchantment()
        .from_oracle_text_with_keywords(&["Vanishing"], REGENERATIONS_RESTORED)
        .id();
    // The enters-with-twelve-counters replacement is not under test.
    sc.with_counter(regen, CounterType::Time, time_counters);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    sc.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    (sc.build(), regen)
}

/// CR 122.1 + CR 608.2c: Vanishing's upkeep removal (3 -> 2) triggers
/// Regenerations Restored: scry 1, gain 1 life, and — because time counters
/// remain — the "if this enchantment has no time counters" exile does not
/// happen, so there is no reflexive extra turn (CR 603.12).
#[test]
fn regenerations_restored_nonlast_removal_scries_and_gains_life_without_exiling() {
    let (mut runner, regen) = regenerations_scenario(3);
    let turns_before = runner.state().extra_turns.len();

    let report = run_regenerations_upkeep(&mut runner, true);

    let s = runner.state();
    // Reach guards: the removal happened and the trigger's effect chain ran.
    assert_eq!(counters(s, regen, &CounterType::Time), 2, "one removed");
    assert!(report.scried, "the scry choice was offered");
    assert!(
        !report.ordering_prompted,
        "a nonlast removal has one trigger"
    );
    assert_eq!(life(s, P0), 21, "gain 1 life");
    // Discriminators: nothing exiled, no extra turn.
    assert_eq!(s.objects[&regen].zone, Zone::Battlefield);
    assert_eq!(s.extra_turns.len(), turns_before, "no extra turn");
}

/// CR 603.3b + CR 608.2c + CR 603.12: removing the last counter triggers both
/// Regenerations Restored and Vanishing's sacrifice (CR 702.63a). With the
/// Regenerations Restored trigger ordered to resolve first, the enchantment is
/// still on the battlefield with no time counters, so it is exiled and the
/// reflexive "when you do" grants an extra turn.
#[test]
fn regenerations_restored_last_removal_exiles_itself_and_grants_extra_turn() {
    let (mut runner, regen) = regenerations_scenario(1);
    let turns_before = runner.state().extra_turns.len();

    let report = run_regenerations_upkeep(&mut runner, true);

    let s = runner.state();
    assert_eq!(counters(s, regen, &CounterType::Time), 0, "last removed");
    assert!(report.ordering_prompted, "two triggers need ordering");
    assert!(report.scried, "the scry choice was offered");
    assert_eq!(life(s, P0), 21, "gain 1 life");
    assert_eq!(
        s.objects[&regen].zone,
        Zone::Exile,
        "exiled, not sacrificed"
    );
    assert_eq!(s.extra_turns.len(), turns_before + 1, "extra turn granted");
    assert_eq!(s.extra_turns.last().map(|t| t.player), Some(P0));
}

/// CR 400.7 + CR 603.12: if Vanishing's sacrifice resolves first, the
/// enchantment is already in the graveyard when Regenerations Restored tries to
/// exile "it", so the exile does not happen and "when you do" never triggers —
/// no extra turn. Scry and life gain still occur.
#[test]
fn regenerations_restored_sacrificed_first_gets_no_extra_turn() {
    let (mut runner, regen) = regenerations_scenario(1);
    let turns_before = runner.state().extra_turns.len();

    let report = run_regenerations_upkeep(&mut runner, false);

    let s = runner.state();
    assert!(report.ordering_prompted, "two triggers need ordering");
    assert!(report.scried, "the scry choice was offered");
    assert_eq!(life(s, P0), 21, "gain 1 life");
    assert_eq!(s.objects[&regen].zone, Zone::Graveyard, "sacrificed");
    assert_eq!(s.extra_turns.len(), turns_before, "no extra turn");
}
