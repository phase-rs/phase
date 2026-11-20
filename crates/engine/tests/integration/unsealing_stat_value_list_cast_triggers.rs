//! Sarkhan's Unsealing and Kozilek's Unsealing — RUNTIME witnesses for the
//! exact-value number list in a cast trigger's stat qualifier ("power 4, 5, or
//! 6" / "mana value 4, 5, or 6").
//!
//! Oracle (verified against MTGJSON 5.3.0+20261021 and the card-data export):
//!   Sarkhan's Unsealing:
//!     "Whenever you cast a creature spell with power 4, 5, or 6, this
//!     enchantment deals 4 damage to any target."
//!     "Whenever you cast a creature spell with power 7 or greater, this
//!     enchantment deals 4 damage to each opponent and each creature and
//!     planeswalker they control."
//!   Kozilek's Unsealing:
//!     "Devoid (This card has no color.)"
//!     "Whenever you cast a creature spell with mana value 4, 5, or 6, create
//!     two 0/1 colorless Eldrazi Spawn creature tokens with "Sacrifice this
//!     token: Add {C}.""
//!     "Whenever you cast a creature spell with mana value 7 or greater, draw
//!     three cards."
//!
//! Rulings quoted:
//!   R1 (Sarkhan's Unsealing): "Each of the triggered abilities of Sarkhan's
//!   Unsealing resolves before the spell that caused it to trigger. It
//!   resolves even if that spell is countered."
//!   R2 (Sarkhan's Unsealing): "... Sarkhan's unsealing checks its power only
//!   as you finish paying costs; it doesn't matter what the spell's power is
//!   as you begin to cast it or when the triggered ability resolves."
//!   R4 (Kozilek's Unsealing): "If you cast a creature spell with {X} in its
//!   mana cost, use the value of X that was chosen when it was cast to
//!   determine its mana value."
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 601.2i: abilities that trigger when a spell is cast trigger as the
//!     spell becomes cast, so the qualifier reads the spell on the stack.
//!   - CR 603.3: a triggered ability is put on the stack the next time a
//!     player would receive priority, above the spell that caused it.
//!   - CR 208.1: power is the first number of a creature's power/toughness.
//!   - CR 202.3: an object's mana value is the total mana in its mana cost.
//!   - CR 202.3e: on the stack, X in a mana cost is the announced value.
//!   - CR 107.3a: X is announced while the spell is being cast.
//!   - CR 115.4: "any target" is a creature, player, planeswalker, or battle.
//!   - CR 120.3: damage results (life loss for players, marked damage for
//!     creatures).
//!   - CR 111.1: some effects create tokens.
//!   - CR 121.1: a player draws a card by putting the top card of their
//!     library into their hand.
//!
//! Trigger-count instrument: the triggered abilities on the stack sourced by
//! the Unsealing, read at the post-cast priority window (CR 601.2i + CR
//! 603.3), before anything resolves.
//!
//! Discrimination: each test includes a value-5-or-6 case, so it fails if the
//! list parse is reverted (the qualifier then reads only "power 4" /
//! "mana value 4").

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityDefinition;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const SARKHAN: &str = "Whenever you cast a creature spell with power 4, 5, or 6, this enchantment deals 4 damage to any target.\nWhenever you cast a creature spell with power 7 or greater, this enchantment deals 4 damage to each opponent and each creature and planeswalker they control.";

const KOZILEK: &str = "Devoid (This card has no color.)\nWhenever you cast a creature spell with mana value 4, 5, or 6, create two 0/1 colorless Eldrazi Spawn creature tokens with \"Sacrifice this token: Add {C}.\"\nWhenever you cast a creature spell with mana value 7 or greater, draw three cards.";

/// Kozilek's Unsealing's two trigger lines. Devoid goes on through the
/// keyword path, so its reminder text is never parsed as an ability line.
fn kozilek_trigger_lines() -> &'static str {
    KOZILEK
        .split_once('\n')
        .expect("Kozilek's Unsealing has a Devoid line")
        .1
}

