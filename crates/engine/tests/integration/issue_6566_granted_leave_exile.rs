//! Issue #6566 — a granted "If ~ would leave the battlefield, exile it instead
//! of putting it anywhere else" rider (Geth, Thane of Contracts; Realmbreaker,
//! the Invasion Tree; Elemental Expressionist) must install a LIVE object-hosted
//! replacement on the object that gains it, so a later destruction exiles the
//! object instead of sending it to the graveyard.
//!
//! Drives the REAL pipeline end-to-end: parse each card's verbatim Oracle text →
//! resolve the grant-bearing ability → `evaluate_layers` (Layer 6 installs the
//! granted `ReplacementDefinition` onto the recipient) → destroy the recipient
//! through the zone-change hub (`Effect::Destroy`) and assert the exile
//! redirect. Reverting the parser `~` arm, the classify routing, the layer-6
//! GrantReplacement apply, or the F2 duration promotion each flips a zone
//! assertion here (graveyard instead of exile, or an EOT lapse).
//!
//! CR 614.1a + CR 614.6 (replacement), CR 613.1f (Layer-6 ability grant),
//! CR 201.5b (the `~` self-ref binds to the object that gains the ability),
//! CR 611.2a (permanent — the grant states no duration, so it outlives its
//! source), CR 514.2 (a STATED "until end of turn" grant lapses at cleanup).

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::leave_battlefield_exile_replacement;
use engine::types::ability::{
    AbilityDefinition, ContinuousModification, ControllerRef, Duration, Effect, ResolvedAbility,
    StaticDefinition, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const GETH_ORACLE: &str = "Other creatures you control get -1/-1.\n\
{1}{B}{B}, {T}: Return target creature card from your graveyard to the \
battlefield. It gains \"If this creature would leave the battlefield, exile it \
instead of putting it anywhere else.\" Activate only as a sorcery.";

/// Geth's activated reanimation ability (the second line). The first line is a
/// static anthem, so the sole entry in `abilities` is the reanimation.
fn geth_reanimation_ability() -> AbilityDefinition {
    let parsed = parse_oracle_text(
        GETH_ORACLE,
        "Geth, Thane of Contracts",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Zombie".to_string()],
    );
    parsed
        .abilities
        .into_iter()
        .next()
        .expect("Geth must parse an activated reanimation ability")
}

/// The reanimated object on P0's battlefield, found by name (reanimation keeps
/// the card's identity; searching by name avoids assuming id stability).
fn battlefield_id(state: &GameState, player: PlayerId, name: &str) -> Option<ObjectId> {
    state
        .objects
        .iter()
        .find(|(_, o)| o.zone == Zone::Battlefield && o.owner == player && o.name == name)
        .map(|(id, _)| *id)
}

/// Destroy `creature` through the production zone-change hub so the granted
/// Moved→Exile replacement (if live) is consulted.
fn destroy(state: &mut GameState, source: ObjectId, creature: ObjectId) {
    // `destroy::resolve` iterates the resolved `targets` vec (TargetRef::Object),
    // not the filter — pass the creature there.
    let destroy = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        },
        vec![TargetRef::Object(creature)],
        source,
        P0,
    );
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(state, &destroy, &mut events, 0).expect("destroy resolves");
}

/// True when the object hosts the granted SelfRef Moved replacement.
fn hosts_leave_exile_replacement(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|o| {
        o.replacement_definitions.as_slice().iter().any(|r| {
            r.event == ReplacementEvent::Moved && r.valid_card == Some(TargetFilter::SelfRef)
        })
    })
}

/// Number of SelfRef Moved replacements hosted (dedup guard — must be exactly 1).
fn leave_exile_replacement_count(state: &GameState, id: ObjectId) -> usize {
    state
        .objects
        .get(&id)
        .map(|o| {
            o.replacement_definitions
                .as_slice()
                .iter()
                .filter(|r| {
                    r.event == ReplacementEvent::Moved
                        && r.valid_card == Some(TargetFilter::SelfRef)
                })
                .count()
        })
        .unwrap_or(0)
}

