//! The Will cycle's coordinated graveyard permission, END TO END.
//!
//! **What this suite is for.** The parser half of this class was measured green
//! by the coverage gate while delivering NOTHING to a player: the resolved
//! `Effect::CastFromZone` is not a channel any land-permission consumer reads, so
//! `casting::graveyard_lands_playable_by_permission` returned `[]` after
//! resolving the real Oracle text. Every row here therefore RESOLVES the card and
//! asks the production consumer what a player may actually do — no row asserts
//! parse shape alone, and no row hand-installs a permission.
//!
//! **The class.** "Until end of turn, you may play lands and cast spells from
//! your graveyard" (Yawgmoth's Will, Gaea's Will, Magus of the Will) is ONE
//! permission naming two actions (CR 116.2a: playing a land is a special action;
//! CR 601.2a: casting).
//!
//! **CR 611.2c is the design constraint, not a footnote.** A resolution-created
//! continuous effect that does not modify characteristics "modifies the rules of
//! the game, so it can affect objects that weren't affected when that continuous
//! effect began." That is why `d2` exists: a card milled AFTER the spell resolved
//! must still be covered. The card itself proves the reading — its second
//! sentence ("If a card would be put into your graveyard from anywhere this turn,
//! exile that card instead") is only meaningful if the first sentence reaches
//! cards that arrive later.
//!
//! **Stack size.** `parse_oracle_text` overflows the default 8 MB test stack and
//! prints a convincing PARTIAL negative on the way down rather than failing, so
//! every body that parses runs on 256 MB via `on_big_stack`.

use engine::game::casting::graveyard_lands_playable_by_permission;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Real Oracle text, verified against Scryfall.
const YAWGMOTHS_WILL: &str = "Until end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";

/// Gaea's Will arrives behind a Suspend line — a different ARRIVAL SHAPE for the
/// same sentence, so the grant must not key on being the first line.
const GAEAS_WILL: &str = "Suspend 4—{G}\nUntil end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";

fn on_big_stack<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .expect("spawn 256MB parser thread")
        .join()
        .expect("parser thread must not panic")
}

/// Stage a graveyard land, resolve `oracle` as a sorcery, and hand back the
/// runner plus the land's id.
///
/// Deliberately drives the REAL cast pipeline (`GameRunner::cast(..).resolve()`)
/// rather than hand-building a `ResolvedAbility`: the defect this suite exists to
/// catch lived precisely in the gap between "the AST looks right" and "resolution
/// installs something a consumer can see."
fn resolve_will(oracle: &'static str) -> (GameRunner, engine::types::ObjectId) {
    on_big_stack(move || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let land = scenario.add_land_to_graveyard(PlayerId(0), "Forest").id();
        let will = scenario
            .add_spell_to_hand_from_oracle(PlayerId(0), "Will", false, oracle)
            .id();
        let mut runner = scenario.build();
        let _ = runner.cast(will).resolve();
        (runner, land)
    })
}

/// THE discriminating row. Pre-change this returned `[]`.
#[test]
fn d1_resolving_the_will_makes_a_graveyard_land_playable() {
    let (runner, land) = resolve_will(YAWGMOTHS_WILL);

    let playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        playable.iter().any(|(object_id, _)| *object_id == land),
        "CR 116.2a: after Yawgmoth's Will resolves, the graveyard land must be \
         playable through the production permission consumer, got {playable:?}"
    );
}

/// CR 611.2c: the grant modifies the RULES, so it covers cards that reach the
/// graveyard after it began.
///
/// This is the row a per-object stamp fails. It is also the row the card itself
/// demands: the printed replacement clause ("if a card would be put into your
/// graveyard this turn, exile it instead") is only meaningful if the permission
/// reaches cards that arrive later in the turn.
#[test]
fn d2_a_land_reaching_the_graveyard_after_resolution_is_also_covered() {
    let (mut runner, staged_land) = resolve_will(YAWGMOTHS_WILL);

    // A SECOND land arrives in the graveyard only now — after the continuous
    // effect began.
    let late_land = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::CardId(4242),
        PlayerId(0),
        "Mountain".to_string(),
        Zone::Graveyard,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&late_land)
        .expect("late land must exist")
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Land);

    let playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        playable
            .iter()
            .any(|(object_id, _)| *object_id == late_land),
        "CR 611.2c: a rules-modifying permission covers objects that were not in \
         the zone when it began — the late-arriving land must be playable, got \
         {playable:?}"
    );
    // Reach-guard: the staged land is still covered, so d2 is testing the LATE
    // arrival rather than a wholesale re-grant.
    assert!(
        playable
            .iter()
            .any(|(object_id, _)| *object_id == staged_land),
        "reach-guard: the originally staged land must remain playable"
    );
}

