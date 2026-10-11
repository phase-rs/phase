//! Regression for issue #6877: a spell-cast "copy that spell" / self-cast
//! "copy this spell" trigger must copy the spell as it last existed on the
//! stack once an earlier trigger already moved it (bounced, countered) —
//! never a different stack object, and never nothing.
//!
//! https://github.com/phase-rs/phase/issues/6877

use engine::game::derived_views::ClientGameStateRef;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{CastingPermission, ResolutionCastCleanup, TargetRef};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::GameRng;
use engine::types::game_state::{
    CastPaymentMode, GameState, StackEntryKind, SyntheticTriggerProvenance, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::cast_this_way_gate_8721::{offered_card, settle_attack_trigger, to_declare_attackers};
use super::rules::AttackTarget;
use crate::support::shared_card_db;

// Krark's Partner line is omitted; it plays no part in a two-player game.
const KRARK: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
    If you lose the flip, return that spell to its owner's hand. \
    If you win the flip, copy that spell, and you may choose new targets for the copy.";

const DRAW_SPELL: &str = "Draw a card.";
const BOLT_SPELL: &str = "Lightning Bolt deals 3 damage to any target.";
const BRAIN_FREEZE: &str = "Target player mills three cards.\n\
    Storm (When you cast this spell, copy it for each spell cast before it this turn. \
    You may choose new targets for the copies.)";
const HESITATION: &str =
    "When a player casts a spell, sacrifice this enchantment and counter that spell.";
const SWARM_INTELLIGENCE: &str = "Whenever you cast an instant or sorcery spell, you may copy \
    that spell. You may choose new targets for the copy.";
const SAGE_OF_THE_SKIES: &str = "When you cast this spell, if you've cast another spell this \
    turn, copy this spell. (The copy becomes a token.)\nFlying, lifelink";
const UNSUBSTANTIATE: &str = "Return target spell or creature to its owner's hand.";
const CHAIN_OF_SMOG: &str = "Target player discards two cards. That player may copy this spell \
    and may choose a new target for that copy.";
const OGRE_BATTLECASTER: &str = "First strike\n\
    Whenever this creature attacks, you may cast target instant or sorcery card from your \
    graveyard by paying {R}{R} in addition to its other costs. If that spell would be put into \
    a graveyard, exile it instead. When you cast that spell, this creature gets +X/+0 until end \
    of turn, where X is that spell's mana value.";

fn floating_mana(n: usize, color: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn library_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .unwrap_or(0)
}

fn spells_cast_count(state: &GameState, player: PlayerId) -> usize {
    state
        .spells_cast_this_turn_by_player
        .get(&player)
        .map_or(0, |records| records.len())
}

fn spells_cast_count_named(state: &GameState, player: PlayerId, name: &str) -> usize {
    state
        .spells_cast_this_turn_by_player
        .get(&player)
        .map_or(0, |records| {
            records.iter().filter(|r| r.name == name).count()
        })
}

/// Drive `act()` through cast setup (targeting, mana payment, modal face
/// choice) until priority or an ordering prompt is reached.
fn commit_cast(runner: &mut GameRunner, spell: ObjectId, target: Option<TargetRef>) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted");
    for _ in 0..32 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
        ) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: target.clone(),
                    })
                    .expect("declared target must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pool-funded remainder must pay");
            }
            WaitingFor::ModalFaceChoice { .. } => {
                runner
                    .act(GameAction::ChooseModalFace { back_face: true })
                    .expect("modal face choice must be accepted");
            }
            other => panic!("unexpected waiting_for while committing cast: {other:?}"),
        }
    }
    panic!("cast did not reach Priority or OrderTriggers");
}

fn reseed(runner: &mut GameRunner, seed: u64) {
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
}

fn saw_coin(events: &[GameEvent], won: bool) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::CoinFlipped { won: w, .. } if *w == won))
}

fn saw_spell_copied(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCopied { .. }))
}

fn saw_spell_countered(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCountered { .. }))
}

/// Drain every `WaitingFor::OrderTriggers` prompt. `prefer_first`, if given,
/// is put last in submission order for its group — CR 603.3b: index 0 of
/// `order` selects the bottom of this controller's stack slot, so its last
/// entry resolves first (LIFO).
fn order_triggers(runner: &mut GameRunner, prefer_first: Option<ObjectId>) {
    let mut guard = 0;
    while let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        guard += 1;
        assert!(guard <= 16, "order_triggers: too many APNAP groups");
        let order: Vec<usize> = match prefer_first {
            Some(source_id) => {
                let mut order: Vec<usize> = (0..triggers.len())
                    .filter(|&i| triggers[i].source_id != source_id)
                    .collect();
                order.extend((0..triggers.len()).filter(|&i| triggers[i].source_id == source_id));
                order
            }
            None => (0..triggers.len()).collect(),
        };
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("OrderTriggers must succeed");
    }
}

/// Options for [`drive`].
struct Drive {
    /// Put this source's trigger(s) on top of their APNAP group (resolve
    /// first) whenever an `OrderTriggers` prompt names them.
    prefer_first: Option<ObjectId>,
    /// Auto-submit `KeepAllCopyTargets` at a `CopyRetarget` pause instead of
    /// stopping the drive there.
    keep_retargets: bool,
    /// Auto-answer `OptionalEffectChoice` / `OptionalCostChoice` with this
    /// accept/decline decision instead of stopping the drive there.
    accept_optional: Option<bool>,
}

