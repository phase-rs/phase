//! Discriminating runtime regression for **Triple Triad**:
//!
//! > At the beginning of your upkeep, each player exiles the top card of their
//! > library. Until end of turn, you may play the card you own exiled this way
//! > and each other card exiled this way with lesser mana value than it without
//! > paying their mana costs.
//!
//! The compound play grant must cover exactly {the card the controller owns
//! exiled this way} ∪ {other cards exiled this way whose mana value is strictly
//! less than the owned card's mana value}. Before the fix the clause misparsed
//! to `Effect::CastFromZone { target: TargetFilter::Any }` (the bare Branch-3
//! fallback), which grants no scoped play permission.
//!
//! The fix adds `ObjectScope::OwnedLinkedExileCard` (the owned source-linked
//! exiled card, resolved via `linked_exile_cards_for_source` selecting
//! `owner == controller`) and a parser branch lowering the compound subject to
//! `Or[ And[ExiledBySource, Owned{You}], And[ExiledBySource, Cmc{LT,
//! OwnedLinkedExileCard MV}] ]`.
//!
//! This test seeds the source's linked-exile pile directly, including an older
//! P0-owned card from a prior resolution, and binds the current resolution's
//! exiled set through `last_zone_changed_ids`. It then drives the grant through
//! the same production resolver the runtime uses (`resolve_ability_chain` ->
//! `cast_from_zone::resolve` -> `grant_lingering_permissions`).
//!
//! DISCRIMINATOR: with a P0-owned MV-3 card, a P1-owned MV-1 card, a P1-owned
//! MV-3 card, and a P1-owned MV-4 card all in the exile-by-source pile, after
//! resolution the current MV-3 (owned) and MV-1 (< 3) cards carry P0's
//! `ExileWithAltCost` free-cast permission, but neither the stale P0-owned MV-5
//! card, the P1-owned MV-3 (== 3, not *lesser*), nor the MV-4 (> 3) card carries
//! any. The stale MV-5 card is the review regression: if `OwnedLinkedExileCard`
//! reads the persistent source pile instead of the current resolution's set, the
//! MV-4 card is incorrectly treated as lesser. The equal-MV opponent card is the
//! `Comparator::LT`-vs-`LE` boundary.
//!
//! CR 118.9 + 118.9b (cast without paying mana cost), CR 601.3 (permission),
//! CR 607.2a (exiled this way), CR 108.3 (the card you own), CR 202.3
//! (lesser mana value), CR 305.1 (play).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, CastingPermission, Effect};
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TRIPLE_TRIAD_ORACLE: &str = "At the beginning of your upkeep, each player exiles the top \
card of their library. Until end of turn, you may play the card you own exiled this way and each \
other card exiled this way with lesser mana value than it without paying their mana costs.";

/// Depth-first search for the single `CastFromZone`-bearing def in the trigger
/// chain, returned cloned so it can be resolved in isolation.
fn find_cast_from_zone(def: &AbilityDefinition) -> Option<AbilityDefinition> {
    if matches!(&*def.effect, Effect::CastFromZone { .. }) {
        return Some(def.clone());
    }
    def.sub_ability
        .as_ref()
        .and_then(|s| find_cast_from_zone(s))
        .or_else(|| {
            def.else_ability
                .as_ref()
                .and_then(|e| find_cast_from_zone(e))
        })
}

fn has_p0_free_cast(perms: &[CastingPermission]) -> bool {
    perms.iter().any(|perm| {
        matches!(
            perm,
            CastingPermission::ExileWithAltCost {
                granted_to: Some(pid),
                ..
            } if *pid == P0
        )
    })
}