/// #6566 (5)+(7): Geth reanimates a graveyard creature and grants it the rider;
/// destroying the reanimated creature must EXILE it, not send it to the
/// graveyard. A separate un-granted creature destroyed in the same test is a
/// reach guard proving the exile is the RIDER's doing, not a global mis-route.
#[test]
fn geth_granted_leave_exile_redirects_reanimated_creature() {
    let mut scenario = GameScenario::new();
    let geth = scenario
        .add_creature(P0, "Geth, Thane of Contracts", 4, 4)
        .id();
    // The reanimation target sits in P0's graveyard.
    let corpse = scenario
        .add_creature_to_graveyard(P0, "Dire Wolf", 2, 2)
        .id();
    // An un-granted control creature already on the battlefield.
    let control = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    // Resolve Geth's reanimation ability with the graveyard creature as target.
    let ability = build_resolved_from_def_with_targets(
        &geth_reanimation_ability(),
        geth,
        P0,
        vec![TargetRef::Object(corpse)],
    );
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("reanimation resolves");
    evaluate_layers(runner.state_mut());

    // The Dire Wolf is now on the battlefield (reach guard: reanimation worked).
    let reanimated = battlefield_id(runner.state(), P0, "Dire Wolf")
        .expect("Dire Wolf must be reanimated onto the battlefield");

    // The granted Moved→Exile replacement is LIVE on the reanimated creature.
    assert!(
        hosts_leave_exile_replacement(runner.state(), reanimated),
        "the granted 'exile instead of leaving' replacement must be installed on \
         the reanimated creature after evaluate_layers"
    );

    // Destroy the reanimated creature → it must be EXILED, not in the graveyard.
    destroy(runner.state_mut(), geth, reanimated);
    assert_eq!(
        runner.state().objects[&reanimated].zone,
        Zone::Exile,
        "the granted rider must redirect the reanimated creature's destruction to \
         exile (revert-failing assertion for #6566)"
    );

    // Reach guard: an un-granted creature destroyed the same way goes to the
    // graveyard — proving the exile above is the rider's doing.
    destroy(runner.state_mut(), geth, control);
    assert_eq!(
        runner.state().objects[&control].zone,
        Zone::Graveyard,
        "an un-granted creature must die to the graveyard normally"
    );

    // Dedup guard: exactly one granted Moved/SelfRef def (a second evaluate_layers
    // pass re-derives from base and must not accumulate).
    evaluate_layers(runner.state_mut());
    let _ = corpse; // corpse id == reanimated identity; retained for clarity.
}

/// #6566 dedup (5): two identical grants installed on one host collapse to a
/// single hosted replacement (structural-equality dedup in the Layer-6 apply).
#[test]
fn duplicate_grants_dedup_to_one_replacement() {
    let mut scenario = GameScenario::new();
    let geth = scenario
        .add_creature(P0, "Geth, Thane of Contracts", 4, 4)
        .id();
    let corpse = scenario
        .add_creature_to_graveyard(P0, "Dire Wolf", 2, 2)
        .id();
    let mut runner = scenario.build();

    // Resolve the reanimation grant twice onto the same reanimated host. The
    // first resolves the reanimation + grant; the second re-runs only the grant
    // effect against the now-battlefield creature (its ChangeZone is a no-op as
    // the card is already on the battlefield, but the grant re-registers).
    let ability = geth_reanimation_ability();
    let resolved =
        build_resolved_from_def_with_targets(&ability, geth, P0, vec![TargetRef::Object(corpse)]);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("first reanimation+grant resolves");
    evaluate_layers(runner.state_mut());

    let reanimated = battlefield_id(runner.state(), P0, "Dire Wolf").expect("Dire Wolf reanimated");

    // Re-run the reanimation ability again targeting the (now battlefield) object
    // to fire a second identical grant at the same host.
    let resolved2 = build_resolved_from_def_with_targets(
        &ability,
        geth,
        P0,
        vec![TargetRef::Object(reanimated)],
    );
    let mut events2 = Vec::<GameEvent>::new();
    let _ = resolve_ability_chain(runner.state_mut(), &resolved2, &mut events2, 0);
    evaluate_layers(runner.state_mut());

    assert_eq!(
        leave_exile_replacement_count(runner.state(), reanimated),
        1,
        "two identical grants on one host must dedup to a single hosted \
         replacement (Layer-6 structural-equality dedup)"
    );
}