fn chain_has_unimplemented(ability: &AbilityDefinition) -> bool {
    matches!(*ability.effect, Effect::Unimplemented { .. })
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
}

/// Reach-guard: the Unsealing carries exactly its two `SpellCast` triggers, and
/// neither trigger body holds an `Unimplemented` link.
fn assert_two_clean_spell_cast_triggers(runner: &GameRunner, unsealing: ObjectId) {
    let object = runner
        .state()
        .objects
        .get(&unsealing)
        .expect("the Unsealing is on the battlefield");
    let triggers: Vec<_> = object
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| &entry.definition)
        .collect();
    assert_eq!(
        triggers
            .iter()
            .filter(|definition| definition.mode == TriggerMode::SpellCast)
            .count(),
        2,
        "the Unsealing must register both SpellCast triggers, got {triggers:?}"
    );
    assert!(
        triggers.iter().all(|definition| definition
            .execute
            .as_deref()
            .is_some_and(|execute| !chain_has_unimplemented(execute))),
        "no Unsealing trigger body may hold an Unimplemented link, got {triggers:?}"
    );
}

/// The triggered abilities on the stack whose source is `unsealing`.
fn unsealing_triggers_on_stack(runner_state: &GameState, unsealing: ObjectId) -> usize {
    runner_state
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == unsealing
            )
        })
        .count()
}

fn count_named_on_battlefield(
    state: &GameState,
    player: engine::types::player::PlayerId,
    name: &str,
) -> usize {
    state
        .objects
        .values()
        .filter(|object| {
            object.name == name && object.zone == Zone::Battlefield && object.controller == player
        })
        .count()
}

fn mana_units(ty: ManaType, count: u32) -> impl Iterator<Item = ManaUnit> {
    (0..count).map(move |_| ManaUnit::new(ty, ObjectId(0), false, Vec::new()))
}

/// Where a cast finished: the prompt the game waits on and the stack entries
/// left, kept whole so a failure shows the actual prompt and stack.
#[derive(Debug)]
struct Settled {
    waiting_for: WaitingFor,
    stack: Vec<ObjectId>,
}

impl Settled {
    fn of(state: &GameState) -> Self {
        Settled {
            waiting_for: state.waiting_for.clone(),
            stack: state.stack.iter().map(|entry| entry.id).collect(),
        }
    }

    /// The cast and every trigger it caused resolved: an empty stack and a
    /// priority window.
    fn assert_at_priority(&self, observation: &impl std::fmt::Debug) {
        assert!(
            matches!(self.waiting_for, WaitingFor::Priority { .. }),
            "{observation:?}"
        );
        assert!(self.stack.is_empty(), "{observation:?}");
    }
}

#[derive(Debug)]
struct SarkhanObservation {
    triggers_on_stack: usize,
    /// When a trigger went on the stack: the stack entry ids and P1's life
    /// change right after the topmost entry (the trigger) resolved.
    after_trigger: Option<(Vec<ObjectId>, i32)>,
    spell: ObjectId,
    p1_life_delta: i32,
    filler_zone: Zone,
    cast_zone: Zone,
    settled: Settled,
}

