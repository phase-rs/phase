//! Urianger Augurelt (FINAL FANTASY XIV, Scions & Spellcraft) ? Play Arcanum's
//! "Spells you cast this way cost {2} less to cast." rider.
//!
//! CR 601.2f: the rider is a cost modification scoped to the ONE grant that
//! states it, applied in the total-cost step with every other modifier ? all
//! increases before all reductions, mana component floored at {0}.
//! CR 608.2c: "this way" binds the rider to the immediately-preceding
//! instruction, which for Urianger is an `Effect::CastFromZone`, not a
//! parser-built `PlayFromExile` grant.
//! CR 305.1: a land played under the same `mode: Play` grant "is never a
//! spell", so the land half must not carry or consume the rider.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    CardPlayMode, CastCostModifier, CastingPermission, Duration, Effect, PlayFromExileProvenance,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;

const URIANGER_ORACLE: &str = "Whenever you play a land from exile or cast a spell from exile, \
you gain 2 life.\nDraw Arcanum — {T}: Look at the top card of your library. You may exile it \
face down.\nPlay Arcanum — {T}: Until end of turn, you may play cards exiled with Urianger \
Augurelt. Spells you cast this way cost {2} less to cast.";

fn generic_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// Parse Urianger's verbatim Oracle text and return Play Arcanum's effect.
///
/// Positive reach guard: the rider must have been ABSORBED into the
/// `CastFromZone` that states it ? if it lowered to a standalone clause instead,
/// the shape assertions here fail before any cost is measured.
fn play_arcanum_effect() -> Effect {
    let parsed = parse_oracle_text(
        URIANGER_ORACLE,
        "Urianger Augurelt",
        &[],
        &["Creature".to_string()],
        &["Elf".to_string(), "Advisor".to_string()],
    );
    let play_arcanum = parsed
        .abilities
        .iter()
        .find(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|text| text.starts_with("Play Arcanum"))
        })
        .expect("Urianger must expose a Play Arcanum activated ability");
    assert!(
        play_arcanum.sub_ability.is_none(),
        "the rider must fold into the grant, not trail it as a sub-ability: {:?}",
        play_arcanum.sub_ability
    );
    let effect = (*play_arcanum.effect).clone();
    let Effect::CastFromZone {
        target,
        mode,
        duration,
        cast_cost_modifier,
        ..
    } = &effect
    else {
        panic!("Play Arcanum must lower to CastFromZone, got {effect:?}");
    };
    // CR 607.2a + CR 406.6: "cards exiled WITH ~" is source-anchored — it reads
    // the source's durable exile ledger across resolutions. A `ParentTarget`
    // binding would resolve to nothing at runtime (no prior clause in this
    // chain produced the exile), so the grant would be a silent no-op.
    assert_eq!(*target, TargetFilter::ExiledBySource);
    // CR 305.1: "you may PLAY cards exiled with ~" authorizes lands too.
    assert_eq!(*mode, CardPlayMode::Play);
    // CR 611.2a: "Until end of turn".
    assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    // CR 601.2f: the rider, as a Reduce of {2} ? not a raise, not a bare cost.
    assert_eq!(
        *cast_cost_modifier,
        Some(CastCostModifier::reduce(ManaCost::generic(2))),
        "\"Spells you cast this way cost {{2}} less to cast.\" must fold into the grant"
    );
    effect
}

/// CR 607.2a + CR 406.6: record `card` as exiled WITH `source`. This is the
/// durable link Draw Arcanum's face-down exile leaves behind, and the ONLY
/// thing `TargetFilter::ExiledBySource` reads when Play Arcanum resolves —
/// fixture setup standing in for an earlier, separately-resolved exile, not an
/// injected target.
fn link_exiled_with(runner: &mut GameRunner, card: ObjectId, source: ObjectId) {
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: card,
        source_id: source,
        kind: ExileLinkKind::TrackedBySource,
    });
}

/// Index of Play Arcanum among `source`'s printed activated abilities.
fn play_arcanum_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|text| text.starts_with("Play Arcanum"))
        })
        .expect("Urianger on the battlefield must expose Play Arcanum")
}