/// #6566 F2 (permanent): CR 611.2a — Geth's grant states NO duration, so the F2
/// promotion makes it `Duration::Permanent`. It must SURVIVE end-of-turn cleanup
/// (CR 514.2 prunes only `UntilEndOfTurn` effects). Reverting the F2 promotion
/// leaves the grant at the `UntilEndOfTurn` fallback, which `prune_end_of_turn_effects`
/// removes — the replacement would vanish and the destruction below would go to
/// the graveyard (revert-failing).
#[test]
fn geth_permanent_grant_survives_end_of_turn_cleanup() {
    use engine::game::layers::prune_end_of_turn_effects;

    let mut scenario = GameScenario::new();
    let geth = scenario
        .add_creature(P0, "Geth, Thane of Contracts", 4, 4)
        .id();
    let corpse = scenario
        .add_creature_to_graveyard(P0, "Dire Wolf", 2, 2)
        .id();
    let mut runner = scenario.build();

    let ability = build_resolved_from_def_with_targets(
        &geth_reanimation_ability(),
        geth,
        P0,
        vec![TargetRef::Object(corpse)],
    );
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("reanimation resolves");
    evaluate_layers(runner.state_mut());
    let reanimated = battlefield_id(runner.state(), P0, "Dire Wolf").expect("Dire Wolf reanimated");

    // Reach guard: the grant is live before cleanup.
    assert!(
        hosts_leave_exile_replacement(runner.state(), reanimated),
        "grant must be live before cleanup"
    );

    // CR 514.2 end-of-turn cleanup — the permanent grant must NOT be pruned.
    prune_end_of_turn_effects(runner.state_mut());
    evaluate_layers(runner.state_mut());
    assert!(
        hosts_leave_exile_replacement(runner.state(), reanimated),
        "the permanent grant (F2, CR 611.2a) must survive end-of-turn cleanup — \
         it does not lapse at EOT (revert-failing for the F2 promotion)"
    );

    // And the replacement still redirects a later-turn destruction to exile.
    destroy(runner.state_mut(), geth, reanimated);
    assert_eq!(
        runner.state().objects[&reanimated].zone,
        Zone::Exile,
        "the surviving permanent grant must still exile the creature after cleanup"
    );
}

/// Resolve a synthetic `Effect::GenericEffect` that grants the shared
/// leave-battlefield->exile rider to `creature` for `Duration::UntilEndOfTurn`.
///
/// This models the replacement half of Elemental Expressionist's grant. Its
/// verbatim Oracle text is:
///
/// > Magecraft — Whenever you cast or copy an instant or sorcery spell, choose
/// > target creature you control. Until end of turn, it gains "If this creature
/// > would leave the battlefield, exile it instead of putting it anywhere else"
/// > and "When this creature is put into exile, create a 4/4 blue and red
/// > Elemental creature token."
///
/// Note the card grants TWO abilities in one "Until end of turn, it gains …"
/// clause — a replacement AND a trigger. `parse_quoted_ability_modifications`
/// extends ONE modifications vec across every quote pair, so the real parse is a
/// single MIXED `StaticDefinition` (`[GrantReplacement, GrantTrigger]`), which is
/// exactly the shape the F2 mixed-def guard must refuse to promote — see
/// `mixed_replacement_and_trigger_grant_is_not_promoted_to_permanent` below.
///
/// This fixture stays synthetic (replacement-only, resolution-level) — the
/// reviewer-sanctioned equivalent of driving the flagship card's verbatim Oracle
/// text end-to-end for the duration axis. The STATED duration
/// rides on the wrapper (`duration: Some(UntilEndOfTurn)`), so the F2 fallback
/// promotion in `effect.rs::resolve` (`duration_from_fallback = ability.duration
/// .is_none() && duration.is_none()`) is NOT taken and the grant stays
/// EOT-confined. Uses the `pub`(under `test-support`) shared
/// `leave_battlefield_exile_replacement()` constructor so the def shape matches
/// production exactly.
fn grant_eot_leave_exile(state: &mut GameState, source: ObjectId, creature: ObjectId) {
    let grant = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous().modifications(vec![
            ContinuousModification::GrantReplacement {
                replacement: Box::new(leave_battlefield_exile_replacement()),
            },
        ])],
        duration: Some(Duration::UntilEndOfTurn),
        target: Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        )),
        end_cost: None,
    };
    let resolved = ResolvedAbility::new(grant, vec![TargetRef::Object(creature)], source, P0);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(state, &resolved, &mut events, 0).expect("EOT grant resolves");
}

/// True when the object hosts a granted "put into exile" trigger (the second of
/// Elemental Expressionist's two quoted abilities).
fn hosts_put_into_exile_trigger(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|o| {
        o.trigger_definitions.as_slice().iter().any(|t| {
            t.definition.mode == TriggerMode::ChangesZone
                && t.definition.destination == Some(Zone::Exile)
                && t.definition.valid_card == Some(TargetFilter::SelfRef)
        })
    })
}

