//! Regression: a play permission that states its own lifetime ("you may play
//! that card FOR AS LONG AS you control this creature") is a lingering
//! permission (CR 611.2a), not a resolution-time "may" offer (CR 608.2d).
//!
//! Bug: `lower_imperative_clause` stripped the trailing duration and assigned it
//! to `ParsedEffectClause.duration` alone, bypassing the shared
//! `with_clause_duration` building block that also patches the duration slot
//! INSIDE effects that own one. `Effect::CastFromZone.duration` therefore stayed
//! `None`, and the two consumers this class reaches both went wrong:
//!
//!   * `is_lingering_cast_from_zone` (parser assembly) stopped recognizing the
//!     clause as a deferred grant, so the clause's "may" was promoted to
//!     `AbilityDefinition.optional` — the engine paused resolution with a
//!     `WaitingFor::OptionalEffectChoice`, and declining destroyed the grant
//!     outright, stranding the card in exile forever.
//!   * `record_lingering_permissions` (engine) reads the same effect field for
//!     the granted permission's expiry, so an accepted grant was stamped with no
//!     duration at all.
//!
//! F3 — the field gained a value it never carried, so its five reader sites were
//! walked: `is_lingering_cast_from_zone` (assembly), the two `duration.is_none()`
//! gates in `cast_from_zone::resolve` (`immediate_graveyard_free_cast` and
//! `paid_during_resolution_cast`), `record_lingering_permissions`, and
//! `legacy_duration` (ability_rw). Named rather than numbered on purpose: the
//! line numbers this paragraph once carried had all rotted by the time the
//! fourth defect below was written. The first and fourth are the two above. The
//! other three cannot move for any input:
//!
//!   * both `duration.is_none()` gates require exactly that, so populating the
//!     field can only ever CLOSE them, never open one.
//!   * `legacy_duration` is a two-armed match: `ForAsLongAs` delegates to
//!     `legacy_static_condition`, where the `Unrecognized` conditions this fix
//!     installs are a `false` arm; every other variant is `false` outright. No
//!     duration this fix can install moves it.
//!
//! Fix (first half): route that assignment through `with_clause_duration`.
//!
//! SECOND DEFECT, same class, found by playtest and by review: recording the
//! duration is not enough, because nothing ever ENFORCED it.
//! `layers::prune_host_left_effects` retains only
//! `state.transient_continuous_effects`, and `CastingPermission::ExileWithAltCost`
//! carried no granting-source identity to prune against — so a permission
//! stamped `Duration::UntilHostLeavesPlay` outlived its host indefinitely,
//! turning a prompt bug into a rules-incorrect standing permission.
//!
//! Fix (second half): `ExileWithAltCost` gains `source_id`, stamped in
//! `record_lingering_permissions` and `grant_permission::resolve` exactly where
//! the `PlayFromExile` land companion already stamped its own; and
//! `layers::prune_host_left_casting_permissions` revokes both halves from the
//! battlefield-exit lifecycle in `zones::apply_zone_exit_cleanup`, beside the
//! existing `prune_host_left_effects` call.
//!
//! THIRD DEFECT SET, same class again: recording and enforcing ONE lifetime is
//! not enough while each prune keeps its own hand-written variant list. Three
//! holes were open at once, and no single place could be read to see any of
//! them:
//!
//!   * `CastingPermission::ExileWithAltAbilityCost` — the CR 118.9 NON-MANA
//!     alternative-cost form — had no `duration` field at all, so a stated
//!     lifetime was dropped where the permission was built. Nashi, Moon Sage's
//!     Scion prints exactly that pair ("Until end of turn, you may play one of
//!     those cards" + "pay life equal to its mana value rather than paying its
//!     mana cost"), and its exiled cards stayed playable for the rest of the
//!     game.
//!   * `Duration::UntilNextStepOf { step: Untap }` reached casting permissions
//!     from the parser but no prune owned it.
//!   * "for as long as you CONTROL ~" and "for as long as ~ REMAINS ON THE
//!     BATTLEFIELD" both lowered to `UntilHostLeavesPlay`, so a control change
//!     ended neither. Both wordings are printed on play permissions (Gwen
//!     Stacy / Hama vs. Intet, the Dreamer / The Day of the Doctor), so
//!     collapsing them either keeps a stolen grantor's permission alive or
//!     revokes a presence-bound one too early.
//!
//! THE PHASING LEG, split rather than named. `UntilHostLeavesPlay` used to
//! carry two printed wordings, and CR 702.26f governs only one: "for as long
//! as ~ remains on the battlefield" is a CR 611.2b duration that ends when the
//! permanent phases out, while "until ~ leaves the battlefield" is a plain
//! event deadline that a phase-out does not reach (CR 702.26d: phasing is not
//! a zone change). The wording now lives on the variant —
//! `Duration::WhileHostOnBattlefield` for the presence reading — so each side
//! asks the phasing question exactly where it belongs: the permission lapse
//! pass asks `is_phased_in()` for the presence reading (Intet, the Dreamer and
//! The Day of the Doctor, the only permissions in the corpus with a presence
//! lifetime), `transient_effect_is_live` and `prune_lapsed_host_bound_effects`
//! END a presence-bound transient on the host's phase-out (Sower of
//! Temptation's steal — `sower_phase_out_ends_presence_bound_steal.rs`), and
//! the event deadline is ended by nothing but the host's actual battlefield
//! exit (`an_event_deadline_permission_ends_only_at_the_hosts_battlefield_exit`).
//!
//! Fix (third): one model instead of three patches.
//! `CastingPermission::lifetime` is the single place that knows which variants
//! carry a lifetime and where; `layers::permission_duration_expires_at` is the
//! single expiry table, keyed on a named `PermissionSeam`; both are
//! wildcard-free, so a new permission variant or a new `Duration` variant is a
//! compile error rather than a silent hole. A duration no seam implements is
//! refused at the grant sites (`casting_permission_duration_is_enforceable`)
//! instead of being attached unbounded. `Duration::WhileControllingHost` gives
//! the control reading its own variant, evaluated by
//! `replacement::controller_controls_source_gate` — the authority that already
//! answers CR 611.2b for replacement conditions.
//!
//! Two members of the repaired set are covered, reached by different routes: a
//! top-of-library impulse with the "that card" anaphor (Gwen Stacy) and a
//! targeted graveyard exile with the "it" anaphor (Victor Mancha, Runaway).
//! Both print "for as long as you CONTROL ~" and so carry
//! `Duration::WhileControllingHost` (see the third defect set above).
//!
//! THE READER WALK ABOVE COVERS ONE DIRECTION ONLY, and this change is the
//! first to need the other: "populating the field can only ever CLOSE them" is
//! true of a write, and Twinning Glass is a CLEAR — it loses the invented
//! `UntilEndOfTurn`. Both `duration.is_none()` gates were re-read for that
//! direction and both stay shut: `immediate_graveyard_free_cast` additionally
//! requires a target in `Zone::Graveyard`, `paid_during_resolution_cast`
//! additionally requires `!without_paying`, and Twinning Glass is a free cast
//! from the hand.
//!
//! FOURTH DEFECT, the free half of the same class (issue #8029, reported from
//! play on Thranduil's Decree as #7132): recording, enforcing and typing the
//! lifetime is still not enough while the CASTING MECHANISM ignores it. A
//! stated lifetime and a CR 608.2g during-resolution cast are mutually
//! exclusive — a cast that happens as the ability resolves has no later
//! priority window (CR 117.1a), which is exactly what a printed lifetime
//! claims. `CastFromZoneDriver::with_lingering_duration` nevertheless answered
//! `Some(DuringResolution)` for the single-card mechanism, and the one seam
//! that degraded it anyway did so behind a `without_paying_mana_cost: false`
//! guard. So the free members of the shape kept the one-shot CR 608.2g
//! mechanism their own text contradicts — surfacing, wherever the clause's
//! "may" was promoted, as the CR 608.2d prompt this file is named after. CR
//! 118.9 governs what a permission COSTS, never when it is exercised, and it
//! was doing the deciding.
//!
//! Fix (fourth): the degrade moves INTO `with_lingering_duration`, the shared
//! authority every duration seam already calls, and the hand-written guard goes.
//! MEASURED, because "write the guard wider where it stood" is the obvious
//! alternative and it does not work: a reach marker at that block does fire over
//! the corpus, so a wider guard there would have reached the TRAILING-duration
//! members — but a sentence-leading duration is stamped around the body
//! lowering, after `lower_imperative_clause` has returned, and never sees that
//! gate at all. Only the shared authority covers both. (With the judgement in
//! the authority the block is now redundant for a cast clause: neutralizing it
//! entirely leaves every cast grant in the corpus byte-identical.)
//!
//! REPAIRED SINCE, in `lasting_cast_from_hand_permission`, and NOT by giving
//! Chandra the lingering `CastFromZone` mechanism this module is about: her
//! ultimate is promoted out of `CastFromZone` altogether, into
//! `StaticMode::CastFromHandFree` — Omniscience's mechanism — carried as a
//! duration-bound player grant. A per-object permission was never going to be
//! right for her: it is stamped once per card at resolution, so a card DRAWN
//! LATER in the same turn is not covered, and "Until end of turn, you may cast
//! spells from your hand" covers it.
//!
//! The paragraph below is kept as written because it is the measurement that
//! identified the seam, and because its diagnosis still stands: the resolver
//! routed the hand by ZONE alone and never asked whether the grant stated a
//! lifetime.
//!
//! KARLACH IS NOT COVERED THERE, and the sentence below overstates what was
//! measured about her, so read it with this correction. What IS measured: her
//! grant carries `target: ParentTarget`, which is neither a `TargetFilter::Typed`
//! (so the promotion's gate declines it) nor a filter `extract_in_zone()` can
//! answer for (so this resolver's hand tail never sees it either). What is NOT
//! measured: whether she is broken at all. The reading below was taken by
//! driving her grant clause in isolation, where the parent target is unbound and
//! the resolver exits at "No targets resolved". In a real specialize chain the
//! seam may bind the sought card, in which case the ordinary target path reaches
//! `grant_lingering_permissions` and stamps a correct in-place hand permission.
//! Driving the real trigger is the measurement nobody has made; until then she
//! is untested, not established as wrong.
//!
//! THE STATE AT THE TIME OF THIS CHANGE — every sentence from here to the end of
//! this paragraph is in the PRESENT TENSE OF THEN, not of now: Chandra's half is
//! since repaired (she no longer reaches this resolver at all), and Karlach's
//! half stands subject to the correction above. It concerns the HAND-RESIDENT
//! members. Chandra, Flame's Catalyst's
//! ultimate offers the hand through a filter; Karlach, Tiefling Spellrager's
//! sought card lands there by a different road. (Twinning Glass is hand-origin
//! too, but it loses an invented duration and keeps its during-resolution
//! cast.) For both this change moves the AST and nothing else. MEASURED end-to-end
//! through `GameRunner`, with the degrade and without it, the ultimate stops at
//! the same `WaitingFor::EffectZoneChoice { effect_kind: CastFromZone, zone:
//! Hand, up_to: true, duration: None }`: a pick-one-now offer raised while the
//! ability is still resolving, no permission recorded on any hand card, none
//! castable afterwards. `resolve` does consult the driver earlier — for the
//! library one-shot and for `window_bounds()` — but neither route's remaining
//! conditions hold for a hand pool with no resolved targets, and the branch it
//! does take reads neither field (`open_private_zone_cast_selection` writes
//! `duration: None` as a literal). So the card behaves exactly as it does on
//! main. This is not a
//! wrong lifetime left standing: no permission is granted for the hand pool at
//! all, before or after, so nothing outlives anything. The card already carried
//! `duration: UntilEndOfTurn` on main; what the change buys it is an AST whose
//! MECHANISM finally matches that printed lifetime, which is the field a runtime
//! repair would have to read.
//!
//! The `Duration::ForAsLongAs` half of that set IS covered, through Gale's
//! Redirection — CR 202.3 makes its d20 table deterministic for a countered
//! spell of mana value 14, so the 15+ row is reached without touching the die.
//! Its three siblings are NOT reachable end-to-end, measured with a probe at
//! `cast_from_zone::resolve`: Thranduil's Decree and Kheru Spellsnatcher never
//! reach that resolver at all (the countered card lands in exile and the tracked
//! set holds it, but the grant clause under the `ZoneChangedThisWay` link never
//! resolves), and Planeswalker's Mischief reaches it with an empty tracked set
//! (the "reveals a card at random … exile it" anaphor never binds the revealed
//! card). Separate defects upstream of this seam; #7132 is open on the first and
//! stays open.
//!
//! The PLURAL members are a third such case, and the reason a plural grant is a
//! different FORM rather than a different anaphor ("cards exiled this way …
//! their mana costs" on Dream Harvest, "those cards … their mana costs" on
//! Ugin, Eye of the Storms):
//! `driver_free_cast` binds a single object. Driven through `GameRunner` with
//! the degrade and without it, Dream Harvest exiles its cards and records a
//! permission on NONE of them either way — unchanged, like the hand pool below.
//! Ugin, Eye of the Storms prints the same plural.
//!
//! Their parse side is pinned in
//! `parser::oracle_effect::tests::a_free_cast_grant_with_a_stated_lifetime_is_a_lingering_permission`.
//! Spelljack and Resourceful Collector print a similar lifetime but were already
//! lingering (and Spelljack is `mode: Play`, CR 305.1), so neither discriminates.
//!
//! CR 611.2a: a continuous effect generated by the resolution of a spell or
//! ability "lasts as long as stated by the spell or ability creating it".
//! CR 611.2b: the "for as long as . . ." wording is exactly this class; its own
//! example is Master Thief's "for as long as you control this creature", the
//! same shape as Gwen Stacy's and Victor Mancha's.
//! CR 608.2d: a resolution-time choice is one the player "announces … while
//! applying the effect" — the shape this class was wrongly classified as.
//! CR 601.3: "A player can begin to cast a spell only if a rule or effect allows
//! that player to cast it" — what the recorded permission provides.
//! CR 305.1: playing a land "is a special action … it is never a spell", so the
//! land companion of a `mode: Play` grant is authorized by a different rule than
//! its spell half.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CastingPermission, Duration, Effect, FilterProp, LibraryPosition,
    QuantityExpr, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (`client/public/card-data.json`, key `gwen stacy`).