impl Drive {
    fn new() -> Self {
        Self {
            prefer_first: None,
            keep_retargets: false,
            accept_optional: None,
        }
    }

    fn prefer(mut self, source_id: ObjectId) -> Self {
        self.prefer_first = Some(source_id);
        self
    }

    fn keep_retargets(mut self) -> Self {
        self.keep_retargets = true;
        self
    }

    fn accept(mut self) -> Self {
        self.accept_optional = Some(true);
        self
    }

    fn decline(mut self) -> Self {
        self.accept_optional = Some(false);
        self
    }
}

/// Pass priority (draining ordering/optional/retarget prompts per `opts`)
/// until the stack shrinks below its length at call time, or an unhandled
/// prompt is reached. Returns the events collected along the way.
fn drive(runner: &mut GameRunner, opts: &Drive) -> Vec<GameEvent> {
    order_triggers(runner, opts.prefer_first);
    let initial_stack_len = runner.state().stack.len();
    let mut events = Vec::new();
    for _ in 0..32 {
        order_triggers(runner, opts.prefer_first);
        // Prompt handling is checked BEFORE the stack-shrink stop condition:
        // a resolving entry is popped from the stack before its effect chain
        // runs (`resolve_top`), so a mid-resolution pause (CopyRetarget,
        // OptionalEffectChoice) can already show a shrunk stack while still
        // needing an answer here.
        let waiting = runner.state().waiting_for.clone();
        match waiting {
            WaitingFor::CopyRetarget { .. } if opts.keep_retargets => {
                match runner.act(GameAction::KeepAllCopyTargets) {
                    Ok(result) => {
                        events.extend(result.events);
                        continue;
                    }
                    Err(_) => break,
                }
            }
            WaitingFor::CopyRetarget { .. } => break,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::OptionalCostChoice { .. }
                if opts.accept_optional.is_some() =>
            {
                let accept = opts.accept_optional.unwrap();
                let action = if matches!(waiting, WaitingFor::OptionalEffectChoice { .. }) {
                    GameAction::DecideOptionalEffect { accept }
                } else {
                    GameAction::DecideOptionalCost { pay: accept }
                };
                match runner.act(action) {
                    Ok(result) => {
                        events.extend(result.events);
                        continue;
                    }
                    Err(_) => break,
                }
            }
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::OptionalCostChoice { .. } => {
                break
            }
            _ => {}
        }
        if runner.state().stack.len() < initial_stack_len {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => events.extend(result.events),
            Err(_) => break,
        }
    }
    events
}

fn setup_krarks_and_spell(
    krark_count: usize,
    seed: u64,
    spell_name: &str,
    spell_oracle: &str,
) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    // CR 704.5j: non-legendary creatures so the legend rule does not collapse
    // the group.
    for i in 0..krark_count {
        scenario.add_creature_from_oracle(P0, &format!("Krark {i}"), 2, 2, KRARK);
    }
    for i in 0..8 {
        scenario.add_spell_to_library_top(P0, &format!("Library {i}"), true);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, spell_name, true, spell_oracle)
        .id();
    scenario.with_mana_pool(P0, floating_mana(10, ManaType::Colorless));
    (scenario, spell)
}

fn krark_id(runner: &GameRunner, name: &str) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == name)
        .unwrap_or_else(|| panic!("{name} must be on the battlefield"))
}

/// CONTROL: winning while the cast spell is still on the stack copies it —
/// establishes that the harness can observe a copy at all.
#[test]
fn winning_trigger_copies_spell_still_on_stack() {
    let (scenario, spell) = setup_krarks_and_spell(2, 42, "Draw Spell", DRAW_SPELL);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);

    let lib_before = library_len(runner.state(), P0);
    reseed(&mut runner, 0);
    let win_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(
        saw_coin(&win_events, true),
        "seed 0 must be a WIN for the first trigger: {win_events:?}"
    );
    assert!(
        saw_spell_copied(&win_events) && library_len(runner.state(), P0) < lib_before,
        "win while on stack must copy and the copy must resolve; events={win_events:?}"
    );
}

/// CR 608.2h: two Krark triggers, lose then win — the leftover winning
/// trigger still copies the spell as it last existed on the stack, even
/// though the earlier lose already bounced it to hand.
#[test]
fn winning_trigger_copies_spell_bounced_by_earlier_trigger() {
    let (scenario, spell) = setup_krarks_and_spell(2, 42, "Draw Spell", DRAW_SPELL);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);
    order_triggers(&mut runner, None);
    assert_eq!(
        runner.state().stack.len(),
        3,
        "spell plus two Krark triggers must be on the stack: {:?}",
        runner.state().stack
    );

    reseed(&mut runner, 1);
    let lose_events = drive(&mut runner, &Drive::new());
    assert!(
        saw_coin(&lose_events, false),
        "seed 1 must be a LOSE for the first trigger: {lose_events:?}"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "lose must bounce the original to hand"
    );

    let lib_before = library_len(runner.state(), P0);
    reseed(&mut runner, 0);
    let win_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(
        saw_coin(&win_events, true),
        "seed 0 must be a WIN for the second trigger: {win_events:?}"
    );
    assert!(
        saw_spell_copied(&win_events) && library_len(runner.state(), P0) < lib_before,
        "win after bounce must create a copy that resolves (SpellCopied and a \
         resolved draw); events={win_events:?}"
    );
}