/// CR 514.2: "all 'until end of turn' and 'this turn' effects end" at cleanup.
///
/// An unstated duration lasts until end of GAME (CR 611.2a), so a permission that
/// outlives its window is a worse failure than one that never parsed.
#[test]
fn d3_the_permission_ends_at_cleanup() {
    let (mut runner, land) = resolve_will(YAWGMOTHS_WILL);

    // Reach-guard FIRST: the grant must be live before the prune, or the
    // post-prune emptiness below would pass for the wrong reason.
    let before = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        before.iter().any(|(object_id, _)| *object_id == land),
        "reach-guard: the permission must be live before cleanup, got {before:?}"
    );

    engine::game::layers::prune_end_of_turn_effects(runner.state_mut());

    let after = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        !after.iter().any(|(object_id, _)| *object_id == land),
        "CR 514.2: the until-end-of-turn permission must not survive cleanup, got \
         {after:?}"
    );
}

/// MULTI-AUTHORITY: the same sentence behind a Suspend line must deliver
/// identically, proving the grant keys on the sentence rather than on being the
/// card's first line.
#[test]
fn d4_the_grant_survives_a_different_arrival_shape() {
    let (runner, land) = resolve_will(GAEAS_WILL);

    let playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        playable.iter().any(|(object_id, _)| *object_id == land),
        "CR 116.2a: Gaea's Will (Suspend line first) must deliver the same \
         permission, got {playable:?}"
    );
}

/// GUARD: the permission is bound to the GRANTEE, so it must not leak to an
/// opponent's graveyard.
///
/// The card says "**you** may play lands", and the consumer is keyed per player.
#[test]
fn g1_the_permission_does_not_reach_an_opponent() {
    let (mut runner, _land) = resolve_will(YAWGMOTHS_WILL);

    let opponent_land = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::CardId(4343),
        PlayerId(1),
        "Island".to_string(),
        Zone::Graveyard,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&opponent_land)
        .expect("opponent land must exist")
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Land);

    // REACH-GUARD FIRST: the grant must actually be live for the CASTER, or the
    // opponent emptiness below would pass for a failed delivery rather than for
    // the player binding under test.
    let caster_playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        caster_playable.iter().any(|(id, _)| *id == _land),
        "reach-guard: the caster must hold the permission, got {caster_playable:?}"
    );

    let opponent_playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(1));
    assert!(
        opponent_playable.is_empty(),
        "CR 611.2c: the grant is bound to the caster — an opponent must gain no \
         graveyard land permission, got {opponent_playable:?}"
    );
}