const GWEN_STACY: &str =
    "When Gwen Stacy enters, exile the top card of your library. You may play \
     that card for as long as you control this creature.\n{2}{U}{R}{W}: Transform Gwen Stacy. \
     Activate only as a sorcery.";

/// Verbatim Oracle text (`client/public/card-data.json`, key
/// `victor mancha, runaway`).
const VICTOR_MANCHA: &str = "When Victor Mancha enters, exile target card from your graveyard. \
     You may play it for as long as you control Victor Mancha.";

/// Verbatim Oracle text (`client/public/card-data.json`, key
/// `the day of the doctor`).
const DAY_OF_THE_DOCTOR: &str =
    "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\n\
     I, II, III — Exile cards from the top of your library until you exile a legendary card. \
     You may play that card for as long as this Saga remains on the battlefield. Put the rest of \
     those exiled cards on the bottom of your library in a random order.\n\
     IV — Choose up to three Doctors. You may exile all other creatures. If you do, this Saga \
     deals 13 damage to you.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .zone
}

fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// CR 305.1: a land is PLAYED, never cast, so the land companion of a
/// `mode: Play` grant surfaces as `GameAction::PlayLand` — a different action
/// than `can_cast` above inspects.
fn can_play_land(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == id))
}

/// Verbatim Oracle text (`client/public/card-data.json`, key `murder`): the
/// test needs a production removal path, not Murder specifically.
const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";

