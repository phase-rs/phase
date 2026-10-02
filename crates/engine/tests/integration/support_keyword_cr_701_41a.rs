//! CR 701.41a — the `support N` keyword action, end to end.
//!
//! > 701.41a "Support N" on a permanent means "Put a +1/+1 counter on each of up
//! > to N other target creatures." "Support N" on an instant or sorcery spell
//! > means "Put a +1/+1 counter on each of up to N target creatures."
//!
//! THE AXIS IS PERMANENT-VS-SPELL, exactly as the rule states, and `is_other` is
//! derived from the source's printed card types rather than from the enclosing
//! parse grammar.
//!
//! The three non-creature permanents that print support — Blitzball Stadium
//! (Artifact), Captured by Lagacs (Enchantment — Aura), Together Forever
//! (Enchantment) — show reminder text reading "up to N target creatures" with no
//! "other", which looks like a counterexample and is not one. CR 207.2a:
//! reminder text "summarizes a rule" and has no game function. The summary is
//! accurate, because "other" excludes exactly one object — the source
//! (`FilterProp::Another`) — and a non-creature permanent is never a legal
//! "target creature" in the first place. The elided clause is vacuous, not
//! absent. Keying on the permanent axis is therefore behaviourally identical on
//! every printed card AND remains correct if such a permanent is animated, which
//! a creature-typed axis would not be.
//!
//! So the LIVE half of the source-type defect is the other direction: an
//! activated support carries no parse subject, so the previous
//! `ctx.subject.is_some()` derivation dropped "other" from Joraga Auxiliary's
//! "{4}{G}{W}: Support 2." and let a creature support itself. That one is pinned
//! at parse level by
//! `parse_support_on_subjectless_creature_ability_still_excludes_self`.
//!
//! CORPUS, and the limit of this commit's claim: 20 distinct cards print
//! `support N` — 11 creatures, 3 non-creature permanents, 6 instants/sorceries.
//! This commit corrects the support clause on all of them. It does NOT fix
//! **Sol, Advocate Eternal** ("support 4 and investigate four times"), which has
//! two further, PRE-EXISTING defects outside this diff's scope: the
//! `investigate four times` conjunct is dropped entirely, and its multiplier
//! appears to leak onto the support clause as `repeat_for: 4`. Both are
//! false-green (no `Unimplemented` marker). Deferred deliberately, not missed.
//!
//! Every card below is staged from its verbatim Scryfall Oracle text.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Creature, {2}{G}. Prints "other" in its reminder text.
const GENEROUS_PATRON_ORACLE: &str = "When this creature enters, support 2. (Put a +1/+1 counter on each of up to two other target creatures.)\nWhenever you put one or more counters on a creature you don't control, draw a card.";

/// Enchantment, {W}{W}. Prints NO "other" — a permanent that is not a creature.
const TOGETHER_FOREVER_ORACLE: &str = "When this enchantment enters, support 2. (Put a +1/+1 counter on each of up to two target creatures.)";

/// Artifact, {X}{U}. `support X`, and no "other".
const BLITZBALL_STADIUM_ORACLE: &str = "When this artifact enters, support X. (Put a +1/+1 counter on each of up to X target creatures.)";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// The union of every slot's legal targets, in first-seen order.
fn offered_union(slots: &[TargetSelectionSlot]) -> Vec<TargetRef> {
    let mut union = Vec::new();
    for target in slots.iter().flat_map(|slot| slot.legal_targets.iter()) {
        if !union.contains(target) {
            union.push(target.clone());
        }
    }
    union
}

/// Cast `source` through the real pipeline and stop at its ETB's stack-time
/// target prompt (CR 603.3d), returning the offered slots.
///
/// Panics with the observed `WaitingFor` sequence when the prompt never appears
/// — which is exactly the shape of the reported defect, so the panic message is
/// the diagnostic rather than a bare unwrap.
fn cast_to_etb_target_prompt(
    runner: &mut GameRunner,
    source: ObjectId,
) -> Vec<TargetSelectionSlot> {
    runner.cast(source).commit();
    let mut observed = Vec::new();
    for _ in 0..40 {
        let waiting = runner.state().waiting_for.clone();
        observed.push(format!("{waiting:?}").chars().take(96).collect::<String>());
        match waiting {
            WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                ..
            } => {
                assert_eq!(player, P0, "the source's controller chooses ETB targets");
                assert_eq!(
                    source_id,
                    Some(source),
                    "the prompt must belong to this source's ETB"
                );
                return target_slots;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => panic!(
                "no TriggerTargetSelection: the support ETB resolved without a stack-time \
                 target prompt; observed WaitingFor sequence {observed:#?}"
            ),
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance resolution");
            }
            other => panic!(
                "unexpected WaitingFor before the support ETB target prompt: {other:?}; \
                 observed sequence {observed:#?}"
            ),
        }
    }
    panic!("the cast did not reach a target prompt in 40 steps: {observed:#?}");
}

fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance resolution");
    }
    panic!("stack did not drain in 40 steps");
}

/// CR 701.41a + CR 601.2c: `support 2` announces UP TO TWO targets, so its ETB
/// must surface two slots and put a counter on each of the two declared
/// creatures.
///
/// This is the reported defect: the prompt offered a single target. The cause
/// was not the count — `multi_target` already carried max = 2 — but
/// `target_choice_timing`. `lower::target_choice_timing_for_clause` classifies a
/// `PutCounter` as an untargeted resolution-time pick when its printed fragment
/// lacks the literal word "target" (CR 115.10a), and the printed fragment of a
/// keyword action is the shorthand "support 2", which has no "target" in it. The
/// clause was stamped `Resolution`, and `ability_utils::collect_target_slots_inner`
/// builds multi-target slots only under `Stack` — so the whole two-slot
/// expansion was skipped and the effect fell back to a single recipient.
///
/// REVERT-TO-RED: drop the `declared_target_choice_timing` stamp in
/// `imperative.rs`'s `"support"` arm and this fails at
/// `cast_to_etb_target_prompt` with "no TriggerTargetSelection", because the ETB
/// resolves with no stack-time prompt at all.
#[test]
fn support_two_offers_two_target_slots_and_counters_both_declared_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario.add_creature(P0, "Ally Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let patron = scenario
        .add_creature_to_hand_from_oracle(P0, "Generous Patron", 1, 3, GENEROUS_PATRON_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Green));
    // CR 104.3c: the Patron's OWN second ability ("Whenever you put one or more
    // counters on a creature you don't control, draw a card") fires off the
    // support counter placed on the opponent's creature. Stock P0's library so
    // that draw is legal — on an empty library P0 loses to the draw-from-empty
    // state-based action mid-resolution and the second counter never lands,
    // which reads exactly like a support bug and is not one.
    for i in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);

    let slots = cast_to_etb_target_prompt(&mut runner, patron);
    assert_eq!(
        slots.len(),
        2,
        "CR 701.41a: `support 2` must surface two target slots, got {}",
        slots.len()
    );
    assert!(
        slots.iter().all(|slot| slot.optional),
        "CR 701.41a: \"up to two\" means min 0, so every slot is optional"
    );

    let offered = offered_union(&slots);
    // Reach guard for the exclusion assertion below: both other creatures ARE
    // offered, so "the Patron is absent" is a real exclusion rather than a
    // vacuous pass on an empty offered set.
    for (id, name) in [(ally, "ally"), (theirs, "opposing")] {
        assert!(
            offered.contains(&TargetRef::Object(id)),
            "reach: {name} must be offered; offered union {offered:?}"
        );
    }
    // CR 701.41a: the source IS a creature, so its own expansion says "other".
    assert!(
        !offered.contains(&TargetRef::Object(patron)),
        "CR 701.41a: a creature source's support must not offer itself; offered {offered:?}"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(ally), TargetRef::Object(theirs)],
        })
        .expect("declaring both creatures must be accepted");
    drain_stack(&mut runner);

    for (id, name) in [(ally, "ally"), (theirs, "opposing")] {
        assert_eq!(
            counters(&runner, id),
            1,
            "CR 701.41a: {name} was declared and must get exactly one +1/+1 counter"
        );
    }
    assert_eq!(
        counters(&runner, patron),
        0,
        "CR 701.41a: the creature source supports OTHER creatures, never itself"
    );
}

