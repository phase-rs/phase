use crate::game::filter::{matches_target_filter, FilterContext};
use crate::types::ability::{
    AbilityKind, ContinuousModification, Duration, Effect, EffectError, EffectKind,
    ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 113.1a + CR 113.10 + CR 611.2 + CR 611.2c + CR 613.1f: Grant the
/// recipient(s) all activated abilities of the targeted donor object, for a
/// duration. This is the resolution-time grant for Quicksilver Elemental
/// ("This creature gains all activated abilities of target creature until end
/// of turn") and Grell Philosopher ("each Horror you control gains all
/// activated abilities of target artifact an opponent controls until end of
/// turn").
///
/// CR 611.2c: the affected set and the granted-ability snapshot are fixed at
/// the moment the effect begins. The donor's activated abilities are
/// snapshotted ONCE, here, into concrete `ContinuousModification::GrantAbility`
/// instances — the resolver never constructs
/// `ContinuousModification::GrantAllActivatedAbilitiesOf` (the static-side
/// meta-modification re-scanned every layer pass). A donor that later gains a
/// new activated ability does NOT retroactively extend the grant.
///
/// CR 201.5b: a granted ability that references the donor's name (`~` /
/// `SelfRef` in its cost/effect — e.g. "Sacrifice ~") is reinterpreted to use
/// the recipient's identity, because the granted `GrantAbility` lands in the
/// recipient's `obj.abilities`; when activated, its `source_id` is the
/// recipient, so the self-reference resolves to the recipient (Quicksilver),
/// not the donor.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (recipient, duration) = match &ability.effect {
        Effect::GainActivatedAbilitiesOfTarget {
            recipient,
            duration,
            ..
        } => (
            recipient.clone(),
            duration
                .clone()
                .or(ability.duration.clone())
                .unwrap_or(Duration::UntilEndOfTurn),
        ),
        _ => (
            TargetFilter::SelfRef,
            ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn),
        ),
    };

    // The donor is the single declared object target (BecomeCopy-style).
    let donor_id = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .ok_or_else(|| {
            EffectError::MissingParam(
                "GainActivatedAbilitiesOfTarget requires a target".to_string(),
            )
        })?;

    // CR 611.2c: snapshot the donor's activated abilities exactly once, now.
    // Read the donor's CURRENT abilities (post-layer) so abilities the donor
    // itself has gained by this point are included, but never re-read after
    // this point — the resulting `GrantAbility` set is frozen.
    let modifications: Vec<ContinuousModification> = state
        .objects
        .get(&donor_id)
        .map(|donor| {
            donor
                .abilities
                .iter()
                .filter(|a| a.kind == AbilityKind::Activated)
                .map(|a| ContinuousModification::GrantAbility {
                    definition: Box::new(a.clone()),
                })
                .collect()
        })
        .unwrap_or_default();

    // CR 611.2b: a grant that affects zero abilities (donor has none) is not an
    // error — it resolves cleanly with no continuous effect registered.
    if !modifications.is_empty() {
        match &recipient {
            // Quicksilver Elemental: the recipient is the source itself.
            TargetFilter::SelfRef => {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration,
                    TargetFilter::SpecificObject {
                        id: ability.source_id,
                    },
                    modifications,
                    None,
                );
            }
            // Grell Philosopher: "each Horror you control" — resolve the
            // recipient filter against the battlefield at resolution time and
            // register one independent transient effect per matching object so
            // multiple recipients (or multiple sequential grants) never collide
            // or overwrite each other (CR 611.2c fixes each affected set
            // independently).
            //
            // CR 112.6a: "you control" must resolve against the resolving
            // ability's controller, not whatever player currently controls the
            // live source object. `FilterContext::from_ability` binds
            // `source_controller` to `ability.controller` (captured when the
            // ability was put on the stack), so the grant still lands on the
            // correct player's Horrors even if the source's controller has
            // since changed or the source has left the battlefield entirely
            // (`FilterContext::from_source` would instead read the live
            // object's controller, or `None` if the source object no longer
            // exists in `state.objects` — silently matching the wrong
            // player, or no one).
            _ => {
                let ctx = FilterContext::from_ability(ability);
                let recipient_ids: Vec<_> = state
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|id| matches_target_filter(state, *id, &recipient, &ctx))
                    .collect();
                for recipient_id in recipient_ids {
                    state.add_transient_continuous_effect(
                        ability.source_id,
                        ability.controller,
                        duration.clone(),
                        TargetFilter::SpecificObject { id: recipient_id },
                        modifications.clone(),
                        None,
                    );
                }
            }
        }
        crate::game::layers::flush_layers(state);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::scenario::GameScenario;
    use crate::game::turns::execute_cleanup;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, ControllerRef, QuantityExpr, SacrificeCost, TargetFilter,
        TargetRef, TypedFilter,
    };
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    fn create_creature(
        state: &mut GameState,
        card_id: u64,
        player: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_name = name.to_string();
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        obj.card_types = obj.base_card_types.clone();
        id
    }

    /// A simple "{T}: Draw a card" activated ability.
    fn draw_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap)
    }

    /// "Sacrifice ~: Destroy target permanent" — the cost sacrifices the
    /// ability's own source (`SelfRef`), the CR 201.5b substitution proof.
    fn self_sacrifice_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::permanent()),
                cant_regenerate: false,
            },
        )
        .cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::SelfRef,
            1,
        )))
    }

    fn make_gain_ability(
        donor_id: ObjectId,
        source_id: ObjectId,
        recipient: TargetFilter,
        player: PlayerId,
        duration: Option<Duration>,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainActivatedAbilitiesOfTarget {
                target: TargetFilter::Any,
                recipient,
                duration,
            },
            vec![TargetRef::Object(donor_id)],
            source_id,
            player,
        )
    }

    // ── Plan engine test 1: SelfRef grant copies the donor's activated ability,
    // and activating it on the recipient produces the donor's effect sourced
    // from the recipient. Drives the real activation pipeline.
    #[test]
    fn self_grant_copies_donor_ability_and_activates_from_recipient() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let donor = scenario
            .add_creature(P0, "Donor", 1, 1)
            .with_ability_definition(draw_ability())
            .id();
        let quicksilver = scenario.add_creature(P0, "Quicksilver", 1, 1).id();
        scenario.with_library_top(P0, &["Reward Card"]);
        let mut runner = scenario.build();

        // Grant the donor's activated ability to Quicksilver.
        let ability = make_gain_ability(
            donor,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        let mut events = Vec::new();
        resolve(runner.state_mut(), &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(runner.state_mut());

        assert!(
            runner.state().objects[&quicksilver]
                .abilities
                .iter()
                .any(|a| *a == draw_ability()),
            "Quicksilver must gain the donor's activated ability; got {:?}",
            runner.state().objects[&quicksilver].abilities
        );

        // Activate the granted "{T}: Draw a card" on Quicksilver. The draw
        // resolves for Quicksilver's controller (P0) and taps Quicksilver, not
        // the donor — proving the granted ability is sourced from the recipient.
        let hand_before = runner.state().players[0].hand.len();
        let ability_index = runner.state().objects[&quicksilver].abilities.len() - 1;
        let outcome = runner.activate(quicksilver, ability_index).resolve();
        assert_eq!(
            outcome.state().players[0].hand.len(),
            hand_before + 1,
            "activating the granted ability must draw a card for the recipient's controller"
        );
        assert!(
            outcome.state().objects[&quicksilver].tapped,
            "the granted {{T}} cost must tap the recipient (Quicksilver)"
        );
        assert!(
            !outcome.state().objects[&donor].tapped,
            "the donor must NOT be tapped by the recipient's activation"
        );
    }

    // ── Plan engine test 2: grant expires at end-of-turn cleanup ──
    #[test]
    fn grant_reverts_at_cleanup() {
        let mut state = GameState::new_two_player(42);
        let donor = create_creature(&mut state, 1, P0, "Donor");
        state.objects.get_mut(&donor).unwrap().base_abilities = Arc::new(vec![draw_ability()]);
        let quicksilver = create_creature(&mut state, 2, P0, "Quicksilver");
        evaluate_layers(&mut state);

        let ability = make_gain_ability(
            donor,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&quicksilver].abilities.len(), 1, "granted");

        execute_cleanup(&mut state, &mut events);
        evaluate_layers(&mut state);
        assert!(
            state.objects[&quicksilver].abilities.is_empty(),
            "the grant must revert at end-of-turn cleanup (CR 611.2 until-end-of-turn)"
        );
    }

    // ── Plan engine test 3: CR 201.5b name substitution (self-sacrifice) ──
    //
    // The donor's "Sacrifice ~: Destroy ..." references the donor's own identity
    // via the self-sacrifice cost. After granting to Quicksilver, the granted
    // ability lives in Quicksilver's `abilities`; activating it (driving the
    // real cost-payment pipeline) must sacrifice QUICKSILVER (the new source),
    // not the donor.
    #[test]
    fn self_referential_cost_binds_to_recipient_not_donor() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let donor = scenario
            .add_creature(P0, "Donor", 1, 1)
            .with_ability_definition(self_sacrifice_ability())
            .id();
        let quicksilver = scenario.add_creature(P0, "Quicksilver", 1, 1).id();
        // A bystander permanent for the granted "Destroy target permanent".
        let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
        let mut runner = scenario.build();

        let ability = make_gain_ability(
            donor,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        let mut events = Vec::new();
        resolve(runner.state_mut(), &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(runner.state_mut());

        assert_eq!(
            runner.state().objects[&quicksilver].abilities[0],
            self_sacrifice_ability(),
            "Quicksilver gained the self-sacrifice ability"
        );

        // Activate the granted ability ON Quicksilver, targeting the bystander.
        // The SacrificeSelf cost is paid through the production activation
        // pipeline against the ability's source (Quicksilver).
        let ability_index = runner.state().objects[&quicksilver].abilities.len() - 1;
        let outcome = runner
            .activate(quicksilver, ability_index)
            .target_object(bystander)
            .resolve();
        let state = outcome.state();

        assert!(
            !state.battlefield.contains(&quicksilver),
            "CR 201.5b: the self-sacrifice cost must sacrifice the RECIPIENT (Quicksilver)"
        );
        assert!(
            state.battlefield.contains(&donor),
            "the donor must NOT be sacrificed by the granted self-referential cost"
        );
    }

    // ── Plan engine test 4: donor with zero activated abilities → no-op ──
    #[test]
    fn donor_without_activated_abilities_resolves_cleanly() {
        let mut state = GameState::new_two_player(42);
        let donor = create_creature(&mut state, 1, P0, "Vanilla Donor");
        let quicksilver = create_creature(&mut state, 2, P0, "Quicksilver");
        evaluate_layers(&mut state);

        let ability = make_gain_ability(
            donor,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&quicksilver].abilities.is_empty(),
            "no abilities granted when the donor has none"
        );
        assert!(
            !state
                .transient_continuous_effects
                .iter()
                .any(|t| t.source_id == quicksilver),
            "no empty-modification TCE registered for a zero-ability donor"
        );
    }

    // ── Plan engine test 5: two sequential grants from two donors stack ──
    #[test]
    fn two_sequential_grants_accumulate_independently() {
        let mut state = GameState::new_two_player(42);
        let donor_a = create_creature(&mut state, 1, P0, "Donor A");
        state.objects.get_mut(&donor_a).unwrap().base_abilities = Arc::new(vec![draw_ability()]);
        let donor_b = create_creature(&mut state, 2, P0, "Donor B");
        state.objects.get_mut(&donor_b).unwrap().base_abilities =
            Arc::new(vec![self_sacrifice_ability()]);
        let quicksilver = create_creature(&mut state, 3, P0, "Quicksilver");
        evaluate_layers(&mut state);

        let mut events = Vec::new();
        let ability_a = make_gain_ability(
            donor_a,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        resolve(&mut state, &ability_a, &mut events).unwrap();
        let ability_b = make_gain_ability(
            donor_b,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        resolve(&mut state, &ability_b, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&quicksilver]
                .abilities
                .iter()
                .any(|a| *a == draw_ability()),
            "first donor's ability retained after second grant"
        );
        assert!(
            state.objects[&quicksilver]
                .abilities
                .iter()
                .any(|a| *a == self_sacrifice_ability()),
            "second donor's ability also gained (independent TCEs, no overwrite)"
        );
    }

    // ── Plan engine test 6: donor controlled by an opponent still works ──
    #[test]
    fn donor_controlled_by_opponent_still_grants() {
        let mut state = GameState::new_two_player(42);
        // Donor controlled by the opponent (P1).
        let donor = create_creature(&mut state, 1, P1, "Opponent Donor");
        state.objects.get_mut(&donor).unwrap().base_abilities = Arc::new(vec![draw_ability()]);
        let quicksilver = create_creature(&mut state, 2, P0, "Quicksilver");
        evaluate_layers(&mut state);

        let ability = make_gain_ability(
            donor,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&quicksilver]
                .abilities
                .iter()
                .any(|a| *a == draw_ability()),
            "no controller restriction on the donor target (per the card's ruling)"
        );
    }

    // ── Plan engine test 8: CR 611.2c snapshot-once (not live rescan) ──
    #[test]
    fn snapshot_is_fixed_at_resolution_not_live_rescanned() {
        let mut state = GameState::new_two_player(42);
        let donor = create_creature(&mut state, 1, P0, "Donor");
        state.objects.get_mut(&donor).unwrap().base_abilities = Arc::new(vec![draw_ability()]);
        let quicksilver = create_creature(&mut state, 2, P0, "Quicksilver");
        evaluate_layers(&mut state);

        let ability = make_gain_ability(
            donor,
            quicksilver,
            TargetFilter::SelfRef,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&quicksilver].abilities.len(), 1);

        // Give the donor a NEW activated ability AFTER the grant resolved.
        {
            let donor_obj = state.objects.get_mut(&donor).unwrap();
            donor_obj.base_abilities = Arc::new(vec![draw_ability(), self_sacrifice_ability()]);
        }
        evaluate_layers(&mut state);

        // CR 611.2c: the recipient's granted set was frozen at resolution — it
        // must NOT pick up the donor's newly added ability.
        assert_eq!(
            state.objects[&quicksilver].abilities.len(),
            1,
            "snapshot-once: recipient must not gain the donor's later-added ability"
        );
        assert!(
            state.objects[&quicksilver]
                .abilities
                .iter()
                .all(|a| *a == draw_ability()),
            "only the originally-snapshotted ability remains"
        );
    }

    // ── Plan engine test 7: group recipient ("each Horror you control") ──
    #[test]
    fn group_recipient_grants_to_each_matching_object_only() {
        let mut state = GameState::new_two_player(42);
        // Opponent's artifact donor with one activated ability.
        let donor = create_object(
            &mut state,
            CardId(1),
            P1,
            "Opponent Artifact".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&donor).unwrap();
            obj.base_name = "Opponent Artifact".to_string();
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
            obj.base_abilities = Arc::new(vec![draw_ability()]);
        }

        // Two Horrors you control + one non-Horror you control.
        let horror_a = create_creature(&mut state, 2, P0, "Horror A");
        let horror_b = create_creature(&mut state, 3, P0, "Horror B");
        for h in [horror_a, horror_b] {
            let obj = state.objects.get_mut(&h).unwrap();
            obj.base_card_types.subtypes = vec!["Horror".to_string()];
            obj.card_types.subtypes = vec!["Horror".to_string()];
        }
        let bear = create_creature(&mut state, 4, P0, "Plain Bear");
        let grell = create_creature(&mut state, 5, P0, "Grell Philosopher");
        evaluate_layers(&mut state);

        let horror_filter = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Horror".to_string())
                .controller(ControllerRef::You),
        );
        let ability = make_gain_ability(
            donor,
            grell,
            horror_filter,
            P0,
            Some(Duration::UntilEndOfTurn),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&horror_a]
                .abilities
                .iter()
                .any(|a| *a == draw_ability()),
            "Horror A must gain the ability"
        );
        assert!(
            state.objects[&horror_b]
                .abilities
                .iter()
                .any(|a| *a == draw_ability()),
            "Horror B must gain the ability"
        );
        assert!(
            state.objects[&bear].abilities.is_empty(),
            "the non-Horror must NOT gain the ability"
        );
    }

    // ── Regression: group recipients must resolve against the resolving
    // ability's controller (CR 112.6a), not the live source object's current
    // controller. Simulates the source (Grell) leaving the battlefield before
    // its triggered ability resolves — `state.objects` no longer holds it, so
    // `FilterContext::from_source` would read `source_controller: None` and
    // silently match nobody. `FilterContext::from_ability` must instead use
    // `ability.controller`, captured when the ability was put on the stack. ──
    #[test]
    fn group_recipient_resolves_against_ability_controller_when_source_left_play() {
        let mut state = GameState::new_two_player(42);
        let donor = create_object(
            &mut state,
            CardId(1),
            P1,
            "Opponent Artifact".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&donor).unwrap();
            obj.base_name = "Opponent Artifact".to_string();
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
            obj.base_abilities = Arc::new(vec![draw_ability()]);
        }

        let horror = create_creature(&mut state, 2, P0, "Horror A");
        {
            let obj = state.objects.get_mut(&horror).unwrap();
            obj.base_card_types.subtypes = vec!["Horror".to_string()];
            obj.card_types.subtypes = vec!["Horror".to_string()];
        }
        let grell = create_creature(&mut state, 3, P0, "Grell Philosopher");
        evaluate_layers(&mut state);

        let horror_filter = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Horror".to_string())
                .controller(ControllerRef::You),
        );
        // The ability's controller is captured (P0) at trigger-resolution
        // setup time, exactly as `ResolvedAbility::controller` would hold it
        // on the stack — independent of whatever happens to the source object
        // afterward.
        let ability = make_gain_ability(
            donor,
            grell,
            horror_filter,
            P0,
            Some(Duration::UntilEndOfTurn),
        );

        // The source (Grell) leaves the battlefield entirely before the
        // triggered ability resolves (e.g., destroyed in response, while its
        // "at the beginning of your upkeep" trigger is still on the stack).
        // `state.objects` no longer has an entry for it.
        state.objects.remove(&grell);
        state.battlefield.retain(|id| *id != grell);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&horror]
                .abilities
                .iter()
                .any(|a| *a == draw_ability()),
            "CR 112.6a: the grant must still land on the ORIGINAL ability \
             controller's (P0) Horror even though the source has left play \
             and `state.objects` no longer has an entry for it"
        );
    }

    // ── Regression: group recipients must use the ability's captured
    // controller even when the live source object's controller has since
    // changed (e.g. a control-change effect resolved between the ability
    // being put on the stack and it resolving). ──
    #[test]
    fn group_recipient_resolves_against_ability_controller_when_source_controller_changed() {
        let mut state = GameState::new_two_player(42);
        let donor = create_object(
            &mut state,
            CardId(1),
            P1,
            "Opponent Artifact".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&donor).unwrap();
            obj.base_name = "Opponent Artifact".to_string();
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
            obj.base_abilities = Arc::new(vec![draw_ability()]);
        }

        // P0's Horror — must gain the ability, because the ability was put on
        // the stack under P0's control.
        let horror_p0 = create_creature(&mut state, 2, P0, "P0 Horror");
        // P1's Horror — must NOT gain the ability, even though P1 now controls
        // the live source object.
        let horror_p1 = create_creature(&mut state, 3, P1, "P1 Horror");
        for h in [horror_p0, horror_p1] {
            let obj = state.objects.get_mut(&h).unwrap();
            obj.base_card_types.subtypes = vec!["Horror".to_string()];
            obj.card_types.subtypes = vec!["Horror".to_string()];
        }
        let grell = create_creature(&mut state, 4, P0, "Grell Philosopher");
        evaluate_layers(&mut state);

        let horror_filter = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Horror".to_string())
                .controller(ControllerRef::You),
        );
        let ability = make_gain_ability(
            donor,
            grell,
            horror_filter,
            P0,
            Some(Duration::UntilEndOfTurn),
        );

        // Control of Grell changes to P1 after the triggered ability was put
        // on the stack (with P0 as its captured controller) but before it
        // resolves.
        state.objects.get_mut(&grell).unwrap().controller = P1;

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert!(
            state.objects[&horror_p0]
                .abilities
                .iter()
                .any(|a| *a == draw_ability()),
            "the grant must land on the ORIGINAL ability controller's (P0) Horror"
        );
        assert!(
            state.objects[&horror_p1].abilities.is_empty(),
            "the grant must NOT follow the source's new controller (P1)"
        );
    }
}