/// Remove `victim` from the battlefield by casting and resolving a removal
/// spell, so the battlefield-exit lifecycle in `zones::apply_zone_exit_cleanup`
/// actually runs. CR 701.8a: to destroy a permanent is to "move it from the
/// battlefield to its owner's graveyard", done here during the removal spell's
/// own resolution; CR 701.8b lists the only other route as the state-based
/// actions for lethal or deathtouch damage, neither of which is in play. A direct
/// `zones::move_to_zone` would skip the lifecycle and make the assertions below
/// vacuous.
fn destroy_via_removal_spell(runner: &mut GameRunner, removal: ObjectId, victim: ObjectId) {
    runner.cast(removal).target_object(victim).commit();
    runner.advance_until_stack_empty();
}

/// The durations recorded on `id`'s casting permissions, in grant order.
fn recorded_permission_durations(runner: &GameRunner, id: ObjectId) -> Vec<Option<Duration>> {
    runner.state().objects[&id]
        .casting_permissions
        .iter()
        .map(|permission| permission.lifetime().duration.cloned())
        .collect()
}

/// Drive resolution to a stable point, declining every optional-effect prompt
/// that appears. Returns whether any prompt was offered at all — the decline is
/// what the player did in the field report, and it is what destroyed the grant.
fn settle_declining_optionals(runner: &mut GameRunner) -> bool {
    let mut saw_optional = false;
    loop {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("declining an offered optional effect must be accepted");
            }
            _ => break,
        }
    }
    saw_optional
}

/// CR 611.2a: "for as long as you control this creature" states the permission's
/// own lifetime, so resolution offers NO choice and the exiled card stays
/// castable — and the permission is recorded carrying that stated lifetime.
///
/// DISCRIMINATING ASSERTIONS: `!saw_optional`, `can_cast(exiled)`, and the
/// recorded `Duration::WhileControllingHost` — Gwen Stacy prints "for as long
/// as you control", the wording this change splits off. Reverting the
/// `with_clause_duration` routing leaves `Effect::CastFromZone.duration` empty,
/// `is_lingering_cast_from_zone` stops matching, the clause's "may" becomes
/// `AbilityDefinition.optional`, and all three flip — a prompt appears, and
/// declining it strands the card in exile with no permission at all.
///
/// Positive reach-guard: `zone_of(exiled) == Exile` proves the enters trigger
/// really resolved, so a green run is not a vacuous no-op.
///
/// NOT PROVEN HERE: that the recorded `WhileControllingHost` is ever *enforced*.
/// That is the subject of `host_departure_revokes_the_lasting_cast_permission`
/// and `host_departure_revokes_the_lasting_land_play_companion` below; this
/// test stops while the host is still on the battlefield on purpose, so a
/// failure here localizes to the parse fix and not to the lifecycle.
#[test]
fn lasting_play_permission_is_not_a_declinable_resolution_offer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let impulse_card = scenario
        .add_spell_to_library_top(P0, "Library Top Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let gwen = scenario
        .add_creature_to_hand_from_oracle(P0, "Gwen Stacy", 2, 2, GWEN_STACY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(gwen).commit();
    let saw_optional = settle_declining_optionals(&mut runner);

    // Reach-guards: the enters trigger resolved, and the stated lifetime holds.
    assert_eq!(
        zone_of(&runner, impulse_card),
        Zone::Exile,
        "Gwen Stacy's enters trigger must exile the top card of the library"
    );
    assert_eq!(
        zone_of(&runner, gwen),
        Zone::Battlefield,
        "Gwen Stacy must be on the battlefield — the permission's stated lifetime"
    );

    assert!(
        !saw_optional,
        "CR 611.2a: a permission that states its own lifetime is not a CR 608.2d \
         resolution-time choice, so no prompt may be offered"
    );
    assert!(
        can_cast(&runner, impulse_card),
        "the exiled card must be castable from exile through the standing permission"
    );
    assert_eq!(
        recorded_permission_durations(&runner, impulse_card),
        vec![
            Some(Duration::WhileControllingHost),
            Some(Duration::WhileControllingHost)
        ],
        "CR 601.3 + CR 305.1 + CR 611.2b: both halves of a `mode: Play` grant (the spell \
         permission and its land companion) must carry the stated lifetime, and \
         \"for as long as you CONTROL ~\" is the control-bound reading"
    );
}

/// A second member of the same class, reached by a different route: a TARGETED
/// graveyard exile rather than a top-of-library impulse, with the "it" anaphor
/// rather than "that card". Same stated lifetime, same defect before the fix.
///
/// DISCRIMINATING ASSERTIONS: `!saw_optional` and `can_cast(exiled)`. Both flip
/// on revert — the promoted "may" pauses resolution and the decline destroys the
/// grant.
///
/// Positive reach-guard: the targeted card moved Graveyard -> Exile, so the
/// permission clause was reached.
#[test]
fn lasting_play_permission_covers_the_targeted_graveyard_exile_member() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let buried = scenario
        .add_spell_to_graveyard(P0, "Buried Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let victor = scenario
        .add_creature_to_hand_from_oracle(P0, "Victor Mancha, Runaway", 3, 3, VICTOR_MANCHA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(victor).commit();
    let saw_optional = settle_declining_optionals(&mut runner);

    // Reach-guards: the enters trigger resolved and Victor is still out.
    assert_eq!(
        zone_of(&runner, buried),
        Zone::Exile,
        "Victor Mancha's enters trigger must exile the targeted graveyard card"
    );
    assert_eq!(
        zone_of(&runner, victor),
        Zone::Battlefield,
        "Victor Mancha must be on the battlefield — the permission's stated lifetime"
    );

    assert!(
        !saw_optional,
        "CR 611.2a: \"you may play it for as long as you control Victor Mancha\" states the \
         permission's lifetime — no CR 608.2d resolution-time choice may be offered"
    );
    assert!(
        can_cast(&runner, buried),
        "the exiled card must be castable from exile through the standing permission"
    );
}

/// Collect every `CastFromZone` node in an ability chain, in walk order.
fn cast_from_zone_nodes(a: &AbilityDefinition, out: &mut Vec<AbilityDefinition>) {
    if matches!(&*a.effect, Effect::CastFromZone { .. }) {
        out.push(a.clone());
    }
    if let Some(sub) = &a.sub_ability {
        cast_from_zone_nodes(sub, out);
    }
    if let Some(els) = &a.else_ability {
        cast_from_zone_nodes(els, out);
    }
}

/// CR 608.2c: "later text on the card may modify the meaning of earlier text" —
/// "the rest of those exiled cards" means every card this `ExileFromTopUntil`
/// exiled EXCEPT the hit the permission covers, whether or not that permission
/// is a resolution-time offer.
///
/// This pins the second half of the fix. Removing the spurious `optional` (the
/// first half) would otherwise have silently disarmed the Jodah bottom-cleanup
/// rewrite, because `is_exile_until_cast_bottom_cleanup` was consulted only for
/// an `optional` cast — the cleanup would fall back to the parser-default
/// `TrackedSet` selector with `count: 1`, which no longer denotes "the rest".
///
/// DISCRIMINATING ASSERTIONS: the cleanup's `target` and `count`. Reverting the
/// decoupling turns them into `TargetFilter::TrackedSet { id: 0 }` and
/// `QuantityExpr::Fixed { value: 1 }` — measured, not assumed.
/// `else_ability.is_none()` is narrower: it pins the inner `if chain.optional`
/// guard alone (a wholesale revert also leaves it `None`), and it says that a
/// standing permission has no decline branch (CR 608.2d), so the duplicate copy
/// the old code stashed for the chain scanners must be gone.
///
/// Positive reach-guard: three chapter chains are found and each carries a
/// `CastFromZone` with `optional == false` — the fix's first half ran, which is
/// exactly the precondition that used to disarm the rewrite.
///
/// SCOPE — this is a PARSE assertion, deliberately, and here is what it does NOT
/// say. It does not assert that a card moves: verified against the live
/// deployment of `main`, The Day of the Doctor leaves the exiled remainder in
/// exile at runtime BOTH with and without this change, so the delivery of this
/// cleanup is independently broken and this diff neither causes nor repairs it.
/// The pin exists so that the correct instruction keeps being emitted.
///
/// It also pins ONE of the two call sites. The Day of the Doctor reaches
/// `assembly.rs`'s `normalize_linked_exile_cast_pair`; the sibling site in
/// `sequence.rs` (`append_definition_to_sub_chain`) is edited for consistency
/// with no demonstrated reachable input. Four of its five callers build the
/// trailing definition themselves and none produces the
/// `PutAtLibraryPosition { target: TrackedSet | TrackedSetFiltered }` shape the
/// gate requires (`sequence.rs:4877` builds `TargetFilter::Any`; the others pass
/// `ChangeZoneAll` / `GrantCastingPermission` bodies). The fifth
/// (`sequence.rs:4850`) forwards a pre-existing tail whose shape is NOT under
/// the caller's control, but it passes a `ChangeZone` as the chain head, and the
/// gate's first check demands an `ExileFromTopUntil { NextMatches }` head
/// (`lower.rs:930-938`) — so it short-circuits whatever the tail is. Leaving the
/// two sites answering the same question differently would be the worse defect,
/// so the edit stays and is named here rather than tested.
#[test]
fn the_rest_of_those_exiled_cards_survives_the_removed_optionality() {
    let parsed = parse_oracle_text(
        DAY_OF_THE_DOCTOR,
        "The Day of the Doctor",
        &[],
        &["Enchantment".to_string()],
        &["Saga".to_string()],
    );

    let mut nodes = vec![];
    for ability in &parsed.abilities {
        cast_from_zone_nodes(ability, &mut nodes);
    }
    for trigger in &parsed.triggers {
        if let Some(execute) = &trigger.execute {
            cast_from_zone_nodes(execute, &mut nodes);
        }
    }

    // Reach-guard: chapters I, II and III each lower to their own chain.
    assert_eq!(
        nodes.len(),
        3,
        "the I, II, III chapter ability must lower to one cast-from-zone chain per chapter"
    );

    let expected_target = TargetFilter::And {
        filters: vec![
            TargetFilter::ExiledBySource,
            TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::DistinctFrom {
                    reference: Box::new(TargetFilter::ParentTarget),
                },
            ])),
        ],
    };

    for (chapter, node) in nodes.iter().enumerate() {
        assert!(
            !node.optional,
            "chapter {} — a stated-lifetime permission is not a resolution-time choice",
            chapter + 1
        );
        assert!(
            node.else_ability.is_none(),
            "chapter {} — a standing permission has no decline branch, so no duplicate \
             cleanup may be stashed as one (CR 608.2d)",
            chapter + 1
        );
        let cleanup = node
            .sub_ability
            .as_deref()
            .expect("the bottom cleanup must hang under the cast node");
        let Effect::PutAtLibraryPosition {
            target,
            count,
            position,
        } = &*cleanup.effect
        else {
            panic!("chapter {} — expected the bottom cleanup", chapter + 1);
        };
        assert_eq!(*position, LibraryPosition::Bottom);
        assert_eq!(
            *target,
            expected_target,
            "chapter {} — \"the rest\" is every card exiled by this source EXCEPT the hit",
            chapter + 1
        );
        assert_eq!(
            *count,
            QuantityExpr::Fixed { value: 0 },
            "chapter {} — count 0 is the resolver's \"every matching card\" placeholder",
            chapter + 1
        );
    }
}