/// CR 701.41a: the LIVE half of the source-type defect, at runtime. Joraga
/// Auxiliary's support is an ACTIVATED ability, whose body carries no parse
/// subject — so the previous `ctx.subject.is_some()` derivation dropped "other"
/// and offered the Auxiliary itself as a legal target for its own support.
///
/// This is the direction that is NOT vacuous: unlike a non-creature permanent,
/// a creature source really is a legal "target creature", so the missing
/// exclusion changed the offered set.
///
/// REVERT-TO-RED: restore `is_other = ctx.subject.is_some()` and this fails on
/// the exclusion assertion with the Auxiliary present in the offered set.
#[test]
fn activated_support_on_a_creature_does_not_offer_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario.add_creature(P0, "Ally Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let auxiliary = scenario
        .add_creature_from_oracle(
            P0,
            "Joraga Auxiliary",
            2,
            3,
            "{4}{G}{W}: Support 2. (Put a +1/+1 counter on each of up to two other target creatures.)",
        )
        .id();
    scenario.with_mana_pool(P0, floating_mana(6, ManaType::Green));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);

    runner
        .act(GameAction::ActivateAbility {
            source_id: auxiliary,
            ability_index: 0,
        })
        .expect("activating the support ability must be accepted");
    let mut slots = None;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { target_slots, .. } => {
                slots = Some(target_slots);
                break;
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                slots = Some(target_slots);
                break;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("the activated support raised no target prompt")
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance resolution");
            }
            other => panic!("unexpected WaitingFor: {other:?}"),
        }
    }
    let slots = slots.expect("the activated support must raise a target prompt");
    let offered = offered_union(&slots);

    // Reach guard: both other creatures ARE offered, so the exclusion below is a
    // real absence rather than a vacuous pass on an empty offered set.
    for (id, name) in [(ally, "ally"), (theirs, "opposing")] {
        assert!(
            offered.contains(&TargetRef::Object(id)),
            "reach: {name} must be offered; offered union {offered:?}"
        );
    }
    assert!(
        !offered.contains(&TargetRef::Object(auxiliary)),
        "CR 701.41a: an activated support on a creature must not offer its own \
         source; offered {offered:?}"
    );
}

/// object — the source. On Together Forever, an Enchantment, that exclusion is
/// inert on an ordinary board, so this row pins the part that IS observable:
/// the non-creature permanent reaches the same two-slot stack-time announcement
/// the creature one does.
///
/// REVERT-TO-RED: this row is held by the defect-C timing stamp, not by the
/// source-type axis. The axis is pinned at parse level by
/// `parse_support_on_non_creature_permanent_source` and
/// `parse_support_on_spell_source`.
#[test]
fn support_two_on_a_non_creature_permanent_counters_both_declared_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario.add_creature(P0, "Ally Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let enchantment = scenario
        .add_spell_to_hand(P0, "Together Forever", false)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        // Applied AFTER `as_enchantment`: the parser reads the object's core
        // types to decide CR 701.41a's "other"/"any" branch, so the card must
        // already be an Enchantment when its text is parsed.
        .from_oracle_text(TOGETHER_FOREVER_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::White));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);

    let slots = cast_to_etb_target_prompt(&mut runner, enchantment);
    assert_eq!(
        slots.len(),
        2,
        "CR 701.41a: `support 2` on an enchantment must surface two target slots"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(ally), TargetRef::Object(theirs)],
        })
        .expect("declaring both creatures must be accepted");
    drain_stack(&mut runner);

    for (id, name) in [(ally, "ally"), (theirs, "opposing")] {
        assert_eq!(
            counters(&runner, id),
            1,
            "{name} was declared and must get exactly one +1/+1 counter"
        );
    }
}

/// CR 701.41a + CR 107.3a: `support X` takes the X announced for the spell that
/// produced the source. Blitzball Stadium costs {X}{U}, so X=2 must surface two
/// slots.
///
/// REVERT-TO-RED: restore `parse_number(...).unwrap_or(1)` in the `"support"`
/// arm and this fails on the slot-count assertion with one slot — "x" is not a
/// number, so the fallback silently capped every `support X` card at a single
/// target.
#[test]
fn support_x_uses_the_announced_x_for_its_target_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario.add_creature(P0, "Ally Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let stadium = scenario
        .add_artifact_to_hand_from_oracle(P0, "Blitzball Stadium", BLITZBALL_STADIUM_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Blue));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);

    runner.cast(stadium).x(2).commit();
    let mut slots = None;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                slots = Some(target_slots);
                break;
            }
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: 2 })
                    .expect("X announcement must be accepted");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("the Stadium's ETB resolved without a stack-time target prompt")
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance resolution");
            }
            other => panic!("unexpected WaitingFor: {other:?}"),
        }
    }
    let slots = slots.expect("the Stadium's ETB must raise a target prompt");
    assert_eq!(
        slots.len(),
        2,
        "CR 107.3a: `support X` with X=2 must surface two target slots, got {}",
        slots.len()
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(ally), TargetRef::Object(theirs)],
        })
        .expect("declaring both creatures must be accepted");
    drain_stack(&mut runner);

    for (id, name) in [(ally, "ally"), (theirs, "opposing")] {
        assert_eq!(
            counters(&runner, id),
            1,
            "{name} was declared and must get exactly one +1/+1 counter"
        );
    }
}