/// Cast a vanilla P0 creature of the given power (cost {R}) with Sarkhan's
/// Unsealing on P0's battlefield and a 1/4 creature on P1's. A 4/5/6 trigger
/// targets P1 (the declared player intent).
fn cast_into_sarkhan(power: i32) -> SarkhanObservation {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sarkhan = scenario
        .add_enchantment_from_oracle(P0, "Sarkhan's Unsealing", SARKHAN)
        .id();
    let filler = scenario
        .add_creature_from_oracle(P1, "Opponent Filler", 1, 4, "")
        .id();
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, &format!("Test Power {power}"), power, power, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, mana_units(ManaType::Red, 1).collect());
    let mut runner = scenario.build();
    assert_two_clean_spell_cast_triggers(&runner, sarkhan);

    let mut commit = runner.cast(spell).target_player(P1).commit();
    let triggers_on_stack = unsealing_triggers_on_stack(commit.state(), sarkhan);
    let after_trigger = (triggers_on_stack > 0).then(|| {
        let p1_life = |state: &GameState| {
            state
                .players
                .iter()
                .find(|player| player.id == P1)
                .expect("P1 exists")
                .life
        };
        let life_before = p1_life(commit.state());
        let depth = commit.state().stack.len();
        // CR 603.3: the trigger sits above the spell, so passing priority
        // resolves it first.
        for _ in 0..4 {
            if commit.state().stack.len() < depth {
                break;
            }
            commit
                .act(GameAction::PassPriority)
                .expect("passing priority resolves the top of the stack");
        }
        let stack = commit.state().stack.iter().map(|entry| entry.id).collect();
        (stack, p1_life(commit.state()) - life_before)
    });
    let outcome = commit.resolve();

    SarkhanObservation {
        triggers_on_stack,
        after_trigger,
        spell,
        p1_life_delta: outcome.life_delta(P1),
        filler_zone: outcome.zone_of(filler),
        cast_zone: outcome.zone_of(spell),
        settled: Settled::of(outcome.state()),
    }
}

/// R1: the trigger resolved (P1 already lost 4) while the cast creature spell
/// was still alone on the stack beneath it.
fn assert_trigger_resolved_before_spell(observation: &SarkhanObservation) {
    assert_eq!(
        observation.after_trigger,
        Some((vec![observation.spell], -4)),
        "the trigger must resolve first, leaving only the creature spell on the stack, \
         got {observation:?}"
    );
}

/// The 4/5/6 ability alone fired: one trigger, 4 damage to P1, the filler
/// untouched (the 7+ ability would have killed it), and the cast creature
/// resolved after the trigger (R1).
fn assert_four_five_six_fired_alone(observation: &SarkhanObservation, power: i32) {
    assert_eq!(
        observation.triggers_on_stack, 1,
        "power {power}: exactly one Sarkhan trigger must go on the stack, got {observation:?}"
    );
    assert_trigger_resolved_before_spell(observation);
    // CR 115.4 + CR 120.3: 4 damage to the declared player target.
    assert_eq!(
        observation.p1_life_delta, -4,
        "power {power}: the 4/5/6 ability must deal 4 to P1, got {observation:?}"
    );
    assert_eq!(
        observation.filler_zone,
        Zone::Battlefield,
        "power {power}: the 7+ ability must not also fire, got {observation:?}"
    );
    assert_eq!(observation.cast_zone, Zone::Battlefield);
    observation.settled.assert_at_priority(&observation);
}

#[test]
fn sarkhans_unsealing_power_4_5_6_deal_4_to_chosen_target() {
    for power in [4, 5, 6] {
        assert_four_five_six_fired_alone(&cast_into_sarkhan(power), power);
    }
}

#[test]
fn sarkhans_unsealing_power_3_below_list_does_not_trigger() {
    // Positive control: a power-5 cast fires the 4/5/6 ability, so the
    // trigger instrument is live.
    assert_four_five_six_fired_alone(&cast_into_sarkhan(5), 5);

    let three = cast_into_sarkhan(3);
    assert_eq!(
        three.triggers_on_stack, 0,
        "power 3 is not 4, 5, or 6 and not 7 or greater, got {three:?}"
    );
    assert_eq!(three.after_trigger, None, "{three:?}");
    assert_eq!(three.p1_life_delta, 0, "{three:?}");
    assert_eq!(three.filler_zone, Zone::Battlefield, "{three:?}");
    assert_eq!(three.cast_zone, Zone::Battlefield, "{three:?}");
    three.settled.assert_at_priority(&three);
}