/// CR 707.10c: targeted instant, lose then win — the leftover win still
/// offers `CopyRetarget`, and the copy's default target is the original's.
#[test]
fn winning_trigger_offers_new_targets_for_copy_of_bounced_spell() {
    let (scenario, spell) = setup_krarks_and_spell(2, 42, "Lightning Bolt", BOLT_SPELL);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, Some(TargetRef::Player(P1)));
    order_triggers(&mut runner, None);

    reseed(&mut runner, 1);
    let lose_events = drive(&mut runner, &Drive::new());
    assert!(
        saw_coin(&lose_events, false),
        "seed 1 must be a LOSE for the first trigger: {lose_events:?}"
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);

    reseed(&mut runner, 0);
    let win_events = drive(&mut runner, &Drive::new());
    assert!(
        saw_coin(&win_events, true),
        "seed 0 must be a WIN for the second trigger: {win_events:?}"
    );
    match &runner.state().waiting_for {
        WaitingFor::CopyRetarget { target_slots, .. } => {
            assert_eq!(
                target_slots[0].current,
                Some(TargetRef::Player(P1)),
                "the departed spell's own target must be the copy's default target"
            );
        }
        other => panic!("targeted leftover copy expected to halt on CopyRetarget, got {other:?}"),
    }
}

/// CR 608.2h: a spell countered before a winning Krark trigger resolves still
/// gets copied. Hesitation is P1's (non-active), so under CR 603.3b APNAP its
/// trigger is placed on the stack after Krark's own and resolves first.
#[test]
fn winning_trigger_copies_spell_countered_before_it_resolved() {
    let (mut scenario, spell) = setup_krarks_and_spell(2, 0, "Draw Spell", DRAW_SPELL);
    scenario.add_enchantment_from_oracle(P1, "Hesitation", HESITATION);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);
    order_triggers(&mut runner, None);
    assert_eq!(
        runner.state().stack.len(),
        4,
        "spell plus two Krark triggers plus Hesitation's must be on the stack: {:?}",
        runner.state().stack
    );

    let counter_events = drive(&mut runner, &Drive::new());
    assert!(
        saw_spell_countered(&counter_events),
        "Hesitation must counter first under APNAP: {counter_events:?}"
    );

    let lib_before = library_len(runner.state(), P0);
    reseed(&mut runner, 0);
    let win_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(
        saw_coin(&win_events, true),
        "seed 0 must be a WIN for a remaining trigger: {win_events:?}"
    );
    assert!(
        saw_spell_copied(&win_events) && library_len(runner.state(), P0) < lib_before,
        "win after the spell was countered must still create a copy that resolves; \
         events={win_events:?}"
    );
}

/// CR 608.2h + CR 608.2d: Swarm Intelligence's optional copy of a
/// spell-cast trigger still offers (and creates) a copy after Hesitation
/// counters the spell first — accept branch.
#[test]
fn optional_copy_of_countered_spell_is_created_when_accepted() {
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Swarm Intelligence", SWARM_INTELLIGENCE);
    scenario.add_enchantment_from_oracle(P1, "Hesitation", HESITATION);
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("Library {i}"), true);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw Spell", true, DRAW_SPELL)
        .id();
    scenario.with_mana_pool(P0, floating_mana(10, ManaType::Colorless));
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);
    order_triggers(&mut runner, None);

    let counter_events = drive(&mut runner, &Drive::new());
    assert!(
        saw_spell_countered(&counter_events),
        "Hesitation must counter first under APNAP: {counter_events:?}"
    );

    let accept_events = drive(&mut runner, &Drive::new().accept().keep_retargets());
    assert!(
        saw_spell_copied(&accept_events),
        "accepting the optional copy of a countered spell must still create it: \
         {accept_events:?}"
    );
}

/// Decline branch of `optional_copy_of_countered_spell_is_created_when_accepted`: declining creates no copy.
#[test]
fn optional_copy_of_countered_spell_is_not_created_when_declined() {
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Swarm Intelligence", SWARM_INTELLIGENCE);
    scenario.add_enchantment_from_oracle(P1, "Hesitation", HESITATION);
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("Library {i}"), true);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw Spell", true, DRAW_SPELL)
        .id();
    scenario.with_mana_pool(P0, floating_mana(10, ManaType::Colorless));
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);
    order_triggers(&mut runner, None);

    let counter_events = drive(&mut runner, &Drive::new());
    assert!(
        saw_spell_countered(&counter_events),
        "Hesitation must counter first under APNAP: {counter_events:?}"
    );

    // `drive` stops as soon as ANY entry leaves the stack (here: Hesitation's
    // own trigger, which also removed the countered spell) — Swarm's own
    // trigger has not yet started resolving. One more `drive` call advances
    // through the fresh priority round it needs before it reaches its own
    // `OptionalEffectChoice` pause.
    let _ = drive(&mut runner, &Drive::new());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: the optional copy choice must actually be offered, got {:?}",
        runner.state().waiting_for
    );

    let decline_events = drive(&mut runner, &Drive::new().decline());
    assert!(
        !saw_spell_copied(&decline_events),
        "declining must create no copy: {decline_events:?}"
    );
}

