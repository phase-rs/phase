//! CR 603.12 + CR 701.20a: a "When you reveal a <filter> card this way"
//! reflexive is lowered (`WhenYouDo` + the reveal-until-hit guard) only when the
//! reveal-until before it is proven to be the resolution's only instruction and
//! to execute exactly once. The guard reads that one reveal's own verdict; with
//! a second reveal or a repeated process, the antecedent and its occurrence set
//! are not proven by this lowering, so the head stays explicitly unsupported
//! (issue #9763): a named gap that resolves as nothing.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Effect, EffectOutcomeSignal, QuantityExpr,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HEAD: &str = "Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest on the bottom of your library in a random order. ";
const GATE: &str = "When you reveal a creature card this way, you gain 3 life.";
/// The gate with an extra intervening-if guard composed onto the root `And`.
const GUARDED_GATE: &str =
    "When you reveal a creature card this way, if it has mana value 2 or greater, you gain 3 life.";
/// A reflexive body that, accepted, would continue the reveal-until itself.
const CONTINUATION_GATE: &str =
    "When you reveal a creature card this way, put that card onto the battlefield.";
const REPEAT_ONCE: &str = "Repeat this process one more time. ";
const EARLIER: &str = "You gain 1 life. ";

/// The issue's board: two reveal-untils before the head.
const DOUBLE_DIG: &str = "Reveal cards from the top of your library until you reveal a land card. Put that card into your hand and the rest on the bottom of your library in a random order. Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest on the bottom of your library in a random order. When you reveal a creature card this way, you gain 3 life.";

/// Two Grizzly Bears (mana value 2) on top of P0's library; P0 at 20 life.
fn board(oracle: &str) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe Dig", true, oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    let library = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_library_top(P0, "Grizzly Bears", false)
                .as_creature()
                .with_mana_cost(ManaCost::generic(2))
                .id()
        })
        .collect();
    let runner = scenario.build();
    assert_eq!(runner.life(P0), 20, "reach guard: starting life");
    (runner, spell, library)
}

fn root(runner: &GameRunner, spell: ObjectId) -> AbilityDefinition {
    let abilities = &runner.state().objects[&spell].abilities;
    assert_eq!(abilities.len(), 1, "{abilities:?}");
    abilities[0].clone()
}

fn chain(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![def];
    while let Some(next) = nodes.last().and_then(|node| node.sub_ability.as_deref()) {
        nodes.push(next);
    }
    nodes
}

fn reveal_untils(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    chain(def)
        .into_iter()
        .filter(|node| matches!(node.effect.as_ref(), Effect::RevealUntil { .. }))
        .collect()
}

fn has_reveal_gate_gap(def: &AbilityDefinition) -> bool {
    chain(def).into_iter().any(|node| {
        matches!(
            node.effect.as_ref(),
            Effect::Unimplemented { name, .. } if name == "reveal_this_way_reflexive_gate"
        )
    })
}

fn is_reveal_guard(condition: &AbilityCondition) -> bool {
    matches!(
        condition,
        AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::RevealUntilMatched,
        }
    )
}

/// The root condition of the guarded reflexive node, if any: the guard alone
/// or as a member of the flat root `And`.
fn guarded_condition(def: &AbilityDefinition) -> Option<&AbilityCondition> {
    chain(def)
        .into_iter()
        .filter_map(|node| node.condition.as_ref())
        .find(|condition| match condition {
            AbilityCondition::And { conditions } => conditions.iter().any(is_reveal_guard),
            condition => is_reveal_guard(condition),
        })
}