/// GUARD: a card whose grant states NO window must not be recovered.
///
/// CR 611.2a makes an unstated duration last until end of GAME, so copying an
/// absent window would synthesize a permanent permission. Shaman's Trance is the
/// real corpus card with this shape; its cast sibling lowers with
/// `duration: None`, and its filter is independently unfaithful
/// (`controller: You` against the printed "other players' graveyards").
#[test]
fn g2_a_grant_with_no_stated_window_is_not_delivered() {
    // PAIRED POSITIVE REACH-GUARD, run first. The only difference between this
    // fixture and the windowed one is the leading "Until end of turn, ", so if
    // the windowed form delivers and this one does not, the WINDOW is what the
    // row is measuring — not a parse failure, an unresolved cast, or a total
    // delivery regression, each of which would otherwise turn this row green.
    let (windowed_runner, windowed_land) = resolve_will(YAWGMOTHS_WILL);
    let windowed = graveyard_lands_playable_by_permission(windowed_runner.state(), PlayerId(0));
    assert!(
        windowed.iter().any(|(id, _)| *id == windowed_land),
        "reach-guard: the windowed twin of this sentence must deliver, or this \
         row proves nothing about the window, got {windowed:?}"
    );

    let (runner, land) = resolve_will("You may play lands and cast spells from your graveyard.");

    // Staging guard: the land really is in the graveyard, so an empty result
    // cannot be explained by the fixture never staging a candidate.
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "reach-guard: the staged land must be in the graveyard"
    );

    // The windowless form is owned by the STATIC parser, not this pass, so no
    // resolution-created permission may appear. (If the static path later starts
    // delivering this shape, that is a deliberate change and this row should be
    // revisited rather than silently relaxed.)
    let playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        !playable.iter().any(|(object_id, _)| *object_id == land),
        "CR 611.2a: a grant with no stated window must not be recovered as a \
         resolution-created permission, got {playable:?}"
    );
}

/// REGRESSION: the rewrite must SPLICE the cast node out, not truncate the chain.
///
/// Magus of the Will puts the whole card on ONE line, so the following sentence's
/// replacement ("If a card would be put into your graveyard from anywhere this
/// turn, exile that card instead") lowers as the cast sibling's own
/// `sub_ability`. An earlier revision replaced the head and set
/// `sub_ability = None`, which discarded that independent printed clause and
/// raised two `swallowed-clause` warnings — while Yawgmoth's Will, which prints
/// the same sentence on a SEPARATE line, was unaffected.
///
/// The one-line arrival shape is the one that loses text, which is exactly what a
/// chain-truncating rewrite hides. This row pins both halves: the permission is
/// delivered AND the replacement clause survives.
#[test]
fn g3_the_magus_replacement_clause_survives_the_rewrite() {
    const MAGUS: &str = "{2}{B}, {T}, Exile this creature: Until end of turn, you may play lands and cast spells from your graveyard. If a card would be put into your graveyard from anywhere this turn, exile that card instead.";

    let parsed = on_big_stack(move || {
        engine::parser::parse_oracle_text(
            MAGUS,
            "Magus of the Will",
            &[],
            &["Creature".to_string()],
            &[],
        )
    });

    // (i) THE REGRESSION. The parser must not silently drop the replacement
    // sentence; the swallow audit is the authority that notices when it does.
    assert!(
        parsed.parse_warnings.is_empty(),
        "the rewrite must not swallow the replacement clause, got {:?}",
        parsed.parse_warnings
    );

    // (ii) REACH-GUARD: the permission is actually delivered for this fixture, so
    // (i) cannot pass merely because the pass declined to fire at all.
    assert!(
        parsed
            .abilities
            .iter()
            .any(ability_grants_graveyard_permission),
        "reach-guard: Magus must still deliver the graveyard permission"
    );

    // (iii) The replacement tail is REATTACHED rather than merely present
    // somewhere: it must hang off the ability whose head is the delivered grant.
    assert!(
        parsed.abilities.iter().any(|ability| {
            ability_grants_graveyard_permission(ability) && ability.sub_ability.is_some()
        }),
        "the cast node must be spliced out and its tail reattached, not truncated"
    );
}

/// Does this ability's head install a `GraveyardCastPermission`?
///
/// Selects the grant by the mode it actually carries rather than by "some
/// `GenericEffect` exists", so a row cannot pass on an unrelated continuous
/// effect that happens to share the outer shape.
fn ability_grants_graveyard_permission(
    ability: &engine::types::ability::AbilityDefinition,
) -> bool {
    let engine::types::ability::Effect::GenericEffect {
        static_abilities, ..
    } = &*ability.effect
    else {
        return false;
    };
    static_abilities.iter().any(|static_def| {
        static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                engine::types::ability::ContinuousModification::GrantStaticAbility { definition }
                    if matches!(
                        definition.mode,
                        engine::types::statics::StaticMode::GraveyardCastPermission { .. }
                    )
            )
        })
    })
}