/// CR 400.7 + CR 601.2i: the same card cast twice in one step, leaving the
/// stack each time with a different target — the OLDER trigger's copy must
/// carry the OLDER cast's target, never the newer recast's, even though both
/// casts share the same storage object id.
#[test]
fn older_trigger_copies_its_own_cast_not_a_later_recast() {
    let (scenario, bolt) = setup_krarks_and_spell(2, 0, "Lightning Bolt", BOLT_SPELL);
    let mut runner = scenario.build();

    // First cast: Bolt -> P1.
    commit_cast(&mut runner, bolt, Some(TargetRef::Player(P1)));
    order_triggers(&mut runner, None);
    assert_eq!(runner.state().stack.len(), 3);

    // Resolve one trigger with a LOSE — bounces the first Bolt to hand,
    // leaving the first cast's other Krark trigger waiting on the stack.
    reseed(&mut runner, 1);
    let lose_events = drive(&mut runner, &Drive::new());
    assert!(saw_coin(&lose_events, false), "{lose_events:?}");
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the first cast's other Krark trigger should remain: {:?}",
        runner.state().stack
    );

    // Recast the same Bolt object -> P0, while the older trigger still waits.
    commit_cast(&mut runner, bolt, Some(TargetRef::Player(P0)));
    order_triggers(&mut runner, None);
    assert_eq!(runner.state().stack.len(), 4);
    assert_eq!(
        spells_cast_count(runner.state(), P0),
        2,
        "reach guard: two SpellCast events for the one object id"
    );

    // The recast's first Krark trigger loses -> bounces the second Bolt to hand.
    reseed(&mut runner, 1);
    let new_lose_events = drive(&mut runner, &Drive::new());
    assert!(saw_coin(&new_lose_events, false), "{new_lose_events:?}");
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);

    // The recast's second Krark trigger wins -> copies the SECOND cast (keeps its P0 target); drive it
    // fully through so only the old trigger remains.
    reseed(&mut runner, 0);
    let new_win_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(saw_coin(&new_win_events, true), "{new_win_events:?}");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the old trigger should remain after the recast's pair resolves: {:?}",
        runner.state().stack
    );

    // Finally the OLDEST trigger wins. It must copy the FIRST cast's record
    // (target P1) — never the highest (most recent) departed record, which
    // would be the second cast's (target P0).
    reseed(&mut runner, 0);
    let old_win_events = drive(&mut runner, &Drive::new());
    assert!(saw_coin(&old_win_events, true), "{old_win_events:?}");
    match &runner.state().waiting_for {
        WaitingFor::CopyRetarget { target_slots, .. } => {
            assert_eq!(
                target_slots[0].current,
                Some(TargetRef::Player(P1)),
                "the OLDER trigger's pin must read the OLDER cast's departed record, \
                 not the highest (most recent) one"
            );
        }
        other => panic!("expected CopyRetarget for the old trigger's win, got {other:?}"),
    }
}

/// CR 702.40a + CR 113.7a + CR 400.7: the same card cast twice in one step
/// while its own first Storm trigger still waits — the OLDER Storm trigger's
/// copy must carry the OLDER cast's target, never the newer recast's, even
/// though both casts share the same storage object id.
#[test]
fn older_storm_trigger_copies_its_own_cast_not_a_later_recast() {
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Krark 0", 2, 2, KRARK);
    for i in 0..8 {
        scenario.add_spell_to_library_top(P0, &format!("Library P0 {i}"), true);
    }
    for i in 0..8 {
        scenario.add_spell_to_library_top(P1, &format!("Library P1 {i}"), true);
    }
    // A creature spell, not instant/sorcery, so Krark's own trigger does not
    // fire on it — only its cast counts toward Storm's copy count.
    let filler_spell = scenario
        .add_creature_to_hand(P0, "Filler Creature", 1, 1)
        .id();
    let brain_freeze = scenario
        .add_spell_to_hand(P0, "Brain Freeze", true)
        .from_oracle_text_with_keywords(&["Storm"], BRAIN_FREEZE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(10, ManaType::Colorless));
    let mut runner = scenario.build();
    let krark = krark_id(&runner, "Krark 0");

    commit_cast(&mut runner, filler_spell, None);
    let _ = drive(&mut runner, &Drive::new());

    // First cast: Brain Freeze -> P1. Krark preferred to resolve first, so its
    // own (first) Storm trigger is left waiting below it.
    commit_cast(&mut runner, brain_freeze, Some(TargetRef::Player(P1)));
    order_triggers(&mut runner, Some(krark));
    assert_eq!(
        runner.state().stack.len(),
        3,
        "spell plus Krark's trigger plus its own (first) Storm trigger: {:?}",
        runner.state().stack
    );

    reseed(&mut runner, 1);
    let lose_events = drive(&mut runner, &Drive::new().prefer(krark));
    assert!(saw_coin(&lose_events, false), "{lose_events:?}");
    assert_eq!(
        runner.state().objects[&brain_freeze].zone,
        Zone::Hand,
        "Krark's lose must bounce the first Brain Freeze cast to hand"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the first ('old') Storm trigger should remain: {:?}",
        runner.state().stack
    );

    // Recast the same Brain Freeze object -> P0, while the older Storm
    // trigger still waits.
    commit_cast(&mut runner, brain_freeze, Some(TargetRef::Player(P0)));
    order_triggers(&mut runner, Some(krark));
    assert_eq!(
        runner.state().stack.len(),
        4,
        "recast plus Krark's new trigger plus a new Storm trigger, above the \
         older waiting Storm trigger: {:?}",
        runner.state().stack
    );
    assert_eq!(
        spells_cast_count_named(runner.state(), P0, "Brain Freeze"),
        2,
        "reach guard: two SpellCast events for the one object id"
    );

    reseed(&mut runner, 1);
    let new_lose_events = drive(&mut runner, &Drive::new().prefer(krark));
    assert!(saw_coin(&new_lose_events, false), "{new_lose_events:?}");
    assert_eq!(
        runner.state().objects[&brain_freeze].zone,
        Zone::Hand,
        "Krark's lose must bounce the second Brain Freeze cast to hand"
    );
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the new Storm trigger and the older Storm trigger remain: {:?}",
        runner.state().stack
    );

    // Reach guard: both remaining entries are Storm-provenance triggers
    // sharing the one Brain Freeze object id, not e.g. leftover Krark
    // triggers or a TriggeringSource shape.
    let storm_entries = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == brain_freeze
                && matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility {
                        provenance: Some(SyntheticTriggerProvenance::Storm { .. }),
                        ..
                    }
                )
        })
        .count();
    assert_eq!(
        storm_entries,
        2,
        "reach guard: two Storm-provenance triggers must be waiting: {:?}",
        runner.state().stack
    );

    // The newer Storm trigger is on top and resolves first; keep its target.
    let new_storm_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(
        saw_spell_copied(&new_storm_events),
        "the newer Storm trigger must copy its own (newer) cast: {new_storm_events:?}"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the OLDER Storm trigger should remain: {:?}",
        runner.state().stack
    );

    // Finally the OLDER Storm trigger resolves. It must copy the FIRST
    // cast's record (target P1) — never the highest (most recent) departed
    // record, which would be the second cast's (target P0).
    let lib_before_p1 = library_len(runner.state(), P1);
    let _ = drive(&mut runner, &Drive::new());
    match &runner.state().waiting_for {
        WaitingFor::CopyRetarget { target_slots, .. } => {
            assert_eq!(
                target_slots[0].current,
                Some(TargetRef::Player(P1)),
                "the OLDER Storm trigger's pin must read the OLDER cast's \
                 departed record, not the highest (most recent) one"
            );
        }
        other => {
            panic!("expected CopyRetarget for the older Storm trigger's resolution, got {other:?}")
        }
    }
    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("KeepAllCopyTargets must succeed for the older Storm trigger's copy");
    let _ = drive(&mut runner, &Drive::new());
    assert_eq!(
        library_len(runner.state(), P1),
        lib_before_p1.saturating_sub(3),
        "the OLDER Storm copy must have resolved against the older cast's \
         target (P1), milling 3 as Brain Freeze's own effect does"
    );
}

