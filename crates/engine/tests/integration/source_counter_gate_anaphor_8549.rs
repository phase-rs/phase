//! Regression pins for the source-counter-gate anaphor class.
//!
//! CR 608.2k: "If an ability's effect refers to a specific untargeted object
//! that has been previously referred to by that ability's cost or trigger
//! condition, it still affects that object even if the object has changed
//! characteristics." On a spell-cast trigger whose body carries a
//! source-counter gate ("if there are three or more depletion counters on this
//! enchantment, sacrifice IT"), the object previously referred to is the
//! ability's own source.
//!
//! CR 113.7: the source of a triggered ability is the object whose ability
//! triggered — the enchantment. CR 109.2b: a description using the word
//! "spell" means a spell on the stack. So binding that "it" to the CAST SPELL
//! makes the sacrifice and the transform name an object that is not the
//! permanent.
//!
//! Which of the two the parser picks is a parser heuristic, not a rule the CR
//! legislates; the CR numbers above establish what each candidate referent IS,
//! and the cards below establish which one the class needs.
//!
//! PR #8549 propagated the trigger-condition antecedent into every effect chunk
//! unconditionally. `resolve_it_pronoun` reads `object_pronoun_ref` BEFORE it
//! reaches `ctx.subject`, so that propagation outranked the nearer binding this
//! class establishes (`binds_source_counter_pronoun` sets
//! `chunk_subject = Some(SelfRef)`), and the PR's own coverage-parse-diff
//! recorded the damage on four cards:
//!
//!   ability/Sacrifice  target self          -> triggering source
//!     Decree of Silence, Charitable Levy
//!   ability/Transform  target parent target -> triggering source
//!     Thing in the Ice, The Emperor of Palamecia
//!
//! Decree of Silence and Charitable Levy stopped sacrificing themselves; Thing
//! in the Ice stopped transforming. All Oracle text below is verbatim from
//! Scryfall.

use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::ability_visit::visit_trigger;
use std::ops::ControlFlow;

/// Collect the `TargetFilter` of every `Sacrifice` / `Transform` effect the
/// card's triggers reach, at any chain depth. Walking the whole tree rather
/// than indexing a known position keeps the pin honest if the chain is
/// restructured — the assertion is about the BINDING, not the shape.
fn gated_targets(oracle: &str, name: &str) -> Vec<TargetFilter> {
    let parsed = parse_oracle_text(oracle, name, &[], &[], &[]);
    let mut found = Vec::new();
    for trigger in &parsed.triggers {
        let _ = visit_trigger(trigger, &mut |effect: &Effect| {
            match effect {
                Effect::Sacrifice { target, .. } | Effect::Transform { target, .. } => {
                    found.push(target.clone());
                }
                _ => {}
            }
            ControlFlow::Continue(())
        });
    }
    found
}

const DECREE_OF_SILENCE: &str = "Whenever an opponent casts a spell, counter that spell and put a depletion counter on this enchantment. If there are three or more depletion counters on this enchantment, sacrifice it.\n\
Cycling {4}{U}{U}\n\
When you cycle this card, you may counter target spell.";

const CHARITABLE_LEVY: &str = "Noncreature spells cost {1} more to cast.\n\
Whenever a player casts a noncreature spell, put a collection counter on this enchantment. Then if there are three or more collection counters on it, sacrifice it. If you do, draw a card, then you may search your library for a Plains card, put it onto the battlefield tapped, then shuffle.";

const THING_IN_THE_ICE: &str = "Defender\n\
This creature enters with four ice counters on it.\n\
Whenever you cast an instant or sorcery spell, remove an ice counter from this creature. Then if it has no ice counters on it, transform it.";

/// The regression itself, pinned to the EXACT binding the parse-diff recorded
/// before #8549 moved it — which differs per card, so a blanket "is not
/// TriggeringSource" would be weaker than the evidence available.
///
///   Decree of Silence / Charitable Levy : `self`         (the enchantment)
///   Thing in the Ice                    : `parent target` (the creature)
///
/// Each case carries a reach guard: a card that failed to parse at all would
/// make `gated_targets` return an empty vec, and a bare negative assertion
/// would then pass for the wrong reason.
#[test]
fn a_source_counter_gate_binds_its_anaphor_to_the_source_not_the_cast_spell() {
    for (name, oracle, expected) in [
        (
            "Decree of Silence",
            DECREE_OF_SILENCE,
            TargetFilter::SelfRef,
        ),
        ("Charitable Levy", CHARITABLE_LEVY, TargetFilter::SelfRef),
        (
            "Thing in the Ice",
            THING_IN_THE_ICE,
            TargetFilter::ParentTarget,
        ),
    ] {
        let targets = gated_targets(oracle, name);

        // REACH GUARD: the gated effect must actually be present, or every
        // assertion below is vacuous.
        assert!(
            !targets.is_empty(),
            "{name}: no Sacrifice/Transform effect reached — the assertions below \
             would be vacuous"
        );

        // The regression: `TriggeringSource` on a spell-cast trigger is the
        // cast spell on the stack (CR 109.2b), not the permanent (CR 113.7).
        assert!(
            !targets.contains(&TargetFilter::TriggeringSource),
            "{name}: the source-counter gate's bare 'it' must not bind to the cast \
             spell on the stack (CR 109.2b); regression from #8549, got {targets:?}"
        );

        // Positive, and EXHAUSTIVE: every gated target must be the binding the
        // parse-diff recorded before the regression. `contains` would pass while
        // the parser also emitted some other wrong filter beside the right one —
        // the negative above rules out exactly one variant, not all of them.
        //
        // Asserted as "every element equals" rather than `targets == [expected]`
        // deliberately: the chain walk can legitimately reach one effect from
        // two registration points, so a duplicate of the CORRECT binding is not
        // a defect, while any element that differs is.
        assert!(
            targets.iter().all(|t| *t == expected),
            "{name}: every gated anaphor must bind {expected:?}, got {targets:?}"
        );
    }
}