/// Cast the spell and pass priority until the stack is empty. Returns how many
/// of `library` reached the hand.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId, library: &[ObjectId]) -> usize {
    runner.cast(spell).commit();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::Priority { .. } => {
                return library
                    .iter()
                    .filter(|id| runner.state().objects[*id].zone == Zone::Hand)
                    .count();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

/// The fail-closed contract at runtime: the reveal is still delivered, and the
/// unsupported reflexive gains no life.
fn assert_resolves_without_the_reflexive(text: &str, life_without_reflexive: i32) {
    let (mut runner, spell, library) = board(text);
    let def = root(&runner, spell);
    let in_hand = cast_and_resolve(&mut runner, spell, &library);
    assert!(
        in_hand >= 1,
        "reach guard: the reveal delivered a Grizzly Bears to hand: {text}"
    );
    assert_eq!(
        runner.life(P0),
        life_without_reflexive,
        "the unsupported reflexive must not fire: {text}"
    );
    assert!(has_reveal_gate_gap(&def), "named gap: {def:?}");
    assert!(guarded_condition(&def).is_none(), "{def:?}");
}

/// Two reveal-untils before the head: the guard could read only the second's
/// verdict, while "this way" may name what the first revealed too.
#[test]
fn a_second_reveal_until_before_the_head_fails_closed() {
    let (runner, spell, _) = board(DOUBLE_DIG);
    let def = root(&runner, spell);
    assert_eq!(
        reveal_untils(&def).len(),
        2,
        "reach guard: both reveal-untils parsed: {def:?}"
    );
    assert_resolves_without_the_reflexive(DOUBLE_DIG, 20);
}

/// Probe P1: "Repeat this process one more time" before the head stamps a
/// total count of 2 on the one reveal-until clause without emitting another
/// clause — the reveal runs twice, and the guard would see only the last
/// verdict.
#[test]
fn a_repeated_reveal_before_the_head_fails_closed() {
    let text = format!("{HEAD}{REPEAT_ONCE}{GATE}");
    let (runner, spell, _) = board(&text);
    let def = root(&runner, spell);
    let reveals = reveal_untils(&def);
    assert_eq!(reveals.len(), 1, "reach guard: one reveal-until: {def:?}");
    assert!(
        matches!(
            reveals[0].effect.as_ref(),
            Effect::RevealUntil {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ),
        "reach guard: count 1: {def:?}"
    );
    assert!(
        matches!(
            reveals[0].repeat_for,
            Some(QuantityExpr::Offset { offset: 1, .. })
        ),
        "reach guard: the process runs once more (1 + 1): {def:?}"
    );
    assert_resolves_without_the_reflexive(&text, 20);
}

/// A repeat directive printed after the head repeats the whole process,
/// reveal included; the gate is already lowered when the directive is read,
/// so the proof runs over the finished chain.
#[test]
fn a_repeat_directive_after_the_head_fails_closed() {
    for directive in ["Repeat this process one more time.", "Repeat this process."] {
        let text = format!("{HEAD}{GATE} {directive}");
        let (runner, spell, _) = board(&text);
        let def = root(&runner, spell);
        assert_eq!(
            reveal_untils(&def).len(),
            1,
            "{directive}: reach guard: one reveal-until: {def:?}"
        );
        assert_resolves_without_the_reflexive(&text, 20);
    }
}

/// An extra intervening-if guard composed onto the gate's root `And` cannot
/// hide it from the proof, after a repeat or after an earlier instruction.
#[test]
fn an_extra_guard_on_the_gate_still_fails_closed() {
    for (text, life) in [
        (format!("{HEAD}{REPEAT_ONCE}{GUARDED_GATE}"), 20),
        (format!("{EARLIER}{HEAD}{GUARDED_GATE}"), 21),
    ] {
        assert_resolves_without_the_reflexive(&text, life);
    }
}

/// Control for the extra-guard case: accepted, the extra guard rides on the
/// gate beside the reveal-until-hit guard. (Shape only: how that guard's "it"
/// binds at resolution is outside this lowering.)
#[test]
fn an_extra_guard_on_an_accepted_gate_is_kept() {
    let (runner, spell, _) = board(&format!("{HEAD}{GUARDED_GATE}"));
    let def = root(&runner, spell);
    let Some(AbilityCondition::And { conditions }) = guarded_condition(&def) else {
        panic!("the accepted gate is a flat root And: {def:?}");
    };
    assert_eq!(
        conditions.len(),
        3,
        "reach guard: WhenYouDo + reveal guard + the extra guard: {def:?}"
    );
    assert!(!has_reveal_gate_gap(&def), "{def:?}");
}

/// A rejected reflexive whose body would continue the reveal-until ("put that
/// card onto the battlefield") becomes a whole gap: it neither keeps its
/// continuation nor patches the reveal's destination.
#[test]
fn a_rejected_continuation_body_survives_assembly_as_the_gap() {
    let text = format!("{HEAD}{REPEAT_ONCE}{CONTINUATION_GATE}");
    let (mut runner, spell, library) = board(&text);
    let def = root(&runner, spell);
    assert!(has_reveal_gate_gap(&def), "named gap: {def:?}");
    let reveals = reveal_untils(&def);
    assert_eq!(reveals.len(), 1, "reach guard: one reveal-until: {def:?}");
    assert!(
        matches!(
            reveals[0].effect.as_ref(),
            Effect::RevealUntil {
                kept_destination: Zone::Hand,
                ..
            }
        ),
        "the reveal keeps its printed destination: {def:?}"
    );
    let in_hand = cast_and_resolve(&mut runner, spell, &library);
    assert!(in_hand >= 1, "reach guard: the reveal delivered to hand");
    assert!(
        library
            .iter()
            .all(|id| runner.state().objects[id].zone != Zone::Battlefield),
        "the rejected body moved nothing onto the battlefield"
    );
}

/// Control for P1: the same text without the repeat still lowers to the
/// guarded reflexive and resolves (CR 603.12).
#[test]
fn the_single_reveal_still_lowers_and_resolves() {
    let (mut runner, spell, library) = board(&format!("{HEAD}{GATE}"));
    let def = root(&runner, spell);
    assert!(guarded_condition(&def).is_some(), "{def:?}");
    assert!(!has_reveal_gate_gap(&def), "{def:?}");
    assert_eq!(
        cast_and_resolve(&mut runner, spell, &library),
        1,
        "reach guard: the reveal hit"
    );
    assert_eq!(runner.life(P0), 23);
}