/// CR 712.8a: a spell cast as a modal back face, then departed (bounced by an
/// earlier trigger) — the copy must be of the back face, not the front.
#[test]
fn copy_of_bounced_modal_back_face_spell_is_the_back_face() {
    let Some(db) = shared_card_db() else {
        eprintln!("skipping: no card database available");
        return;
    };
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Krark 0", 2, 2, KRARK);
    scenario.add_creature_from_oracle(P0, "Krark 1", 2, 2, KRARK);
    let card = scenario.add_real_card(P0, "Flamescroll Celebrant", Zone::Hand, db);
    let mut white = floating_mana(4, ManaType::White);
    white.extend(floating_mana(4, ManaType::Colorless));
    scenario.with_mana_pool(P0, white);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_eq!(
        runner.state().objects[&card].name,
        "Flamescroll Celebrant",
        "reach guard: the card in hand is named for its front face"
    );

    commit_cast(&mut runner, card, None);
    order_triggers(&mut runner, None);
    assert_eq!(runner.state().stack.len(), 3);

    reseed(&mut runner, 1);
    let lose_events = drive(&mut runner, &Drive::new());
    assert!(saw_coin(&lose_events, false), "{lose_events:?}");
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);

    reseed(&mut runner, 0);
    // A plain `drive` would run the untargeted copy all the way through its
    // own resolution (Revel in Silence has no targets, so nothing pauses it),
    // and the spell-copy token ceases to exist once resolved — reading its
    // name from `state.objects` after that is a stale lookup. Stop capturing
    // as soon as `SpellCopied` itself appears, while the copy is still live.
    order_triggers(&mut runner, None);
    let mut win_events = Vec::new();
    let mut copy_name = None;
    for _ in 0..8 {
        match runner.act(GameAction::PassPriority) {
            Ok(result) => win_events.extend(result.events),
            Err(_) => break,
        }
        if let Some(id) = win_events.iter().find_map(|event| match event {
            GameEvent::SpellCopied { object_id, .. } => Some(*object_id),
            _ => None,
        }) {
            copy_name = runner.state().objects.get(&id).map(|obj| obj.name.clone());
            break;
        }
    }
    assert!(saw_coin(&win_events, true), "{win_events:?}");
    assert_eq!(
        copy_name.as_deref(),
        Some("Revel in Silence"),
        "the copy must be the back face the departed spell was cast as: {win_events:?}"
    );
}

