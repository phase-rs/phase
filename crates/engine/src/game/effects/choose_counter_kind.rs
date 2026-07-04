//! CR 608.2d + CR 122.1: Interactive counter-kind selection.
//!
//! Resolves `Effect::ChooseCounterKind` ("choose a counter on it" — The Caves
//! of Androzani II/III). The distinct counter kinds present on the resolved
//! target object are enumerated at resolution time, then:
//!   * 0 kinds → no-op (CR 608.2d: a player can't choose an impossible option),
//!   * 1 kind  → auto-select (bind directly, no prompt),
//!   * 2+ kinds → an interactive `WaitingFor::NamedChoice` reusing the shared
//!     named-choice seam with a `ChoiceType::CounterKind` whose option list is
//!     baked with the concrete kinds.
//!
//! The chosen kind persists as `ChosenAttribute::Counter` on the source (via the
//! single `bind_named_choice` authority) so a following `Effect::PutChosenCounter`
//! can read it.

use crate::types::ability::{
    ChoiceType, ChosenAttribute, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::counter::{positive_counter_types, CounterType};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use std::collections::HashSet;

/// CR 608.2d + CR 122.1: Resolve `Effect::ChooseCounterKind`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(&ability.effect, Effect::ChooseCounterKind { .. }) {
        return Err(EffectError::MissingParam("ChooseCounterKind".to_string()));
    }

    // CR 608.2d: Each (per-`repeat_for`-iteration) choice starts fresh — clear any
    // counter kind chosen for a PREVIOUS object so that when THIS object has no
    // counters (the choice is skipped), the following `PutChosenCounter` reads
    // no chosen kind and no-ops, rather than inheriting a stale prior kind.
    if let Some(src) = state.objects.get_mut(&ability.source_id) {
        src.chosen_attributes
            .retain(|a| !matches!(a, ChosenAttribute::Counter(_)));
    }

    // CR 122.1: Enumerate the distinct counter kinds currently on the RESOLVED
    // target object(s). Both shapes bind the object into `ability.targets`: the
    // member-driven `repeat_for` loop binds the i-th permanent for a
    // `ParentTarget` head (The Caves of Androzani), and the targeting pipeline
    // binds the declared "target permanent" (Ichormoon Gauntlet). Reading the
    // bound targets — rather than re-matching a filter against the whole
    // battlefield — keeps the choice scoped to exactly that object.
    let target_ids: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    let mut seen: HashSet<CounterType> = HashSet::new();
    let mut kinds: Vec<CounterType> = Vec::new();
    for id in &target_ids {
        if let Some(obj) = state.objects.get(id) {
            for kind in positive_counter_types(&obj.counters) {
                if seen.insert(kind.clone()) {
                    kinds.push(kind);
                }
            }
        }
    }
    kinds.sort_by(|a, b| a.as_str().cmp(&b.as_str()));

    let resolved = || GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    };

    // CR 608.2d: no counters on the object → nothing to choose → no-op.
    if kinds.is_empty() {
        events.push(resolved());
        return Ok(());
    }

    let choice_type = ChoiceType::CounterKind {
        options: kinds.clone(),
    };

    // CR 608.2d: a single legal option is auto-selected — no interactive prompt.
    if kinds.len() == 1 {
        let only = kinds[0].as_str().into_owned();
        crate::game::effects::choose::bind_named_choice(
            state,
            &choice_type,
            &only,
            Some(ability.source_id),
        );
        events.push(resolved());
        return Ok(());
    }

    // CR 608.2d: two or more kinds → surface the shared interactive choice seam.
    let options: Vec<String> = kinds.iter().map(|k| k.as_str().into_owned()).collect();
    state.waiting_for = WaitingFor::NamedChoice {
        player: ability.controller,
        choice_type,
        options,
        source_id: Some(ability.source_id),
    };
    events.push(resolved());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{TargetFilter, TargetRef};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn ability_with_target(target_obj: ObjectId, source: ObjectId) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::ChooseCounterKind {
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(target_obj)],
            source,
            PlayerId(0),
        );
        ability.targets = vec![TargetRef::Object(target_obj)];
        ability
    }

    /// CR 608.2d: An object with two distinct counter kinds surfaces an
    /// interactive `NamedChoice` listing both kinds.
    #[test]
    fn two_kinds_prompt_named_choice() {
        let mut state = GameState::new_two_player(1);
        let obj = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&obj).unwrap();
            o.counters.insert(CounterType::Plus1Plus1, 1);
            o.counters.insert(CounterType::Stun, 1);
        }
        let ability = ability_with_target(obj, ObjectId(999));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                ..
            } => {
                assert!(matches!(choice_type, ChoiceType::CounterKind { .. }));
                assert_eq!(options.len(), 2);
            }
            other => panic!("expected NamedChoice, got {other:?}"),
        }
    }

    /// CR 608.2d: A single counter kind is auto-selected — no prompt, and the
    /// kind is persisted onto the source.
    #[test]
    fn single_kind_auto_selects_without_prompt() {
        let mut state = GameState::new_two_player(1);
        let source = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let obj = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 2);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let ability = ability_with_target(obj, source);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "a single kind must auto-select without prompting"
        );
        let attrs = &state.objects.get(&source).unwrap().chosen_attributes;
        assert!(attrs.iter().any(|a| matches!(
            a,
            crate::types::ability::ChosenAttribute::Counter(CounterType::Stun)
        )));
    }

    /// CR 608.2d: An object with no counters is skipped (no prompt, no bind).
    #[test]
    fn zero_kinds_is_noop() {
        let mut state = GameState::new_two_player(1);
        let obj = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let ability = ability_with_target(obj, ObjectId(999));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(!matches!(state.waiting_for, WaitingFor::NamedChoice { .. }));
    }

    /// CR 714.2 + CR 608.2d + CR 122.1 + CR 122.6: Runtime proof of the composed
    /// The Caves of Androzani chapter — the real parsed `repeat_for` +
    /// `ChooseCounterKind` (auto-select) + optional `PutChosenCounter` chain,
    /// re-hosted as an activated ability and driven through the production
    /// pipeline. Asserts the single-kind object receives exactly one ADDITIONAL
    /// counter of the chosen kind (Stun: 1 → 2), proving the choose→persist→put
    /// round-trip resolves correctly inside a member-driven iteration.
    #[test]
    fn caves_chapter_repeat_choose_and_put_round_trip() {
        use crate::game::scenario::GameScenario;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);

        let mut activated = parse_effect_chain(
            "For each non-Saga permanent, choose a counter on it. You may put an \
             additional counter of that kind on that permanent.",
            AbilityKind::Spell,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // The Saga subtype excludes the host from the non-Saga repeat filter.
        let host = {
            let mut b = scenario.add_creature(P0, "Caves Host", 0, 1);
            b.with_subtypes(vec!["Saga"]);
            b.with_ability_definition(activated);
            b.id()
        };
        let single = scenario.add_creature(P0, "Single", 2, 2).id();
        scenario.with_counter(single, CounterType::Stun, 1);

        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the Caves chapter ability");

        for _ in 0..80 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept the optional put");
                }
                WaitingFor::NamedChoice { options, .. } => {
                    let choice = options.first().cloned().expect("a counter-kind option");
                    runner
                        .act(GameAction::ChooseOption { choice })
                        .expect("answer the counter-kind choice");
                }
                _ => break,
            }
        }

        let stun = runner
            .state()
            .objects
            .get(&single)
            .unwrap()
            .counters
            .get(&CounterType::Stun)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            stun, 2,
            "one additional counter of the chosen (Stun) kind must be added"
        );
    }

    /// CR 714.2 + CR 608.2d + CR 122.1 + CR 122.6: The genuinely risky mid-repeat
    /// continuation — a permanent bearing TWO distinct counter kinds suspends the
    /// `repeat_for` loop on an interactive `NamedChoice`, resumes on
    /// `GameAction::ChooseOption`, re-binds `ParentTarget` to the SAME iteration's
    /// object for the optional `PutChosenCounter`, and then advances to the next
    /// member (a single-kind permanent driven by auto-select). Proves the loop
    /// does not lose its place across an interactive counter-kind choice.
    #[test]
    fn caves_chapter_two_counter_kinds_suspends_resumes_and_advances() {
        use crate::game::scenario::GameScenario;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);

        let mut activated = parse_effect_chain(
            "For each non-Saga permanent, choose a counter on it. You may put an \
             additional counter of that kind on that permanent.",
            AbilityKind::Spell,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // The Saga subtype excludes the host from the non-Saga repeat filter.
        let host = {
            let mut b = scenario.add_creature(P0, "Caves Host", 0, 1);
            b.with_subtypes(vec!["Saga"]);
            b.with_ability_definition(activated);
            b.id()
        };
        // Two distinct counter kinds → the interactive 2+-kind NamedChoice branch.
        let two_kinds = scenario.add_creature(P0, "Two Kinds", 2, 2).id();
        scenario.with_counter(two_kinds, CounterType::Stun, 1);
        scenario.with_counter(two_kinds, CounterType::Plus1Plus1, 1);
        // A second member with a single kind → auto-select; proves the loop
        // advances to (and resolves) the next member after the suspend/resume.
        let single = scenario.add_creature(P0, "Single", 3, 3).id();
        scenario.with_counter(single, CounterType::Loyalty, 1);

        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the Caves chapter ability");

        let mut chosen_kind: Option<String> = None;
        for _ in 0..80 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept the optional put");
                }
                WaitingFor::NamedChoice { options, .. } => {
                    assert_eq!(
                        options.len(),
                        2,
                        "the two-kind permanent must surface both distinct kinds"
                    );
                    let choice = options.first().cloned().expect("a counter-kind option");
                    chosen_kind = Some(choice.clone());
                    runner
                        .act(GameAction::ChooseOption { choice })
                        .expect("answer the counter-kind choice");
                }
                _ => break,
            }
        }

        let chosen = chosen_kind
            .expect("the two-kind permanent must have suspended the loop on a NamedChoice");

        // The chosen kind receives exactly one additional counter; the other kind
        // on the same permanent is untouched (per-iteration isolation, CR 608.2d).
        let two = &runner.state().objects.get(&two_kinds).unwrap().counters;
        let stun = two.get(&CounterType::Stun).copied().unwrap_or(0);
        let p1p1 = two.get(&CounterType::Plus1Plus1).copied().unwrap_or(0);
        assert_eq!(
            stun + p1p1,
            3,
            "exactly one additional counter must land on the two-kind permanent"
        );
        if chosen == CounterType::Stun.as_str() {
            assert_eq!(stun, 2, "the chosen (Stun) kind gains one counter");
            assert_eq!(p1p1, 1, "the unchosen (+1/+1) kind is untouched");
        } else {
            assert_eq!(p1p1, 2, "the chosen (+1/+1) kind gains one counter");
            assert_eq!(stun, 1, "the unchosen (Stun) kind is untouched");
        }

        // The loop advanced to the next member: its single (auto-selected) kind
        // gains its additional counter too.
        let loyalty = runner
            .state()
            .objects
            .get(&single)
            .unwrap()
            .counters
            .get(&CounterType::Loyalty)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            loyalty, 2,
            "the loop must resume past the interactive choice and resolve the next member"
        );
    }

    /// CR 608.2d + CR 122.6: A counterless permanent in the `repeat_for` (a land,
    /// a fresh creature) has no "that kind" to add, so the per-iteration optional
    /// "you may put an additional counter" is an impossible option and must NOT be
    /// offered — the effect resolves as its defined no-op with no yes/no prompt.
    #[test]
    fn caves_chapter_counterless_permanent_raises_no_impossible_optional() {
        use crate::game::scenario::GameScenario;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);

        let mut activated = parse_effect_chain(
            "For each non-Saga permanent, choose a counter on it. You may put an \
             additional counter of that kind on that permanent.",
            AbilityKind::Spell,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let host = {
            let mut b = scenario.add_creature(P0, "Caves Host", 0, 1);
            b.with_subtypes(vec!["Saga"]);
            b.with_ability_definition(activated);
            b.id()
        };
        // No counters → 0-kind branch fires → PutChosenCounter can only no-op.
        let bare = scenario.add_creature(P0, "Counterless", 2, 2).id();

        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the Caves chapter ability");

        for _ in 0..40 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    panic!(
                        "a counterless permanent must not raise an impossible \
                         'you may put an additional counter' prompt (CR 608.2d)"
                    );
                }
                WaitingFor::NamedChoice { .. } => {
                    panic!("a counterless permanent has no counter kinds to choose");
                }
                _ => break,
            }
        }

        assert!(
            runner
                .state()
                .objects
                .get(&bare)
                .unwrap()
                .counters
                .is_empty(),
            "no counter is added to a counterless permanent"
        );
    }
}