/// CR 611.2a + CR 400.7: the stated lifetime is not decoration — when the named
/// permanent leaves the battlefield the permission ENDS. Gwen Stacy's grant
/// lasts "for as long as you control this creature"; the parser maps that
/// wording to `Duration::WhileControllingHost` (pinned by
/// `parser::oracle_nom::duration::test_the_three_host_lifetime_wordings_do_not_collapse`),
/// and `Duration::ends_when_host_leaves_play` is what routes both host readings
/// through the battlefield-exit pass.
///
/// SCOPE: this test covers the BATTLEFIELD-EXIT leg only. The control-change leg
/// of the same duration — CR 611.2b's Master Thief case, where the permanent
/// stays on the battlefield but changes hands — is
/// `control_change_revokes_the_lasting_cast_permission` below. Keeping them
/// apart is deliberate: each names one pass, so a failure localizes.
///
/// DISCRIMINATING ASSERTIONS: `!can_cast(exiled)` and the now-empty permission
/// list, both taken AFTER Gwen dies. Before this fix the exiled card stayed
/// castable forever: `layers::prune_host_left_effects` retains only
/// `state.transient_continuous_effects`, and `ExileWithAltCost` carried no
/// granting-source id to prune against. Reverting the `source_id` stamp in
/// `record_lingering_permissions` restores the permission and fails this test
/// (measured; the land-half stamp likewise fails the companion test below).
/// The `prune_host_left_casting_permissions` call in `zones` is NOT what this
/// test discriminates — with the stamp intact, the continuous lapse pass
/// revokes the stale control-bound grant even without the exit hook. The exit
/// hook's own discriminating coverage is the unit test
/// `an_event_deadline_permission_ends_only_at_the_hosts_battlefield_exit`,
/// on the event-deadline reading that the lapse pass deliberately skips.
///
/// Positive reach-guards, all three needed for a green run to mean anything:
/// the card reached exile, it WAS castable while Gwen lived, and Gwen actually
/// reached the graveyard through the removal spell.
#[test]
fn host_departure_revokes_the_lasting_cast_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let impulse_card = scenario
        .add_spell_to_library_top(P0, "Library Top Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let gwen = scenario
        .add_creature_to_hand_from_oracle(P0, "Gwen Stacy", 2, 2, GWEN_STACY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Removal", true, DESTROY_TARGET_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(gwen).commit();
    settle_declining_optionals(&mut runner);

    // Reach-guard 1 + 2: the grant exists and is live while the host is out.
    assert_eq!(
        zone_of(&runner, impulse_card),
        Zone::Exile,
        "reach-guard: Gwen Stacy's enters trigger must exile the top card"
    );
    assert!(
        can_cast(&runner, impulse_card),
        "reach-guard: the exiled card must be castable WHILE Gwen is on the battlefield — \
         otherwise the post-death assertion below proves nothing"
    );

    destroy_via_removal_spell(&mut runner, removal, gwen);

    // Reach-guard 3: the host really left through the production path.
    assert_eq!(
        zone_of(&runner, gwen),
        Zone::Graveyard,
        "reach-guard: the removal spell must actually kill Gwen Stacy"
    );

    assert!(
        !can_cast(&runner, impulse_card),
        "CR 611.2a: 'for as long as you control this creature' ends when that creature \
         leaves the battlefield — the exiled card must no longer be castable. \
         gwen={gwen:?} exiled={impulse_card:?} left_over={:#?}",
        runner.state().objects[&impulse_card].casting_permissions
    );
    assert_eq!(
        recorded_permission_durations(&runner, impulse_card),
        Vec::<Option<Duration>>::new(),
        "CR 611.2a + CR 400.7: both halves of the grant are revoked by the departing host, \
         leaving no casting permission behind"
    );
}

/// CR 305.1: the same grant's LAND half. "You may play that card" authorizes a
/// land through `GameAction::PlayLand`, a different action than the spell half,
/// carried by a separate `CastingPermission::PlayFromExile` companion. Revoking
/// only one half would leave a land playable from exile after its granting
/// permanent died.
///
/// DISCRIMINATING ASSERTION: `!can_play_land(exiled)` after Gwen dies, guarded
/// by the same assertion returning true before she does.
#[test]
fn host_departure_revokes_the_lasting_land_play_companion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let top_land = scenario.add_card_to_library_top(P0, "Library Top Land");
    let gwen = scenario
        .add_creature_to_hand_from_oracle(P0, "Gwen Stacy", 2, 2, GWEN_STACY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Removal", true, DESTROY_TARGET_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    // The library-top card is a bare object; make it a land in BOTH the live and
    // the base type sets so the layer rebuild cannot revert it (the scenario
    // builders have no library-top land constructor).
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&top_land)
            .expect("library top card present");
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    runner.cast(gwen).commit();
    settle_declining_optionals(&mut runner);

    assert_eq!(
        zone_of(&runner, top_land),
        Zone::Exile,
        "reach-guard: Gwen Stacy's enters trigger must exile the top card"
    );
    assert!(
        can_play_land(&runner, top_land),
        "reach-guard: CR 305.1 — the exiled land must be playable WHILE Gwen is on the \
         battlefield, or the post-death assertion below proves nothing"
    );

    destroy_via_removal_spell(&mut runner, removal, gwen);

    assert_eq!(
        zone_of(&runner, gwen),
        Zone::Graveyard,
        "reach-guard: the removal spell must actually kill Gwen Stacy"
    );
    assert!(
        !can_play_land(&runner, top_land),
        "CR 305.1 + CR 611.2a: the land companion of the grant ends with its host, so no \
         PlayLand action may remain"
    );
}