/// CR 400.7 + CR 500.2: a departed-spell record is cleared once the step in
/// which the spell left ends — it is not readable in a later step.
#[test]
fn departed_spell_record_is_cleared_when_the_step_ends() {
    let (scenario, spell) = setup_krarks_and_spell(1, 42, "Draw Spell", DRAW_SPELL);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);
    order_triggers(&mut runner, None);

    reseed(&mut runner, 1);
    let lose_events = drive(&mut runner, &Drive::new());
    assert!(saw_coin(&lose_events, false), "{lose_events:?}");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the stack must be empty before the step ends"
    );
    assert!(
        !runner.state().departed_stack_spells.is_empty(),
        "reach guard: a departed record must exist before the step ends"
    );

    let phase_before = runner.state().phase;
    runner.pass_both_players();
    assert_ne!(
        runner.state().phase,
        phase_before,
        "reach guard: passing with an empty stack must advance the step/phase"
    );
    assert!(
        runner.state().departed_stack_spells.is_empty(),
        "the departed record must not survive the step transition"
    );
}

/// CR 702.40a control: a spell with Storm stays on the stack -> its copies
/// are created.
#[test]
fn storm_copies_spell_still_on_stack() {
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Filler Creature", 1, 1);
    scenario.add_spell_to_library_top(P0, "Library P0", true);
    for i in 0..4 {
        scenario.add_spell_to_library_top(P1, &format!("Library P1 {i}"), true);
    }
    let filler_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Filler Spell", true, DRAW_SPELL)
        .id();
    let brain_freeze = scenario
        .add_spell_to_hand(P0, "Brain Freeze", true)
        .from_oracle_text_with_keywords(&["Storm"], BRAIN_FREEZE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(10, ManaType::Colorless));
    let mut runner = scenario.build();

    commit_cast(&mut runner, filler_spell, None);
    let _ = drive(&mut runner, &Drive::new());

    commit_cast(&mut runner, brain_freeze, Some(TargetRef::Player(P1)));
    order_triggers(&mut runner, None);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Brain Freeze plus its own Storm trigger: {:?}",
        runner.state().stack
    );

    let lib_before = library_len(runner.state(), P1);
    let storm_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(
        saw_spell_copied(&storm_events),
        "Storm's copy must still fire while the spell is on the stack: {storm_events:?}"
    );
    assert_eq!(
        library_len(runner.state(), P1),
        lib_before.saturating_sub(3),
        "the Storm copy must have resolved (mill 3)"
    );
}

/// CR 702.40a + CR 113.7a: a spell with Storm is returned to hand (by an
/// unrelated Krark trigger) before its own Storm trigger resolves — the
/// Storm copies are still created.
#[test]
fn storm_copies_spell_returned_to_hand_before_storm_resolves() {
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Krark 0", 2, 2, KRARK);
    for i in 0..4 {
        scenario.add_spell_to_library_top(P1, &format!("Library P1 {i}"), true);
    }
    // The filler ("a spell cast before it this turn") is itself a creature
    // spell, not an instant/sorcery — Krark's own trigger only cares about
    // Storm's later Brain Freeze cast, not this one.
    let filler_spell = scenario
        .add_creature_to_hand(P0, "Filler Creature", 1, 1)
        .id();
    let brain_freeze = scenario
        .add_spell_to_hand(P0, "Brain Freeze", true)
        .from_oracle_text_with_keywords(&["Storm"], BRAIN_FREEZE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(10, ManaType::Colorless));
    let mut runner = scenario.build();

    commit_cast(&mut runner, filler_spell, None);
    let _ = drive(&mut runner, &Drive::new());

    let krark = krark_id(&runner, "Krark 0");
    commit_cast(&mut runner, brain_freeze, Some(TargetRef::Player(P1)));
    // Order with Krark preferred to the top now, before any prompt is
    // answered — a later `.prefer(krark)` on `drive` has no effect once the
    // `OrderTriggers` prompt has already been answered.
    order_triggers(&mut runner, Some(krark));
    assert_eq!(
        runner.state().stack.len(),
        3,
        "Brain Freeze plus Krark's trigger plus its own Storm trigger: {:?}",
        runner.state().stack
    );

    reseed(&mut runner, 1);
    let lose_events = drive(&mut runner, &Drive::new().prefer(krark));
    assert!(
        saw_coin(&lose_events, false),
        "Krark must resolve first and lose: {lose_events:?}"
    );
    assert_eq!(
        runner.state().objects[&brain_freeze].zone,
        Zone::Hand,
        "Krark's lose must bounce Brain Freeze before Storm resolves"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only Storm's trigger remains"
    );

    let lib_before = library_len(runner.state(), P1);
    let storm_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(
        saw_spell_copied(&storm_events),
        "Storm's own trigger must still copy the departed spell: {storm_events:?}"
    );
    assert_eq!(
        library_len(runner.state(), P1),
        lib_before.saturating_sub(3),
        "the Storm copy must have resolved against the departed spell's target"
    );
}