#[test]
fn sarkhans_unsealing_power_7_takes_only_the_seven_or_greater_ability() {
    // Paired list case: power 6 fires only the 4/5/6 ability.
    assert_four_five_six_fired_alone(&cast_into_sarkhan(6), 6);

    let seven = cast_into_sarkhan(7);
    assert_eq!(
        seven.triggers_on_stack, 1,
        "power 7 must fire only the 7+ ability, got {seven:?}"
    );
    assert_trigger_resolved_before_spell(&seven);
    // CR 120.3: exactly 4 to P1 — the 4/5/6 ability (also aimed at P1) did
    // not fire too.
    assert_eq!(seven.p1_life_delta, -4, "{seven:?}");
    // CR 120.3 + CR 704.5g: the 7+ ability dealt 4 to the 1/4 filler.
    assert_eq!(seven.filler_zone, Zone::Graveyard, "{seven:?}");
    assert_eq!(seven.cast_zone, Zone::Battlefield, "{seven:?}");
    seven.settled.assert_at_priority(&seven);
}

#[derive(Debug)]
struct KozilekObservation {
    triggers_on_stack: usize,
    spawn: usize,
    drawn: i64,
    settled: Settled,
}

/// Build P0's board: Kozilek's Unsealing (Devoid through the keyword path) and
/// a three-card library for the 7+ draw. Returns the Unsealing's id.
fn add_kozilek(scenario: &mut GameScenario) -> ObjectId {
    let kozilek = scenario
        .add_enchantment_from_oracle(P0, "Kozilek's Unsealing", "")
        .from_oracle_text_with_keywords(&["Devoid"], kozilek_trigger_lines())
        .id();
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C"]);
    kozilek
}

/// Cast a vanilla P0 creature of the given mana value (cost {G} plus
/// `mana_value - 1` generic) with Kozilek's Unsealing on P0's battlefield.
fn cast_into_kozilek(mana_value: u32) -> KozilekObservation {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kozilek = add_kozilek(&mut scenario);
    let generic = mana_value - 1;
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, &format!("Test MV {mana_value}"), 1, 1, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        mana_units(ManaType::Green, 1)
            .chain(mana_units(ManaType::Colorless, generic))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_two_clean_spell_cast_triggers(&runner, kozilek);

    let commit = runner.cast(spell).commit();
    let triggers_on_stack = unsealing_triggers_on_stack(commit.state(), kozilek);
    let outcome = commit.resolve();
    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);

    KozilekObservation {
        triggers_on_stack,
        spawn: count_named_on_battlefield(outcome.state(), P0, "Eldrazi Spawn"),
        drawn: outcome.hand_drawn(P0),
        settled: Settled::of(outcome.state()),
    }
}

/// The 4/5/6 ability alone fired: one trigger, two Spawn (CR 111.1), and no
/// draw (the 7+ ability did not fire).
fn assert_two_spawn_alone(observation: &KozilekObservation, mana_value: u32) {
    assert_eq!(
        observation.triggers_on_stack, 1,
        "MV {mana_value}: exactly one Kozilek trigger must go on the stack, got {observation:?}"
    );
    assert_eq!(
        observation.spawn, 2,
        "MV {mana_value}: the 4/5/6 ability creates two Eldrazi Spawn, got {observation:?}"
    );
    assert_eq!(
        observation.drawn, 0,
        "MV {mana_value}: the 7+ ability must not also fire, got {observation:?}"
    );
    observation.settled.assert_at_priority(&observation);
}

#[test]
fn kozileks_unsealing_mana_value_4_5_6_create_two_spawn() {
    for mana_value in [4, 5, 6] {
        assert_two_spawn_alone(&cast_into_kozilek(mana_value), mana_value);
    }
}

#[test]
fn kozileks_unsealing_mana_value_3_and_7_boundaries() {
    // Paired list case: MV 6 fires only the 4/5/6 ability.
    assert_two_spawn_alone(&cast_into_kozilek(6), 6);

    let three = cast_into_kozilek(3);
    assert_eq!(three.triggers_on_stack, 0, "{three:?}");
    assert_eq!(three.spawn, 0, "{three:?}");
    assert_eq!(three.drawn, 0, "{three:?}");
    three.settled.assert_at_priority(&three);

    let seven = cast_into_kozilek(7);
    assert_eq!(seven.triggers_on_stack, 1, "{seven:?}");
    assert_eq!(seven.spawn, 0, "{seven:?}");
    // CR 121.1: the 7+ ability draws three cards.
    assert_eq!(seven.drawn, 3, "{seven:?}");
    seven.settled.assert_at_priority(&seven);
}