/// Verbatim Oracle text (`client/public/card-data.json`, key
/// `nashi, moon sage's scion`).
const NASHI: &str =
    "Ninjutsu {3}{B} ({3}{B}, Return an unblocked attacker you control to hand: Put this card \
     onto the battlefield from your hand tapped and attacking.)\n\
     Whenever Nashi deals combat damage to a player, exile the top card of each player's \
     library. Until end of turn, you may play one of those cards. If you cast a spell this way, \
     pay life equal to its mana value rather than paying its mana cost.";

/// Verbatim Oracle text (`client/public/card-data.json`, key `act of treason`).
///
/// Chosen over the instant-speed Word of Seizing because that card's Split
/// second clause lowers to `Effect::Unimplemented`, which swallows the
/// control-gain: the test would then pass or fail on a parser gap unrelated to
/// what it asserts. Act of Treason is a sorcery, so the steal happens on the
/// opponent's own turn — which also makes the permission survive a full turn
/// boundary before the control change ends it.
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
     creature. It gains haste until end of turn. (It can attack and {T} this turn.)";

/// The one shape the parser can emit that no prune owned: a play permission
/// whose stated duration names the UNTAP step. No printed card prints it —
/// measured over the parsed corpus (the card-data snapshot ages separately from
/// the code, so no count is pinned here) — so the wording is
/// assembled here from two productions the grammar already has: the
/// turn-agnostic step deadline ("until the next untap step",
/// `oracle_nom::duration::parse_until_next_step`) and an impulse play grant.
const UNTAP_SCOPED_GRANT: &str = "When ~ enters, exile the top card of your library. Until the \
     next untap step, you may play that card.";

/// CR 118.9 + CR 611.2a: **the non-mana alternative-cost form kept its stated
/// lifetime.**
///
/// Nashi, Moon Sage's Scion is the printed card that reaches this shape: its
/// combat-damage trigger states BOTH a lifetime ("Until end of turn, you may
/// play one of those cards") and a non-mana alternative cost ("pay life equal to
/// its mana value rather than paying its mana cost"). The alternative cost
/// routes the grant into `CastingPermission::ExileWithAltAbilityCost`, which
/// carried no `duration` field at all — so the stated lifetime was dropped where
/// the permission was built, and the exiled cards stayed playable for the rest
/// of the game.
///
/// DISCRIMINATING ASSERTION: `!can_cast(exiled)` after the turn ends, guarded by
/// the same call returning true during the turn. Removing `duration` from the
/// grant in `cast_from_zone::record_lingering_permissions` restores the
/// indefinite permission and fails this test; so does removing
/// `ExileWithAltAbilityCost` from `CastingPermission::lifetime`, because the
/// cleanup seam then reads `duration: None` and retains the grant.
///
/// The recorded-shape assertion is deliberately separate from the behavioural
/// one: it pins that this really is the alternative-ability-cost variant and not
/// its mana-cost sibling, so a future re-routing of the parse cannot make the
/// expiry assertion pass through a different code path.
#[test]
fn stated_lifetime_survives_the_non_mana_alternative_cost_form() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nashi = scenario
        .add_creature_from_oracle(P0, "Nashi, Moon Sage's Scion", 2, 3, NASHI)
        .id();
    let p0_top = scenario
        .add_spell_to_library_top(P0, "P0 Library Top", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_top = scenario
        .add_spell_to_library_top(P1, "P1 Library Top", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(nashi, engine::game::combat::AttackTarget::Player(P1))])
        .expect("declaring Nashi as an attacker must be accepted");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        let _ = runner.declare_blockers(&[]);
    }
    let _ = runner.combat_damage();
    settle_declining_optionals(&mut runner);
    // CR 307.1: the exiled card is a sorcery, so the cast option can only
    // surface in a main phase — advance out of the combat step the trigger
    // resolved in before asking whether it is castable.
    runner.advance_to_phase(Phase::PostCombatMain);

    // Reach-guard 1: the combat-damage trigger resolved and exiled both cards.
    assert_eq!(
        zone_of(&runner, p0_top),
        Zone::Exile,
        "reach-guard: Nashi's combat-damage trigger must exile the top card of each library"
    );
    assert_eq!(zone_of(&runner, p1_top), Zone::Exile);

    // Reach-guard 2: the grant really is the NON-MANA alternative-cost form,
    // and it carries the stated lifetime.
    let permissions = &runner.state().objects[&p0_top].casting_permissions;
    assert!(
        permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::ExileWithAltAbilityCost { .. })),
        "reach-guard: CR 118.9 — \"pay life equal to its mana value rather than paying its mana \
         cost\" must route the grant into the non-mana alternative-cost permission, \
         got {permissions:#?}"
    );
    assert_eq!(
        recorded_permission_durations(&runner, p0_top),
        vec![Some(Duration::UntilEndOfTurn)],
        "CR 611.2a: \"Until end of turn, you may play one of those cards\" states the \
         permission's lifetime, which the non-mana alternative-cost form must carry"
    );

    // Reach-guard 3: the permission is live while the stated window is open.
    assert!(
        can_cast(&runner, p0_top),
        "reach-guard: the exiled card must be castable during the turn the grant was made — \
         otherwise the post-cleanup assertion below proves nothing"
    );

    runner.advance_to_upkeep();

    assert!(
        !can_cast(&runner, p0_top),
        "CR 514.2: an \"until end of turn\" permission ends at that turn's cleanup step, \
         whatever cost the grant asks for. left_over={:#?}",
        runner.state().objects[&p0_top].casting_permissions
    );
    assert_eq!(
        recorded_permission_durations(&runner, p0_top),
        Vec::<Option<Duration>>::new(),
        "CR 514.2: the expired grant is removed, not merely made unusable"
    );
}