/// Activate Play Arcanum through the real CR 602 activation pipeline — the
/// production path a player takes.
///
/// NO targets are declared. `TargetFilter::ExiledBySource` is a context ref
/// (CR 607.2a), so it surfaces no target slot at announcement; the grant must
/// repopulate its own set from the source's exile links as it resolves
/// (`cast_from_zone`'s linked-exile fallback) and then stamp the rider onto the
/// permissions it builds (`record_lingering_permissions`). Injecting a target
/// vector here would supply something production never does and hide a grant
/// that resolves with zero targets.
fn grant_play_arcanum(runner: &mut GameRunner, source: ObjectId) {
    let index = play_arcanum_index(runner, source);
    runner.activate(source, index).resolve();
}

/// Build Urianger on the battlefield plus one card exiled WITH him of `cost`,
/// seeded with exactly `pool` colorless mana, then activate Play Arcanum.
fn urianger_board(cost: u32, pool: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let urianger = scenario
        .add_creature_from_oracle(P0, "Urianger Augurelt", 2, 3, URIANGER_ORACLE)
        .id();
    let spell = scenario
        .add_creature_to_exile(P0, "Arcanum Spell", 2, 2)
        .with_mana_cost(ManaCost::generic(cost))
        .id();
    scenario.with_mana_pool(P0, generic_mana(pool));
    let mut runner = scenario.build();
    link_exiled_with(&mut runner, spell, urianger);
    grant_play_arcanum(&mut runner, urianger);
    (runner, urianger, spell)
}

/// Build Urianger's grant over both a spell and a land in exile. Fixture setup
/// places the land in exile; the test exercises its actual land-play delivery.
fn urianger_board_with_exiled_land(
    cost: u32,
    pool: usize,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let urianger = scenario
        .add_creature_from_oracle(P0, "Urianger Augurelt", 2, 3, URIANGER_ORACLE)
        .id();
    let spell = scenario
        .add_creature_to_exile(P0, "Arcanum Spell", 2, 2)
        .with_mana_cost(ManaCost::generic(cost))
        .id();
    let land = scenario.add_land_to_exile(P0, "Arcanum Island").id();
    scenario.with_mana_pool(P0, generic_mana(pool));
    let mut runner = scenario.build();
    link_exiled_with(&mut runner, spell, urianger);
    link_exiled_with(&mut runner, land, urianger);
    grant_play_arcanum(&mut runner, urianger);
    (runner, urianger, spell, land)
}

/// The elected permission for `spell` ? the one the cast pipeline prices
/// against.
fn cast_permission(runner: &GameRunner, spell: ObjectId) -> &CastingPermission {
    runner.state().objects[&spell]
        .casting_permissions
        .iter()
        .find(|permission| {
            !matches!(
                permission,
                CastingPermission::PlayFromExile {
                    provenance: PlayFromExileProvenance::LandLookCompanion,
                    ..
                }
            )
        })
        .expect("Play Arcanum must build a cast authority for the exiled card")
}

/// CR 601.2f: a {3} spell cast through Urianger's grant costs {1}.
///
/// Revert-failing: with the rider dropped (or never stamped onto the
/// permission) the spell costs {3} and the pool ends at 2, not 4.
#[test]
fn urianger_play_arcanum_reduces_spell_cast_this_way() {
    let (mut runner, _urianger, spell) = urianger_board(3, 5);

    assert_eq!(
        cast_permission(&runner, spell).cast_cost_modifier(),
        Some(&CastCostModifier::reduce(ManaCost::generic(2))),
        "the grant's rider must be stamped onto the cast authority it creates"
    );

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        4,
        "CR 601.2f: {{3}} reduced by {{2}} is {{1}}, so 5 mana must leave 4"
    );
}

/// CR 601.2f: "It can't be reduced to less than {0}." A {1} spell reduced by
/// {2} costs {0} ? the reduction floors instead of wrapping.
///
/// Revert-failing: without the reduction the spell costs {1} and the pool ends
/// at 0, not 1.
#[test]
fn urianger_reduction_floors_at_zero() {
    let (mut runner, _urianger, spell) = urianger_board(1, 1);

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        1,
        "CR 601.2f: a {{1}} cost reduced by {{2}} floors at {{0}}, spending nothing"
    );
}