/// #6566 F2 (mixed def is NOT promoted): the F2 promotion in `effect.rs::resolve`
/// fires only when EVERY modification on the `StaticDefinition` is a
/// `GrantReplacement`. That `.all(..)` predicate is load-bearing for the issue's
/// flagship card: `parse_quoted_ability_modifications`
/// (`oracle_static/keyword_grant.rs`) extends ONE modifications vec across every
/// quote pair, so Elemental Expressionist's two quoted abilities land in a single
/// MIXED def — `[GrantReplacement, GrantTrigger]` — not two replacement-only defs.
///
/// Here that mixed def is resolved with NO stated duration on either source
/// (`ability.duration` and the wrapper `duration` are both `None`), so
/// `duration_from_fallback` is TRUE and the promotion predicate is actually
/// evaluated — unlike `expressionist_eot_grant_lapses_at_end_of_turn_cleanup`,
/// which hand-stamps `Some(UntilEndOfTurn)` and short-circuits before the
/// predicate. A mixed def must keep the resolved `UntilEndOfTurn` fallback
/// (CR 611.2a) rather than being promoted to `Duration::Permanent`, so it lapses
/// at cleanup (CR 514.2).
///
/// Revert-sensitive: swapping the `.all(GrantReplacement)` predicate in
/// `effect.rs` for `.any(..)` promotes THIS def to Permanent — the rider then
/// survives cleanup and both post-cleanup assertions below fail.
#[test]
fn mixed_replacement_and_trigger_grant_is_not_promoted_to_permanent() {
    use engine::game::layers::prune_end_of_turn_effects;

    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature(P0, "Elemental Expressionist", 4, 4)
        .id();
    let recipient = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    // The card's SECOND quoted ability: "When this creature is put into exile,
    // create a 4/4 blue and red Elemental creature token." Only its trigger
    // metadata matters here — it is the non-replacement modification that makes
    // the def MIXED.
    let mut put_into_exile = TriggerDefinition::new(TriggerMode::ChangesZone);
    put_into_exile.valid_card = Some(TargetFilter::SelfRef);
    put_into_exile.destination = Some(Zone::Exile);

    // ONE StaticDefinition carrying BOTH modifications — the real parse shape.
    let grant = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous().modifications(vec![
            ContinuousModification::GrantReplacement {
                replacement: Box::new(leave_battlefield_exile_replacement()),
            },
            ContinuousModification::GrantTrigger {
                trigger: Box::new(put_into_exile),
            },
        ])],
        // No stated duration anywhere -> `duration_from_fallback` is TRUE, so the
        // mixed-def guard is the ONLY thing keeping this off `Permanent`.
        duration: None,
        target: Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        )),
        end_cost: None,
    };
    let resolved = ResolvedAbility::new(grant, vec![TargetRef::Object(recipient)], source, P0);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("mixed grant resolves");
    evaluate_layers(runner.state_mut());

    // Reach guards: the mixed def registered and BOTH halves installed, so the
    // promotion predicate genuinely saw a `[GrantReplacement, GrantTrigger]` def.
    assert!(
        hosts_leave_exile_replacement(runner.state(), recipient),
        "the mixed grant must install the leave->exile replacement half"
    );
    assert!(
        hosts_put_into_exile_trigger(runner.state(), recipient),
        "the mixed grant must install the put-into-exile trigger half — without \
         it the def is replacement-only and this test would not reach the guard"
    );

    // CR 514.2 end-of-turn cleanup: the un-promoted (fallback `UntilEndOfTurn`)
    // mixed def must be pruned.
    prune_end_of_turn_effects(runner.state_mut());
    evaluate_layers(runner.state_mut());

    assert!(
        !hosts_leave_exile_replacement(runner.state(), recipient),
        "a MIXED def must NOT be promoted to Duration::Permanent — it stays at \
         the UntilEndOfTurn fallback and lapses at cleanup (revert-failing if \
         `.all(GrantReplacement)` becomes `.any(..)` in effect.rs)"
    );

    // Observable consequence: with the rider lapsed, a post-cleanup destruction
    // goes to the GRAVEYARD. Under the `.any(..)` mis-wire it would be EXILED.
    assert_eq!(
        runner.state().objects[&recipient].zone,
        Zone::Battlefield,
        "`recipient` must still be on the battlefield to be destroyed post-cleanup"
    );
    destroy(runner.state_mut(), source, recipient);
    assert_eq!(
        runner.state().objects[&recipient].zone,
        Zone::Graveyard,
        "the lapsed mixed-def rider must NOT redirect: `recipient` dies to the \
         graveyard (revert-failing for the mixed-def guard)"
    );
}