/// CR 500.4 + CR 611.2a: **an untap-step lifetime is enforced at the untap
/// step.**
///
/// CR 500.4 is the authority for the expiry itself — "As a step or phase
/// begins, if there are effects that last until that step or phase, those
/// effects expire." CR 502.3 is the untap turn-based action and says nothing
/// about effects ending; it is descriptive context for WHICH step this is.
///
/// `oracle_nom::duration::step_deadline_scope` pairs the untap step with a
/// runtime scope, so the parser can emit
/// `Duration::UntilNextStepOf { step: Untap, .. }` onto a play permission. No
/// prune owned that shape: `prune_controller_untap_step_effects` — the only pass
/// the untap transition invoked — retains `state.transient_continuous_effects`,
/// and a casting permission does not live there. The permission therefore never
/// expired.
///
/// DISCRIMINATING ASSERTION: `!can_cast(exiled)` after the next untap step,
/// guarded by the same call returning true before it. Removing the
/// `PermissionSeam::UntapStep` arm from `PermissionSeam::for_step` makes the
/// duration unenforceable, the grant site then refuses it outright, and the
/// reach-guard above fails instead — either way the test reds.
///
/// `PlayerScope::AnyTurn`: "the next untap step" names a step without naming
/// whose it is, so it ends at the FIRST untap step to occur (CR 500.4) — here
/// the opponent's, one turn later.
#[test]
fn untap_step_lifetime_expires_the_play_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let impulse_card = scenario
        .add_spell_to_library_top(P0, "Library Top Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let granter = scenario
        .add_creature_to_hand_from_oracle(P0, "Untap Scoped Granter", 1, 1, UNTAP_SCOPED_GRANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(granter).commit();
    settle_declining_optionals(&mut runner);

    assert_eq!(
        zone_of(&runner, impulse_card),
        Zone::Exile,
        "reach-guard: the enters trigger must exile the top card"
    );
    assert_eq!(
        recorded_permission_durations(&runner, impulse_card),
        vec![Some(Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: engine::types::ability::PlayerScope::AnyTurn,
        })],
        "reach-guard: the grant must carry the untap-step lifetime this test is about"
    );
    assert!(
        can_cast(&runner, impulse_card),
        "reach-guard: the exiled card must be castable before the next untap step"
    );

    // The next untap step is the opponent's, one turn on; `advance_to_upkeep`
    // crosses it.
    runner.advance_to_upkeep();

    assert!(
        !can_cast(&runner, impulse_card),
        "CR 500.4 + CR 611.2a: a permission stated to last until the next untap step ends \
         there. left_over={:#?}",
        runner.state().objects[&impulse_card].casting_permissions
    );
}

/// CR 611.2b: **continued CONTROL, not merely continued presence.**
///
/// "For as long as you control ~" ends when another player gains control of the
/// permanent, with the permanent still on the battlefield — the case CR 611.2b
/// works through in its own Master Thief example. The parser used to map that
/// wording onto `Duration::UntilHostLeavesPlay`, a battlefield-exit test, so a
/// control change ended nothing and the former controller kept the permission.
///
/// DISCRIMINATING ASSERTIONS: the recorded permission list is non-empty before
/// the steal and EMPTY after it, with `zone_of(gwen) == Battlefield` afterwards
/// — which is what separates this from the host-departure test: the host never
/// leaves, so the battlefield-exit pass cannot be what revoked the permission.
/// Reverting the parser to `UntilHostLeavesPlay` leaves the permission live and
/// fails the post-steal assertion; removing the `WhileControllingHost` arm from
/// `prune_lapsed_host_bound_casting_permissions` does the same.
///
/// The post-steal assertion reads the recorded permission rather than the
/// legal-action list ON PURPOSE, for two independent reasons — either alone
/// would make a `!can_cast` check there green with the bug still present:
///
///   * CR 307.1: "A player who has priority may cast a sorcery card from their
///     hand during a main phase of THEIR turn when the stack is empty." Act of
///     Treason is a sorcery, so the steal lands on P1's turn, and the exiled
///     card is a sorcery too. No card on this board grants an exception (no
///     "as though it had flash" / Vedalken Orrery effect is in play).
///   * `legal_actions` enumerates for the player who currently holds priority,
///     which after the steal resolves is P1 — P0's options are not in the list
///     at all, whatever their timing. Making the exiled card an instant would
///     therefore not repair the check either.
///
/// The legal-action check on P0's own turn, before the steal, is the
/// behavioural anchor that the permission was real to begin with.
///
/// CR 702.26f is covered by the same authority
/// (`replacement::controller_controls_source_gate`) but is not exercised here.
#[test]
fn control_change_revokes_the_lasting_cast_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 104.3c: this test crosses into P1's turn, so P1 must have a card to
    // draw — an empty library would end the game before the steal is cast.
    for i in 0..3 {
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
    }
    let impulse_card = scenario
        .add_spell_to_library_top(P0, "Library Top Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let gwen = scenario
        .add_creature_to_hand_from_oracle(P0, "Gwen Stacy", 2, 2, GWEN_STACY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Treason", false, ACT_OF_TREASON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(gwen).commit();
    settle_declining_optionals(&mut runner);

    assert_eq!(
        zone_of(&runner, impulse_card),
        Zone::Exile,
        "reach-guard: Gwen Stacy's enters trigger must exile the top card"
    );
    assert!(
        can_cast(&runner, impulse_card),
        "reach-guard: the exiled card must be castable while P0 still controls Gwen"
    );

    // CR 307.1: Act of Treason is a sorcery, so P1 casts it in their own main
    // phase. Crossing the turn boundary first is a fourth reach-guard: the
    // permission must still be live at that point, which rules out any
    // turn/step-scoped prune as the thing that removes it below.
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach-guard: the steal must happen on P1's own turn"
    );
    // CR 307.1: a sorcery in exile is not castable during the OPPONENT's turn
    // whatever permission it carries, so the cross-turn guard reads the recorded
    // permission rather than the legal-action list — the legal-action check
    // above, taken on P0's own turn, is the behavioural anchor.
    assert_eq!(
        recorded_permission_durations(&runner, impulse_card),
        vec![
            Some(Duration::WhileControllingHost),
            Some(Duration::WhileControllingHost)
        ],
        "reach-guard: the permission must survive the turn boundary — otherwise a turn-scoped \
         prune, not the control change, is what the final assertion measures"
    );

    runner.cast(steal).target_object(gwen).commit();
    runner.advance_until_stack_empty();
    settle_declining_optionals(&mut runner);

    // Reach-guards: control really moved, and the host never left the battlefield.
    assert_eq!(
        runner.state().objects[&gwen].controller,
        P1,
        "reach-guard: Act of Treason must actually move control of Gwen Stacy"
    );
    assert_eq!(
        zone_of(&runner, gwen),
        Zone::Battlefield,
        "reach-guard: Gwen Stacy must still be on the battlefield — otherwise this test is a \
         second copy of the host-departure test"
    );

    assert_eq!(
        recorded_permission_durations(&runner, impulse_card),
        Vec::<Option<Duration>>::new(),
        "CR 611.2b: \"for as long as you control this creature\" ends when control changes, \
         even though the creature is still on the battlefield — both halves of the grant are \
         revoked. left_over={:#?}",
        runner.state().objects[&impulse_card].casting_permissions
    );
}