#[test]
fn triple_triad_grants_free_play_to_owned_and_lesser_mv_exiled_cards_only() {
    // Production parse path: the whole shipped Oracle text.
    let parsed = parse_oracle_text(
        TRIPLE_TRIAD_ORACLE,
        "Triple Triad",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1, "one upkeep trigger: {parsed:#?}");
    let execute = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("upkeep trigger should have an execute body");
    let cast_def =
        find_cast_from_zone(execute).expect("the play grant must lower to a CastFromZone clause");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let source = scenario.add_creature(P0, "Triple Triad", 0, 0).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    // The linked-exile pile contains an older P0-owned card (MV 5) plus the
    // current "each player exiles the top card" set: the controller (P0) owns
    // one card (MV 3); the opponent (P1) owns a strictly-cheaper card (MV 1),
    // an EQUAL-MV card (MV 3, the LT-vs-LE boundary), and a costlier card (MV 4).
    let stale_owned_mv5 = create_object(
        state,
        CardId(899),
        P0,
        "Stale Owned MV5".to_string(),
        Zone::Exile,
    );
    let owned_mv3 = create_object(state, CardId(900), P0, "Owned MV3".to_string(), Zone::Exile);
    let opp_mv1 = create_object(state, CardId(901), P1, "Opp MV1".to_string(), Zone::Exile);
    let opp_mv4 = create_object(state, CardId(902), P1, "Opp MV4".to_string(), Zone::Exile);
    // Equal mana value to the owned card: "each *other* card ... with *lesser*
    // mana value" excludes it — only strict `Comparator::LT` keeps it out.
    let opp_mv3 = create_object(state, CardId(904), P1, "Opp MV3".to_string(), Zone::Exile);
    // A card exiled by a DIFFERENT source must never receive the grant.
    let unrelated = create_object(state, CardId(903), P1, "Unrelated".to_string(), Zone::Exile);

    for (id, mv) in [
        (stale_owned_mv5, 5u32),
        (owned_mv3, 3u32),
        (opp_mv1, 1),
        (opp_mv3, 3),
        (opp_mv4, 4),
        (unrelated, 1),
    ] {
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(mv);
    }
    for &id in &[stale_owned_mv5, owned_mv3, opp_mv1, opp_mv3, opp_mv4] {
        state.exile_links.push(ExileLink {
            exiled_id: id,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });
    }
    state.last_zone_changed_ids = vec![owned_mv3, opp_mv1, opp_mv3, opp_mv4];

    // Resolve the grant through the production sub-chain seam. Depth 0 is the
    // top-level ability boundary and clears resolution-local zone-change ids;
    // the real ExileTop -> CastFromZone chain reaches this grant at depth 1.
    let ability = build_resolved_from_def(&cast_def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 1)
        .expect("the play-permission grant must resolve");

    let perms_of = |id: ObjectId| runner.state().objects[&id].casting_permissions.clone();

    // REACH GUARD / POSITIVE: the owned card (via disjunct 1) is playable for free.
    assert!(
        has_p0_free_cast(&perms_of(owned_mv3)),
        "the card P0 owns exiled this way must carry P0's free-cast permission, got {:?}",
        perms_of(owned_mv3)
    );
    // POSITIVE: an opponent's card with MV 1 < 3 (the owned card's MV) is playable.
    assert!(
        has_p0_free_cast(&perms_of(opp_mv1)),
        "a lesser-mana-value (MV 1 < owned MV 3) linked-exiled card must carry the free-cast permission, got {:?}",
        perms_of(opp_mv1)
    );
    // REGRESSION: the stale P0-owned MV-5 card belongs to a prior resolution and
    // must not receive this upkeep's permission or supply the "than it" threshold.
    assert!(
        !has_p0_free_cast(&perms_of(stale_owned_mv5)),
        "a stale linked card from a prior resolution must not receive this grant, got {:?}",
        perms_of(stale_owned_mv5)
    );
    // DISCRIMINATOR / NEGATIVE (LT vs LE boundary): an opponent's card with MV 3
    // EQUAL to the owned card's MV is not *lesser*, so it must NOT be playable.
    // This is the only assertion that flips if `Comparator::LT` were `LE`.
    assert!(
        !has_p0_free_cast(&perms_of(opp_mv3)),
        "an equal-mana-value (3 == 3, not lesser) linked-exiled card must NOT be playable, got {:?}",
        perms_of(opp_mv3)
    );
    // NEGATIVE: an opponent's card with MV 4 > 3 is NOT playable.
    assert!(
        !has_p0_free_cast(&perms_of(opp_mv4)),
        "a greater-mana-value (4 > 3) linked-exiled card must NOT be playable, got {:?}",
        perms_of(opp_mv4)
    );
    // NEGATIVE: a card exiled by a different source is untouched.
    assert!(
        !has_p0_free_cast(&perms_of(unrelated)),
        "a card not exiled with this source must not be playable"
    );
}

#[test]
fn triple_triad_does_not_fall_back_to_stale_owned_card_when_current_batch_has_none() {
    let parsed = parse_oracle_text(
        TRIPLE_TRIAD_ORACLE,
        "Triple Triad",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let execute = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("upkeep trigger should have an execute body");
    let cast_def =
        find_cast_from_zone(execute).expect("the play grant must lower to a CastFromZone clause");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let source = scenario.add_creature(P0, "Triple Triad", 0, 0).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let stale_owned_mv5 = create_object(
        state,
        CardId(910),
        P0,
        "Stale Owned MV5".to_string(),
        Zone::Exile,
    );
    let current_opp_mv1 = create_object(
        state,
        CardId(911),
        P1,
        "Current Opp MV1".to_string(),
        Zone::Exile,
    );
    let current_opp_mv4 = create_object(
        state,
        CardId(912),
        P1,
        "Current Opp MV4".to_string(),
        Zone::Exile,
    );

    for (id, mv) in [
        (stale_owned_mv5, 5u32),
        (current_opp_mv1, 1),
        (current_opp_mv4, 4),
    ] {
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(mv);
        state.exile_links.push(ExileLink {
            exiled_id: id,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });
    }
    state.last_zone_changed_ids = vec![current_opp_mv1, current_opp_mv4];

    let ability = build_resolved_from_def(&cast_def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 1)
        .expect("the current linked-exile candidate set must reach the play-grant resolver");

    let perms_of = |id: ObjectId| runner.state().objects[&id].casting_permissions.clone();

    // CR 607.2a + CR 608.2c: The current resolution's candidate set is
    // authoritative. Because it contains no P0-owned card, "the card you own
    // exiled this way" has no referent; an older linked card cannot supply the
    // mana-value threshold for either current card.
    assert!(
        !has_p0_free_cast(&perms_of(current_opp_mv1)),
        "the MV-1 current card must not compare against a stale owned MV-5 card"
    );
    assert!(
        !has_p0_free_cast(&perms_of(current_opp_mv4)),
        "the MV-4 current card must not compare against a stale owned MV-5 card"
    );
    assert!(
        !has_p0_free_cast(&perms_of(stale_owned_mv5)),
        "the stale owned card must not receive the current resolution's permission"
    );
}