/// CR 305.1 + CR 601.2f: a `mode: Play` grant builds TWO permissions on the
/// exiled card ? the cast authority, and the `LandLookCompanion` that carries
/// the CR 305.1 land-play / CR 406.3b look half. "Spells you cast this way"
/// scopes the rider to the cast authority only; a land is never a spell, so the
/// companion must carry no cost modifier at all.
///
/// Positive and negative halves share one fixture: the same grant that leaves
/// the companion unmodified still reduces the spell {3} -> {1}.
#[test]
fn urianger_land_look_companion_carries_no_cost_modifier() {
    let (mut runner, _urianger, spell) = urianger_board(3, 5);

    let permissions = runner.state().objects[&spell].casting_permissions.clone();
    let companions: Vec<_> = permissions
        .iter()
        .filter(|permission| {
            matches!(
                permission,
                CastingPermission::PlayFromExile {
                    provenance: PlayFromExileProvenance::LandLookCompanion,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        companions.len(),
        1,
        "reach guard: `mode: Play` must actually build the CR 305.1 land-play \
         companion this assertion is about, got {permissions:?}"
    );
    assert_eq!(
        companions[0].cast_cost_modifier(),
        None,
        "CR 305.1: a land is never a spell, so the land-play companion must not \
         carry the spell-only cost rider"
    );
    assert_eq!(
        cast_permission(&runner, spell).cast_cost_modifier(),
        Some(&CastCostModifier::reduce(ManaCost::generic(2))),
        "the sibling cast authority from the same grant DOES carry it"
    );

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        4,
        "CR 601.2f: the cast half of the same grant is still reduced {{3}} -> {{1}}"
    );
}

/// Playing Urianger's actual exiled-land companion must consume only that land
/// permission and leave the linked spell's reduce rider intact for the real
/// cast pipeline.
#[test]
fn urianger_land_companion_plays_a_land_without_consuming_spell_reduction() {
    let (mut runner, _urianger, spell, land) = urianger_board_with_exiled_land(3, 5);
    let land_card_id = runner.state().objects[&land].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id: land_card_id,
        })
        .expect("Urianger's mode: Play grant must surface a legal land-play action");
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "the linked land must enter the battlefield through GameAction::PlayLand"
    );
    // Playing a land from exile fires Urianger's first printed ability. Resolve
    // that trigger through priority before casting the sorcery-speed spell.
    runner.pass_both_players();
    assert_eq!(
        cast_permission(&runner, spell).cast_cost_modifier(),
        Some(&CastCostModifier::reduce(ManaCost::generic(2))),
        "playing the land must not select or consume the spell's cast permission"
    );

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        4,
        "the spell still costs {{1}} after the companion land was played"
    );
}

/// CR 601.2a + CR 601.2f: the rider is a property of the ELECTED permission,
/// not of the object. A sibling grant carrying a {2} RAISE sits on the same
/// card; the cast must price against Urianger's elected grant alone.
///
/// The three possible answers are distinguishable: {1} (elected grant only, the
/// fix), {3} (no rider consulted), {5} (sibling raise scanned as well).
#[test]
fn selected_permission_modifier_does_not_leak_from_sibling_permission() {
    let (mut runner, _urianger, spell) = urianger_board(3, 5);

    // A later sibling grant on the SAME object with the opposite direction.
    let sibling = CastingPermission::ExileWithAltCost {
        cost: ManaCost::generic(3),
        cost_provenance: engine::types::ability::ExileGrantCostProvenance::NormalCost,
        cast_transformed: false,
        constraint: None,
        granted_to: Some(P0),
        resolution_cleanup: None,
        duration: None,
        source_id: None,
        graveyard_replacement: None,
        enters_with_counter: None,
        enters_with_modifications: Vec::new(),
        mana_spend_permission: None,
        cast_cost_modifier: Some(CastCostModifier::raise(ManaCost::generic(2))),
    };
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions
        .push(sibling);

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        4,
        "CR 601.2a: only the elected grant prices the cast — {{1}} paid (4 left), \
         not {{3}} (2 left, rider ignored) and not {{5}} (0 left, sibling raise leaked)"
    );
}