/// CR 702.40a class: a self-cast copy trigger (Sage of the Skies, not Storm)
/// still copies its own spell after that spell is returned to hand before
/// the trigger resolves.
#[test]
fn self_cast_copy_trigger_copies_spell_returned_to_hand() {
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_spell_to_library_top(P0, "Library P0", true);
    let filler_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Filler Spell", true, DRAW_SPELL)
        .id();
    let sage = scenario
        .add_creature_to_hand_from_oracle(P0, "Sage of the Skies", 2, 1, SAGE_OF_THE_SKIES)
        .id();
    let unsubstantiate = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(10, ManaType::Colorless));
    let mut runner = scenario.build();

    commit_cast(&mut runner, filler_spell, None);
    let _ = drive(&mut runner, &Drive::new());

    commit_cast(&mut runner, sage, None);
    order_triggers(&mut runner, None);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Sage plus its own self-cast copy trigger: {:?}",
        runner.state().stack
    );

    // While Sage's trigger waits, bounce Sage itself off the stack.
    commit_cast(&mut runner, unsubstantiate, Some(TargetRef::Object(sage)));
    order_triggers(&mut runner, None);
    let bounce_events = drive(&mut runner, &Drive::new());
    assert_eq!(
        runner.state().objects[&sage].zone,
        Zone::Hand,
        "Unsubstantiate must bounce Sage before its own trigger resolves: {bounce_events:?}"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only Sage's own trigger should remain: {:?}",
        runner.state().stack
    );

    let copy_events = drive(&mut runner, &Drive::new().keep_retargets());
    assert!(
        saw_spell_copied(&copy_events),
        "Sage's self-cast trigger must still copy the departed spell: {copy_events:?}"
    );
    let token_on_battlefield = runner.state().battlefield.iter().any(|id| {
        let obj = &runner.state().objects[id];
        obj.is_token && obj.name == "Sage of the Skies"
    });
    assert!(
        token_on_battlefield,
        "the copy of a permanent spell must resolve onto the battlefield as a token \
         (CR 608.3f): battlefield={:?}",
        runner.state().battlefield
    );
}

/// CR 608.2h + CR 400.7 + CR 601.2i: Chain of Smog's own "may copy this
/// spell" is offered while the spell itself is resolving — the copy must
/// read the LIVE (currently resolving) cast, never an earlier departed
/// record of the same card recast at the same storage id. The first cast is
/// bounced by Unsubstantiate before it resolves (writing a departed record
/// targeting P1); the card is then recast targeting P0 and allowed to
/// resolve. If the resolving-spell lookup fell back to a departed record
/// keyed by storage id instead of the live entry, the offered copy's default
/// target would read P1 instead of P0.
#[test]
fn resolving_spell_copies_the_live_cast_not_an_earlier_departed_record() {
    let mut scenario = GameScenario::new_n_player(2, 0);
    scenario.at_phase(Phase::PreCombatMain);
    let chain = scenario
        .add_spell_to_hand_from_oracle(P0, "Chain of Smog", false, CHAIN_OF_SMOG)
        .id();
    let unsubstantiate = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsubstantiate", true, UNSUBSTANTIATE)
        .id();
    // P0 is the recast's own target, so P0 needs cards to discard.
    for i in 0..4 {
        scenario.add_card_to_hand(P0, &format!("Filler {i}"));
    }
    let mut mana = floating_mana(4, ManaType::Black);
    mana.extend(floating_mana(4, ManaType::Blue));
    mana.extend(floating_mana(8, ManaType::Colorless));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();

    // First cast: Chain of Smog -> P1.
    commit_cast(&mut runner, chain, Some(TargetRef::Player(P1)));
    assert_eq!(runner.state().stack.len(), 1);

    // Bounce it before it resolves, writing a departed record with target P1.
    commit_cast(&mut runner, unsubstantiate, Some(TargetRef::Object(chain)));
    let bounce_events = drive(&mut runner, &Drive::new());
    assert_eq!(
        runner.state().objects[&chain].zone,
        Zone::Hand,
        "Unsubstantiate must bounce the first Chain of Smog cast to hand: {bounce_events:?}"
    );

    // Recast the same object -> P0, while the first cast's departed record
    // still exists.
    commit_cast(&mut runner, chain, Some(TargetRef::Player(P0)));
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the recast is on the stack: {:?}",
        runner.state().stack
    );

    // Reach guard: the departed record for the first cast is still present
    // at this point, so the test is not vacuous — a lookup that ignored the
    // live cast entirely would still have somewhere to fall back to.
    assert!(
        runner.state().departed_stack_spells.contains_key(&chain),
        "reach guard: the first cast's departed record must exist before the \
         recast resolves: {:?}",
        runner.state().departed_stack_spells
    );

    // Drive through the recast's own resolution (discard choice, then the
    // optional-copy prompt) without touching the shared `drive` helper, which
    // does not handle `DiscardChoice`.
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DiscardChoice { count, cards, .. } => {
                let chosen: Vec<ObjectId> = cards.into_iter().take(count).collect();
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("P0 discards the recast's two cards");
            }
            WaitingFor::OptionalEffectChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the recast's own copy prompt");
            }
            other => panic!(
                "unexpected waiting_for while driving the recast to its copy prompt: {other:?}"
            ),
        }
    }
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                player, P0,
                "the recast's own copy prompt belongs to the targeted player (P0)"
            );
        }
        other => panic!("expected the recast's optional copy prompt, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P0 accepts the recast's copy");

    match &runner.state().waiting_for {
        WaitingFor::CopyRetarget { target_slots, .. } => {
            assert_eq!(
                target_slots[0].current,
                Some(TargetRef::Player(P0)),
                "the resolving spell's own copy must read the LIVE cast's target (P0), \
                 never the earlier departed record's (P1)"
            );
        }
        other => panic!("expected CopyRetarget for the recast's copy, got {other:?}"),
    }
}

/// The temporary `ExileWithAltCost` permission's cleanup, if the object
/// carries one.
fn resolution_cleanup(object: &GameObject) -> Option<&ResolutionCastCleanup> {
    object
        .casting_permissions
        .iter()
        .find_map(|permission| match permission {
            CastingPermission::ExileWithAltCost {
                resolution_cleanup: Some(cleanup),
                ..
            } => Some(cleanup),
            _ => None,
        })
}