#[derive(Debug)]
struct XCastObservation {
    mana_value_on_stack: u32,
    triggers_on_stack: usize,
    spawn: usize,
    drawn: usize,
    settled: Settled,
}

/// Cast one textless creature body costing {X}{G} with X announced as `x`,
/// driven through the public actions: `CastSpell` → `ChooseXValue` →
/// `ChooseX` → mana payment. Reads the spell's mana value and the Kozilek
/// triggers at the post-cast priority window, then resolves everything.
fn cast_x_body_into_kozilek(x: u32) -> XCastObservation {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kozilek = add_kozilek(&mut scenario);
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Test X Body", 1, 1, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        mana_units(ManaType::Green, 1)
            .chain(mana_units(ManaType::Colorless, x))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_two_clean_spell_cast_triggers(&runner, kozilek);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the X body is legal");
    // CR 107.3a: X is announced while casting, so the cast pauses for it.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "the {{X}}{{G}} body must ask for X, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseX { value: x })
        .expect("announce X");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            // CR 601.2h: finalize the mana payment.
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize the mana payment");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected cast prompt {other:?}"),
        }
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast must commit to a priority window, got {:?}",
        runner.state().waiting_for
    );

    let state = runner.state();
    let spell_entry = state
        .stack
        .iter()
        .find(|entry| matches!(entry.kind, StackEntryKind::Spell { .. }))
        .expect("the X body is on the stack");
    // CR 202.3e: on the stack, X is the announced value.
    let mana_value_on_stack = state.objects[&spell_entry.id].effective_mana_value();
    let triggers_on_stack = unsealing_triggers_on_stack(state, kozilek);
    let hand_at_commit = state.players[0].hand.len();

    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&spell].zone, Zone::Battlefield);
    XCastObservation {
        mana_value_on_stack,
        triggers_on_stack,
        spawn: count_named_on_battlefield(state, P0, "Eldrazi Spawn"),
        drawn: state.players[0].hand.len() - hand_at_commit,
        settled: Settled::of(state),
    }
}

/// R4 + CR 107.3a + CR 202.3e: the X announced while casting decides the
/// spell's mana value on the stack, and that value decides which Kozilek
/// trigger fires. The three casts differ only in the announced X.
#[test]
fn kozileks_unsealing_counts_announced_x_in_mana_value() {
    // X = 4 → MV 5: the 4/5/6 ability.
    let five = cast_x_body_into_kozilek(4);
    assert_eq!(five.mana_value_on_stack, 5, "{five:?}");
    assert_eq!(five.triggers_on_stack, 1, "{five:?}");
    assert_eq!(five.spawn, 2, "{five:?}");
    assert_eq!(five.drawn, 0, "{five:?}");
    five.settled.assert_at_priority(&five);

    // X = 2 → MV 3: no ability.
    let three = cast_x_body_into_kozilek(2);
    assert_eq!(three.mana_value_on_stack, 3, "{three:?}");
    assert_eq!(three.triggers_on_stack, 0, "{three:?}");
    assert_eq!(three.spawn, 0, "{three:?}");
    assert_eq!(three.drawn, 0, "{three:?}");
    three.settled.assert_at_priority(&three);

    // X = 6 → MV 7: the 7+ ability.
    let seven = cast_x_body_into_kozilek(6);
    assert_eq!(seven.mana_value_on_stack, 7, "{seven:?}");
    assert_eq!(seven.triggers_on_stack, 1, "{seven:?}");
    assert_eq!(seven.spawn, 0, "{seven:?}");
    assert_eq!(seven.drawn, 3, "{seven:?}");
    seven.settled.assert_at_priority(&seven);
}