/// CR 601.2f: increases are applied before reductions, across the permission
/// rider and board-wide statics alike. Base {1}, a battlefield static raise of
/// {2}, and the grant's {2} reduction give {1} + {2} - {2} = {1}.
///
/// This is the ordering discriminator. Applying the reduction FIRST (the
/// positional pre-add this change removed) gives {1} - {2} = {0} floored, then
/// + {2} = {2}. Dropping the rider entirely gives {3}.
#[test]
fn permission_reduction_and_static_raise_apply_in_cr_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let urianger = scenario
        .add_creature_from_oracle(P0, "Urianger Augurelt", 2, 3, URIANGER_ORACLE)
        .id();
    // CR 601.2f: a live board-wide increase on every spell its controller casts.
    scenario
        .add_creature(P0, "Taxing Presence", 1, 1)
        .with_static(StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            amount: ManaCost::generic(2),
            spell_filter: None,
            dynamic_count: None,
            reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
        });
    let spell = scenario
        .add_creature_to_exile(P0, "Arcanum Spell", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(P0, generic_mana(5));
    let mut runner = scenario.build();
    link_exiled_with(&mut runner, spell, urianger);
    grant_play_arcanum(&mut runner, urianger);

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.mana_pool_total(P0),
        4,
        "CR 601.2f: {{1}} + raise {{2}} - reduce {{2}} = {{1}} (4 left); \
         reduce-before-raise would give {{2}} (3 left) and no rider {{3}} (2 left)"
    );
    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
}

/// Keeps the shape guard reachable on its own so a parser regression fails
/// here, named, rather than only inside a cost assertion.
#[test]
fn urianger_play_arcanum_absorbs_the_reduce_rider() {
    let _ = play_arcanum_effect();
}

/// CR 607.2a: the plural exiled-cards anaphor splits on its TAIL, and the
/// split must not widen past the source-anchored form.
///
/// "cards exiled with <self>" reads the source's durable exile ledger
/// (`ExiledBySource`). "cards exiled this way" names the batch the SAME
/// resolution just produced and keeps its same-chain `ParentTarget` binding —
/// re-pointing it at the ledger would make every such card read an unrelated
/// lifetime exile pile.
#[test]
fn cards_exiled_anaphor_splits_source_anchored_from_this_way() {
    fn cast_from_zone_target(oracle: &str, name: &str, core_type: &str) -> TargetFilter {
        let parsed = parse_oracle_text(
            oracle,
            name,
            &[],
            &[core_type.to_string()],
            &["Elf".to_string()],
        );
        let rendered = format!(
            "{:?}{:?}{:?}",
            parsed.abilities, parsed.triggers, parsed.statics
        );
        let roots = parsed.abilities.iter().chain(
            parsed
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref()),
        );
        for root in roots {
            let mut node = Some(root);
            while let Some(current) = node {
                if let Effect::CastFromZone { target, .. } = current.effect.as_ref() {
                    return target.clone();
                }
                node = current.sub_ability.as_deref();
            }
        }
        panic!("expected a CastFromZone for {name}: {rendered}");
    }

    // Source-anchored: Urianger's Play Arcanum.
    assert_eq!(
        cast_from_zone_target(
            "{T}: Until end of turn, you may play cards exiled with Urianger Augurelt.",
            "Urianger Augurelt",
            "Creature",
        ),
        TargetFilter::ExiledBySource,
    );
    // Source-anchored through a "this <type>" self-reference: Rogue Class,
    // Pick Up the Pace.
    assert_eq!(
        cast_from_zone_target(
            "{T}: You may play cards exiled with this creature.",
            "Ledger Reader",
            "Creature",
        ),
        TargetFilter::ExiledBySource,
    );
    // Same-resolution batch: Dream Harvest's "cards exiled this way". Unchanged.
    assert_eq!(
        cast_from_zone_target(
            "Each opponent exiles cards from the top of their library until they have exiled \
             cards with total mana value 5 or greater this way. Until end of turn, you may cast \
             cards exiled this way without paying their mana costs.",
            "Dream Harvest",
            "Sorcery",
        ),
        TargetFilter::ParentTarget,
    );
}

/// Urianger is fully supported: every clause of the verbatim Oracle text
/// lowers, with no `Effect::Unimplemented` anywhere in the card.
#[test]
fn urianger_has_no_unimplemented_clause() {
    let parsed = parse_oracle_text(
        URIANGER_ORACLE,
        "Urianger Augurelt",
        &[],
        &["Creature".to_string()],
        &["Elf".to_string(), "Advisor".to_string()],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "reach guard: Urianger's two disjunctive exile-play events must lower separately"
    );
    assert!(
        parsed.abilities.iter().any(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|text| text.starts_with("Play Arcanum"))
        }),
        "reach guard: Urianger's Play Arcanum activated ability must lower"
    );
    let rendered = format!(
        "{:?}{:?}{:?}",
        parsed.abilities, parsed.triggers, parsed.statics
    );
    assert!(
        !rendered.contains("Unimplemented"),
        "Urianger must have no unimplemented clause: {rendered}"
    );
}
