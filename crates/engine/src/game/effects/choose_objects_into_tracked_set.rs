//! CR 608.2c: Interactive battlefield-object selection into the chain's
//! tracked object set.
//!
//! Resolves `Effect::ChooseObjectsIntoTrackedSet`. The `chooser` field is a
//! `TargetFilter` resolved per-instance (like `Effect::PayCost.payer`) so an
//! "at the beginning of each player's upkeep" trigger prompts the player whose
//! upkeep it is — not a fixed controller. The chosen objects are written into
//! a fresh tracked set so downstream effects ("pay {N} for each ... chosen
//! this way", "untap those creatures") resolve against the exact selection.

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::targeting::resolve_effect_player_ref;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 608.2c: Resolve `Effect::ChooseObjectsIntoTrackedSet` — surface a
/// `WaitingFor::ChooseObjectsSelection` prompt for the affected player.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (chooser_filter, filter) = match &ability.effect {
        Effect::ChooseObjectsIntoTrackedSet {
            chooser, filter, ..
        } => (chooser.clone(), filter.clone()),
        _ => {
            return Err(EffectError::MissingParam(
                "ChooseObjectsIntoTrackedSet".to_string(),
            ))
        }
    };

    // CR 608.2c: Resolve the chooser to the affected player — the same
    // single-authority player-ref resolver used by `PayCost.payer`. For an
    // "each player's upkeep" trigger this is the upkeep player.
    let Some(chooser) = resolve_effect_player_ref(state, ability, &chooser_filter) else {
        // No resolvable chooser — nothing to select; resolve as a no-op.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };

    // Evaluate `filter` against the chooser's battlefield permanents. The
    // filter's "they control" controller constraint resolves against the
    // ability controller, so bind the filter context controller to the
    // chooser (mirrors `pay.rs`'s payer-rebinding pattern).
    let ctx = FilterContext::from_ability_with_controller(ability, chooser);
    let eligible: Vec<TargetRef> = state
        .battlefield
        .iter()
        .filter(|&&obj_id| matches_target_filter(state, obj_id, &filter, &ctx))
        .map(|&obj_id| TargetRef::Object(obj_id))
        .collect();

    // CR 608.2c: Surface the interactive selection. Even with an empty
    // `eligible` set the prompt is raised — the player's act of submitting an
    // empty selection IS a legal resolution-time choice (CR 608.2d: choosing
    // zero of an "up to N" selection while applying the effect), and the
    // downstream `ScaledMana { times: 0 }` payment is a no-op {0}-cost SUCCESS
    // (CR 118.5).
    // CR 608.2: carry the triggering event across the interactive selection
    // pause so the stashed `PayCost { payer: TriggeringPlayer }` continuation
    // resolves the payer correctly. PART 1 has already restored
    // `current_trigger_event`, so this clone captures the real event.
    state.waiting_for = WaitingFor::ChooseObjectsSelection {
        player: chooser,
        eligible,
        trigger_event: state.current_trigger_event.clone(),
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::game::scenario::GameScenario;
    use crate::parser::oracle_effect::parse_effect_chain;
    use crate::types::ability::AbilityKind;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    /// CR 608.2c + CR 608.2d + official ruling: The Day of the Doctor IV — "Choose
    /// up to three Doctors. You may exile all other creatures." — when the
    /// controller chooses ZERO Doctors (a legal resolution-time choice for an
    /// "up to N" selection, CR 608.2d), "all other creatures" is EVERY creature
    /// on the battlefield (the chosen set is empty, so nothing is excluded).
    ///
    /// This is the end-to-end runtime proof the `game/filter.rs` unit test
    /// (`not_in_tracked_set_excludes_chosen_and_includes_rest`) cannot give: that
    /// submitting an EMPTY `WaitingFor::ChooseObjectsSelection` really drives
    /// `publish_fresh_tracked_set(state, [])`, which allocates a fresh EMPTY set
    /// and rebinds `chain_tracked_set_id` to it, so the following
    /// `ChangeZoneAll { target: creatures with Not(InTrackedSet(sentinel)) }`
    /// resolves the sentinel to that empty set and exiles ALL creatures — the
    /// controller's, the opponent's, and the (would-be-chosen) Doctors alike.
    #[test]
    fn choosing_zero_doctors_exiles_all_creatures() {
        // Chapter IV, parsed as the real card text and re-hosted as an ACTIVATED
        // ability so the test can fire it on demand. Parse produces:
        //   head:  ChooseObjectsIntoTrackedSet { filter: Doctor, min: 0, max: 3 }
        //   sub0:  ChangeZoneAll -> Exile, target = creatures Not(InTrackedSet(0))
        //          (optional: the "You may exile all other creatures")
        //   sub1:  DealDamage 13 to controller ("If you do, ... 13 damage to you")
        let mut activated = parse_effect_chain(
            "Choose up to three Doctors. You may exile all other creatures. \
             If you do, this Saga deals 13 damage to you.",
            AbilityKind::Activated,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // The activation vehicle for the chapter ability. Typed as a creature
        // purely so it can host an activated ability on the battlefield; it stands
        // in for the (non-creature) Saga. Being a creature, it is itself one of the
        // "other creatures" and is exiled too — which does not weaken the "every
        // creature is exiled" assertion.
        let host = {
            let mut b = scenario.add_creature(P0, "The Day of the Doctor", 0, 1);
            b.with_ability_definition(activated);
            b.id()
        };
        // Two Doctors the controller COULD choose but deliberately does not.
        let doctor_a = {
            let mut b = scenario.add_creature(P0, "First Doctor", 2, 2);
            b.with_subtypes(vec!["Doctor"]);
            b.id()
        };
        let doctor_b = {
            let mut b = scenario.add_creature(P0, "Thirteenth Doctor", 3, 3);
            b.with_subtypes(vec!["Doctor"]);
            b.id()
        };
        // A non-Doctor creature under the OPPONENT — proves the mass exile has no
        // controller constraint (all creatures, not just yours).
        let enemy = scenario.add_creature(P1, "Enemy Dalek", 4, 4).id();

        let mut runner = scenario.build();
        let life_before = runner.life(P0);

        // Fire the chapter ability; it resolves to the interactive selection.
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the chapter IV ability");
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.waiting_for_kind(),
            "ChooseObjectsSelection",
            "the head must pause on the interactive Doctor selection"
        );

        // Choose ZERO Doctors: submit an empty selection (CR 608.2d — choosing
        // zero of an "up to N" selection is a legal resolution-time choice). The
        // eligible set contains both Doctors, but the controller picks none.
        if let WaitingFor::ChooseObjectsSelection { eligible, .. } =
            runner.state().waiting_for.clone()
        {
            assert!(
                eligible.len() >= 2,
                "both Doctors must be eligible to be chosen, got {eligible:?}"
            );
        } else {
            panic!("expected ChooseObjectsSelection");
        }
        runner
            .act(GameAction::SelectTargets { targets: vec![] })
            .expect("submit an empty (zero-Doctor) selection");

        // The "You may exile all other creatures" clause now offers the mass
        // exile; accept it.
        assert_eq!(
            runner.waiting_for_kind(),
            "OptionalEffectChoice",
            "the optional 'You may exile all other creatures' clause must be offered"
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept the mass exile");
        runner.advance_until_stack_empty();

        // With an empty chosen set, EVERY creature is "other" and is exiled.
        let zone_of = |id: ObjectId| runner.state().objects[&id].zone;
        for (id, name) in [
            (host, "host"),
            (doctor_a, "First Doctor"),
            (doctor_b, "Thirteenth Doctor"),
            (enemy, "Enemy Dalek"),
        ] {
            assert_eq!(
                zone_of(id),
                Zone::Exile,
                "{name} must be exiled — zero Doctors chosen means none are excluded"
            );
        }
        let creatures_left = runner
            .state()
            .battlefield
            .iter()
            .filter(|&&id| {
                runner.state().objects[&id]
                    .card_types
                    .core_types
                    .contains(&CoreType::Creature)
            })
            .count();
        assert_eq!(
            creatures_left, 0,
            "no creature may remain on the battlefield after exiling all"
        );

        // "If you do, this Saga deals 13 damage to you" — the tail confirms the
        // continuation drained through the whole chapter chain.
        assert_eq!(
            runner.life(P0),
            life_before - 13,
            "the exile-all branch's 13-damage rider must have resolved"
        );
    }
}