/// Verbatim Oracle text (`client/public/card-data.json`, key `disenchant`).
const DISENCHANT: &str = "Destroy target artifact or enchantment.";

/// Verbatim Oracle text (`client/public/card-data.json`, key `master thief`).
const MASTER_THIEF: &str =
    "When this creature enters, gain control of target artifact for as long as you control this \
     creature.";

/// CR 611.2b: **a lifetime that is already over when the grant is made must
/// produce no permission at all.**
///
/// "If the 'for as long as' duration never starts, the effect does nothing.
/// Similarly, if that duration ends before the moment the effect would first be
/// applied … the effect does nothing. It doesn't start and immediately stop
/// again, and it doesn't last forever."
///
/// The battlefield-exit pass cannot deliver that, and this is the reachable hole
/// it leaves: `layers::prune_host_left_casting_permissions` runs from
/// `zones::apply_zone_exit_cleanup`, so if the host leaves BEFORE the granting
/// ability resolves, the pass has already run and the permission it would have
/// revoked does not exist yet. Nothing revokes it afterwards — the permission
/// then lasts forever, which is the outcome CR 611.2b names in its last
/// sentence.
///
/// Both printed cards in this class expose the window. The Day of the Doctor is
/// used here because its chapter trigger goes on the stack as the Saga enters,
/// giving an ordinary priority window to destroy it; Intet, the Dreamer opens
/// the same window with its "you may pay {2}{U}" combat trigger.
///
/// DISCRIMINATING ASSERTION: the exiled card carries NO casting permission after
/// the chapter trigger resolves with its host already destroyed. Disabling the
/// presence arm of `layers::prune_lapsed_host_bound_casting_permissions`
/// restores the indefinite permission and fails it. The battlefield-exit pass
/// alone cannot make this test pass, which is why the presence arm lives in
/// the continuous pass.
///
/// Positive reach-guard: a card actually reached exile, so the chapter trigger
/// did resolve and did grant.
#[test]
fn a_lifetime_already_over_at_grant_time_produces_no_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Chapter I exiles "until you exile a legendary card"; a legendary top card
    // makes that a one-card exile.
    let legend = scenario
        .add_spell_to_library_top(P0, "Legendary Top Card", false)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();
    // The Saga's chapter text must be parsed with Enchantment/Saga typing, so
    // the type is set before `from_oracle_text` runs.
    let saga = scenario
        .add_spell_to_hand_from_oracle(P0, "The Day of the Doctor", false, "")
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(DAY_OF_THE_DOCTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    // Resolve the Saga ONLY. Its chapter ability then sits on the stack
    // (CR 714.3a + CR 714.2b: the Saga enters with a lore counter, and the
    // chapter ability triggers), which is the window this test is about.
    runner.cast(saga).commit();
    runner.pass_both_players();
    assert_eq!(
        zone_of(&runner, saga),
        Zone::Battlefield,
        "reach-guard: the Saga must have resolved, with its chapter ability still waiting"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "reach-guard: the chapter ability must be ON THE STACK — otherwise the removal below \
         lands after the grant and this test is a copy of the host-departure test"
    );

    // Destroy the Saga WHILE its chapter ability waits. The battlefield-exit
    // pass runs here, before the permission it would revoke exists.
    runner.cast(removal).target_object(saga).commit();
    runner.advance_until_stack_empty();
    let saw_optional = settle_declining_optionals(&mut runner);

    assert_eq!(
        zone_of(&runner, saga),
        Zone::Graveyard,
        "reach-guard: the removal spell must actually destroy the Saga"
    );
    // Without this the test is vacuously green under a PARTIAL revert: undo the
    // `with_clause_duration` routing and the chapter becomes `optional`,
    // `settle_declining_optionals` declines it, no permission is ever created,
    // and the empty-permission assertion below passes for the wrong reason.
    assert!(
        !saw_optional,
        "reach-guard: the grant path must have run — a stated-lifetime permission is not a \
         CR 608.2d resolution-time choice, so no prompt may appear"
    );
    // Reach-guard: the chapter ability still resolved and still exiled — an
    // ability on the stack exists independently of its source (CR 113.7a).
    assert_eq!(
        zone_of(&runner, legend),
        Zone::Exile,
        "reach-guard: chapter I must still exile until the legendary card"
    );
    assert_eq!(
        recorded_permission_durations(&runner, legend),
        Vec::<Option<Duration>>::new(),
        "CR 611.2a: the Saga's presence is the stated lifetime, so no permission may outlive it. \
         left_over={:#?}",
        runner.state().objects[&legend].casting_permissions
    );
}

/// CR 611.2b: the split of "for as long as you control ~" from
/// `UntilHostLeavesPlay` is **not** scoped to casting permissions — it changes
/// the duration for every effect that wording produces, and the class is much
/// larger than the permission cards that motivated it.
///
/// This is the duration class of CR 611.2b's own example, run as a production
/// regression: Master Thief's "gain control of target artifact for as long as
/// you control this creature". The printed rule illustrates that wording from
/// the other end — a duration already over BEFORE the ability resolves, so the
/// effect does nothing — while this test drives the end that a live effect
/// reaches: Master Thief changes hands, the duration ends, control of the
/// artifact reverts. Before the split the wording lowered to
/// `UntilHostLeavesPlay`, a battlefield-exit test, and the thief kept the
/// artifact.
///
/// DISCRIMINATING ASSERTION: `artifact.controller` back to its owner after the
/// steal, guarded by it being the thief's controller before. Reverting the
/// parser arm leaves the artifact with P0 and fails it.
#[test]
fn control_bound_duration_ends_a_stolen_artifact_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..3 {
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
    }
    let loot = scenario.add_artifact_from_oracle(P1, "Loot", "").id();
    let thief = scenario
        .add_creature_to_hand_from_oracle(P0, "Master Thief", 2, 2, MASTER_THIEF)
        .with_mana_cost(ManaCost::zero())
        .id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Treason", false, ACT_OF_TREASON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(thief).commit();
    settle_declining_optionals(&mut runner);

    assert_eq!(
        runner.state().objects[&loot].controller,
        P0,
        "reach-guard: Master Thief's enters trigger must take the artifact"
    );

    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        runner.state().objects[&loot].controller,
        P0,
        "reach-guard: the grant must survive the turn boundary — otherwise a turn-scoped \
         expiry, not the control change, is what the final assertion measures"
    );

    runner.cast(steal).target_object(thief).commit();
    runner.advance_until_stack_empty();
    settle_declining_optionals(&mut runner);

    assert_eq!(
        runner.state().objects[&thief].controller,
        P1,
        "reach-guard: Act of Treason must actually move control of Master Thief"
    );
    assert_eq!(
        zone_of(&runner, thief),
        Zone::Battlefield,
        "reach-guard: Master Thief must still be on the battlefield — otherwise the old \
         `UntilHostLeavesPlay` semantics would satisfy the assertion below too"
    );
    assert_eq!(
        runner.state().objects[&loot].controller,
        P1,
        "CR 611.2b: \"for as long as you control this creature\" ends when control of that \
         creature changes, so the stolen artifact goes back — the Master Thief example from \
         the rule itself"
    );
}

/// Verbatim Oracle text (`client/public/card-data.json`, key `stolen goods`).
const STOLEN_GOODS: &str =
    "Target opponent exiles cards from the top of their library until they exile a nonland \
     card. Until end of turn, you may cast that card without paying its mana cost.";