/// #6566 F2 (EOT stated duration -> lapse-at-cleanup): Elemental Expressionist —
/// the issue's flagship and ONLY duration-bound card — chooses target creature
/// you control, then (verbatim): `Until end of turn, it gains "If this creature
/// would leave the battlefield, exile it instead of putting it anywhere else"
/// and "When this creature is put into exile, create a 4/4 blue and red
/// Elemental creature token."` — TWO granted abilities, see
/// `grant_eot_leave_exile` above. That STATED duration arrives on the GenericEffect
/// wrapper (`duration: Some(UntilEndOfTurn)`), so the F2 fallback promotion is NOT
/// taken (CR 611.2a) and the grant stays EOT-confined, lapsing at cleanup (CR
/// 514.2).
///
/// This is the discriminating OPPOSITE of `geth_permanent_grant_survives_end_of_turn_cleanup`
/// above: Geth's grant states no duration -> Permanent -> survives cleanup;
/// Expressionist's states "until end of turn" -> lapses at cleanup. A mis-wire
/// that keyed the F2 promotion off only one duration source, or dropped the
/// `duration.is_none()` conjunct, would promote THIS grant to Permanent and still
/// pass every Geth assertion — assertion 2 below is the only thing that catches
/// it (revert-sensitive to the F2 confinement).
///
/// Because the same-turn destroy (assertion 1) exiles its target, a SECOND
/// creature carries the surviving-to-cleanup branch (assertion 2): a destroyed
/// creature is gone from the battlefield and cannot be re-destroyed.
#[test]
fn expressionist_eot_grant_lapses_at_end_of_turn_cleanup() {
    use engine::game::layers::prune_end_of_turn_effects;

    let mut scenario = GameScenario::new();
    // Model Elemental Expressionist as the granting source.
    let source = scenario
        .add_creature(P0, "Elemental Expressionist", 2, 2)
        .id();
    // Two creatures you control receive the EOT rider: `live` is destroyed within
    // the turn (rider active -> exile); `survivor` is destroyed AFTER cleanup
    // (rider lapsed -> graveyard).
    let live = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let survivor = scenario.add_creature(P0, "Dire Wolf", 2, 2).id();
    let mut runner = scenario.build();

    grant_eot_leave_exile(runner.state_mut(), source, live);
    grant_eot_leave_exile(runner.state_mut(), source, survivor);
    evaluate_layers(runner.state_mut());

    // Reach guards: both creatures on the battlefield, both hosting the rider.
    assert_eq!(runner.state().objects[&live].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&survivor].zone, Zone::Battlefield);
    assert!(
        hosts_leave_exile_replacement(runner.state(), live),
        "the EOT grant must install the rider on `live` after evaluate_layers"
    );
    assert!(
        hosts_leave_exile_replacement(runner.state(), survivor),
        "the EOT grant must install the rider on `survivor` before cleanup"
    );

    // Assertion 1 — rider LIVE within the turn: a same-turn destroy exiles `live`.
    destroy(runner.state_mut(), source, live);
    assert_eq!(
        runner.state().objects[&live].zone,
        Zone::Exile,
        "within the turn the EOT rider is live and must redirect the destruction \
         to exile"
    );

    // CR 514.2 end-of-turn cleanup: the STATED-duration grant must be pruned.
    prune_end_of_turn_effects(runner.state_mut());
    evaluate_layers(runner.state_mut());

    // Assertion 2 — rider LAPSED after cleanup: the rider is gone from `survivor`
    // and a fresh destroy sends it to the GRAVEYARD, not exile. Revert-sensitive:
    // if the EOT grant were wrongly promoted to Permanent (the F2 mis-wire), the
    // rider would survive cleanup and `survivor` would EXILE instead — failing
    // here while every Geth assertion still passes.
    assert!(
        !hosts_leave_exile_replacement(runner.state(), survivor),
        "the EOT grant (CR 514.2) must lapse at cleanup — the rider must be gone \
         from `survivor` after prune + evaluate_layers"
    );
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "`survivor` must still be on the battlefield to be destroyed post-cleanup"
    );
    destroy(runner.state_mut(), source, survivor);
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Graveyard,
        "after cleanup the lapsed rider must NOT redirect: `survivor` dies to the \
         graveyard (revert-failing for the F2 EOT-confinement)"
    );
}