fn assert_no_cleanup_authority(cleanup: &ResolutionCastCleanup, where_: &str) {
    assert_eq!(
        cleanup.offer_id, None,
        "{where_} must not carry offer_id: {cleanup:?}"
    );
    assert!(
        cleanup.delayed_trigger_receipts.is_empty(),
        "{where_} must not carry delayed_trigger_receipts: {cleanup:?}"
    );
}

/// CR 608.2g (paid graveyard cast): Ogre Battlecaster
/// casts Lightning Bolt from the graveyard and Hesitation counters it before
/// it resolves — no viewer's projection of the resulting `departed_stack_spells`
/// record carries the cast offer's server-only cleanup authority (`offer_id`,
/// `delayed_trigger_receipts`), on either the viewer filter or the direct
/// client serializer. Control: while the spell is still on the stack, its own
/// live projection already carries neither.
#[test]
fn departed_paid_cast_record_redacts_cleanup_authority_for_every_viewer() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ogre = scenario
        .add_creature_from_oracle(P0, "Ogre Battlecaster", 3, 3, OGRE_BATTLECASTER)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Hesitation", HESITATION);
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(ogre, AttackTarget::Player(P1))])
        .expect("Ogre must be a legal attacker");
    settle_attack_trigger(&mut runner, true);
    assert_eq!(
        offered_card(&runner),
        Some(bolt),
        "reach guard: the resolving trigger offers Bolt for casting now"
    );

    runner
        .act(GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept the during-resolution cast offer");
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay the offered cast's cost from lands");
            }
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting_for while paying the offered cast: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == bolt),
        "reach guard: the accepted Bolt is on the stack"
    );

    // CONTROL: while the spell is still live on the stack, its own
    // projection already carries neither cleanup authority — for either
    // player.
    let live_cleanup = resolution_cleanup(&runner.state().objects[&bolt])
        .expect("reach guard: the live cast's object carries the temporary permission");
    assert!(
        live_cleanup.offer_id.is_some(),
        "reach guard: the authoritative live permission carries an offer id"
    );
    for viewer in [P0, P1] {
        let projected = filter_state_for_viewer(runner.state(), viewer);
        let projected_cleanup = resolution_cleanup(&projected.objects[&bolt])
            .expect("live viewer projection must retain the temporary permission itself");
        assert_no_cleanup_authority(
            projected_cleanup,
            &format!("control: {viewer:?}'s live-spell projection"),
        );
    }

    // `drive` stops as soon as ANY stack entry leaves the stack — Ogre's own
    // delayed "when you cast that spell" pump trigger resolves first here,
    // so Hesitation's counter trigger needs a further `drive` call.
    order_triggers(&mut runner, None);
    let mut counter_events = drive(&mut runner, &Drive::new());
    for _ in 0..8 {
        if saw_spell_countered(&counter_events) {
            break;
        }
        let more = drive(&mut runner, &Drive::new());
        if more.is_empty() {
            break;
        }
        counter_events.extend(more);
    }
    assert!(
        saw_spell_countered(&counter_events),
        "Hesitation must counter the offered Bolt: {counter_events:?}"
    );
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Exile,
        "reach guard: Ogre's own rider exiles a countered offered spell instead of the graveyard"
    );

    // Positive reach guard: the UNPROJECTED authoritative record does carry
    // the cast offer's cleanup authority, so the negative assertions below
    // are not vacuous.
    let departed = runner
        .state()
        .departed_stack_spells
        .get(&bolt)
        .and_then(|incarnations| incarnations.values().next())
        .expect("reach guard: a departed record for the countered Bolt must exist");
    let authoritative_cleanup = resolution_cleanup(&departed.object)
        .expect("reach guard: the departed record's object retains the temporary permission");
    assert!(
        authoritative_cleanup.offer_id.is_some(),
        "reach guard: the unprojected departed record must carry offer_id"
    );
    assert!(
        !authoritative_cleanup.delayed_trigger_receipts.is_empty(),
        "reach guard: the unprojected departed record must carry delayed_trigger_receipts"
    );

    for viewer in [P0, P1] {
        let projected = filter_state_for_viewer(runner.state(), viewer);
        let projected_departed = projected
            .departed_stack_spells
            .get(&bolt)
            .and_then(|incarnations| incarnations.values().next())
            .expect("viewer projection must retain the departed record itself");
        let projected_cleanup = resolution_cleanup(&projected_departed.object)
            .expect("viewer projection must retain the temporary permission");
        assert_no_cleanup_authority(
            projected_cleanup,
            &format!("{viewer:?}'s filter_state_for_viewer departed record"),
        );

        // Direct client serializer: same boundary, through the wire shape,
        // decoded back into typed fields.
        let wire = serde_json::to_value(ClientGameStateRef::wrap(runner.state(), Some(viewer)))
            .expect("direct client wire serializes");
        let wire_state: GameState = serde_json::from_value(wire["state"].clone())
            .expect("direct client wire state must deserialize");
        let wire_departed = wire_state
            .departed_stack_spells
            .get(&bolt)
            .and_then(|incarnations| incarnations.values().next())
            .expect("direct client wire must retain the departed record itself");
        let wire_cleanup = resolution_cleanup(&wire_departed.object)
            .expect("direct client wire must retain the temporary permission");
        assert_no_cleanup_authority(
            wire_cleanup,
            &format!("{viewer:?}'s direct client wire departed record"),
        );
    }
}