/// The second free branch of the same class, reached through the OTHER printed
/// lifetime: "Until end of turn, you may cast that card without paying its mana
/// cost" (Stolen Goods). Several further cards print the same free grant; which
/// ones is measured in the double parse, not counted here.
///
/// DISCRIMINATING, and this is what the counter-probe MEASURED rather than what
/// the shape suggests: restoring `Some(DuringResolution)` fails this test at the
/// reach guard below — the exiled card is not in exile at all any more, because
/// the one-shot mechanism casts it as Stolen Goods resolves. There is no later
/// window to be offered one in.
///
/// The turn-boundary assertion is the POSITIVE COUNTER-DIRECTION: a permission
/// that is now lingering must still END where the card says it does (CR 514.2),
/// so over-suppression — a grant that outlives its printed turn — fails here too.
#[test]
fn a_free_until_end_of_turn_permission_is_exercised_at_a_later_priority_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 104.3c: the cleanup assertion below is made after a turn boundary, so
    // both players must survive their draw steps. Without this the run reaches
    // it in a `GameOver` state — the assertion stayed true (CR 514.2 prunes at
    // cleanup, before the boundary) but it was no longer measuring a live game.
    // Seeded BEFORE the exile target, because both library helpers prepend: the
    // card Stolen Goods must find has to end up on top of the filler.
    scenario.with_library_top(P0, &["P0 Filler A", "P0 Filler B", "P0 Filler C"]);
    scenario.with_library_top(P1, &["P1 Filler A", "P1 Filler B", "P1 Filler C"]);
    let loot = scenario
        .add_spell_to_library_top(P1, "Opponent Top Card", false)
        .with_mana_cost(ManaCost::zero())
        .id();
    let goods = scenario
        .add_spell_to_hand_from_oracle(P0, "Stolen Goods", false, STOLEN_GOODS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(goods).target_player(P1).commit();
    let saw_optional = settle_declining_optionals(&mut runner);

    assert_eq!(
        zone_of(&runner, loot),
        Zone::Exile,
        "reach guard: Stolen Goods must exile the opponent's top nonland card"
    );
    assert!(
        !saw_optional,
        "CR 611.2a: \"Until end of turn\" states the permission's lifetime, so resolution \
         offers no CR 608.2d choice"
    );
    assert!(
        can_cast(&runner, loot),
        "CR 117.1a: the exiled card must be castable at a later priority window"
    );
    assert_eq!(
        recorded_permission_durations(&runner, loot),
        vec![Some(Duration::UntilEndOfTurn)],
        "CR 611.2a: the grant must carry the printed turn-long lifetime"
    );

    // CR 514.2: the counter-direction — the permission must still expire.
    runner.advance_to_phase(Phase::Cleanup);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "reach guard: the prune must be measured in a live game, not after a player decked out"
    );
    assert!(
        recorded_permission_durations(&runner, loot).is_empty(),
        "CR 514.2: the turn-long permission must be pruned at the cleanup step, not outlive it"
    );
}

/// Verbatim Oracle text (`client/public/card-data.json`, key
/// `gale's redirection`).
const GALES_REDIRECTION: &str =
    "Exile target spell, then roll a d20 and add that spell's mana value.\n\
     1\u{2014}14 | You may cast the exiled card for as long as it remains exiled, and you may \
     spend mana as though it were mana of any color to cast that spell.\n\
     15+ | You may cast the exiled card without paying its mana cost for as long as it remains \
     exiled.";

/// The `Duration::ForAsLongAs` branch of the same class, end-to-end — the shape
/// the module doc above named as NOT covered: "you may cast the exiled card
/// WITHOUT PAYING ITS MANA COST for as long as it remains exiled" (Gale's
/// Redirection's 15+ row; the module doc above names the three further members
/// of that exact wording, none of which reaches the resolver).
///
/// DETERMINISTIC WITHOUT TOUCHING THE DIE. CR 202.3: the row is chosen by
/// `d20 + that spell's mana value`, so a countered spell of mana value 14 puts
/// every possible roll on the 15+ row. The die is rolled for real; the fixture
/// only removes the branch it could otherwise land on.
///
/// DISCRIMINATING, and MEASURED: restoring `Some(DuringResolution)` in
/// `with_lingering_duration` fails this test on `!saw_optional` — the clause's
/// "may" becomes an `AbilityDefinition.optional` prompt raised while Gale's
/// Redirection is still resolving. This is the row that pins the CR 608.2d
/// offer itself; its Stolen Goods sibling pins the other face of the same
/// defect, the card never staying in exile.
///
/// The zero-cost assertion is also the branch guard: the 1—14 row grants a
/// FULL-cost permission with a colour concession, so a fixture that drifted
/// onto it would fail here rather than pass vacuously.
///
/// Positive reach-guards: the countered spell really left the stack for EXILE,
/// and the assertion is made on the grant holder's own turn, where they hold
/// priority (CR 117.1a).
#[test]
fn a_free_for_as_long_as_exiled_lifetime_is_not_a_declinable_resolution_offer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fatty = scenario
        .add_creature_to_hand(P0, "Big Spell", 5, 5)
        .with_mana_cost(ManaCost::Cost {
            generic: 14,
            shards: vec![],
        })
        .id();
    let gale = scenario
        .add_spell_to_hand_from_oracle(P1, "Gale's Redirection", true, GALES_REDIRECTION)
        .with_mana_cost(ManaCost::zero())
        .id();
    for _ in 0..14 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    }
    // CR 104.3c: the assertion below is made on the grant holder's own turn, so
    // BOTH players must survive their draw steps rather than losing to an empty
    // library — the turn passes through the opponent's draw step to get there.
    scenario.with_library_top(P0, &["P0 Filler A", "P0 Filler B", "P0 Filler C"]);
    scenario.with_library_top(P1, &["P1 Filler A", "P1 Filler B", "P1 Filler C"]);
    let mut runner = scenario.build();

    runner.cast(fatty).commit();
    // CR 117.3c: the caster keeps priority after putting the spell on the stack;
    // the opponent's window to answer it opens only once it is passed.
    runner
        .act(GameAction::PassPriority)
        .expect("the active player must be able to pass priority with the spell on the stack");
    runner.cast(gale).target_object(fatty).commit();
    let saw_optional = settle_declining_optionals(&mut runner);

    assert_eq!(
        zone_of(&runner, fatty),
        Zone::Exile,
        "reach guard: Gale's Redirection must resolve and exile the targeted spell"
    );
    assert!(
        !saw_optional,
        "CR 611.2a: \"for as long as it remains exiled\" states the permission's own lifetime, \
         so resolution offers no CR 608.2d choice"
    );
    assert_eq!(
        recorded_permission_durations(&runner, fatty),
        vec![Some(Duration::ForAsLongAs {
            condition: engine::types::StaticCondition::Unrecognized {
                text: "it remains exiled".to_string(),
            },
        })],
        "CR 611.2b: the grant must carry the printed \"for as long as\" lifetime"
    );
    let free = runner.state().objects[&fatty]
        .casting_permissions
        .iter()
        .any(|p| matches!(p, CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()));
    assert!(
        free,
        "branch guard: mana value 14 forces the 15+ row, whose permission is the FREE one — \
         the 1—14 row grants a full-cost permission instead: {:?}",
        runner.state().objects[&fatty].casting_permissions
    );

    // CR 117.1a: hand priority to the grant holder, on their own turn.
    for _ in 0..4 {
        if runner.state().active_player == P1 {
            break;
        }
        runner.advance_to_phase(Phase::Cleanup);
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the assertion below must be made while the grant holder holds priority"
    );
    assert!(
        can_cast(&runner, fatty),
        "CR 117.1a: the exiled card must be castable at a later priority window through the \
         standing permission — and \"for as long as it remains exiled\" states no turn-based end, \
         so the turn boundary must not have revoked it"
    );
}
