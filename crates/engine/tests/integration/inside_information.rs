//! Inside Information (HOB) — runtime coverage for the three composed pieces:
//!
//! > Exile the top X cards of target opponent's library. You may play those
//! > cards this turn. If you cast a spell this way, pay life equal to its mana
//! > value rather than pay its mana cost.
//!
//! 1. CR 601.2b + CR 115 (targeting): X is announced as part of the cost, and
//!    the exile comes from a TARGET OPPONENT's library — not the caster's own
//!    — while the resulting `PlayFromExile` grant still binds to the ability
//!    CONTROLLER (CR 109.5), not the exiled cards' owner.
//! 2. CR 701.18b (play, not cast): "play" covers both casting a spell
//!    and playing a land, so a land among the exiled cards must be playable
//!    too, not just spells.
//! 3. CR 118.9 + CR 119.4 (alternative cost): "pay life equal to its mana
//!    value rather than pay its mana cost" REPLACES the mana cost for a spell
//!    cast this way. This reuses the `AbilityCost` alt-cost pipeline already
//!    proven by Nashi / Xander's Pact (`ExileWithAltAbilityCost` on a
//!    `CastFromZone` grant); Inside Information's grant is a plain "you may
//!    play those cards" `PlayFromExile` permission instead (it must also
//!    authorize land plays), so the alt cost folds onto a new
//!    `PlayFromExile::alt_ability_cost` field that only the spell-casting cost
//!    pipeline ever consults — land plays never reach it and stay unaffected.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, CardPlayMode, CastingPermission, Duration, PlayFromExileProvenance, QuantityExpr,
    QuantityRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};

const ORACLE: &str = "Exile the top X cards of target opponent's library. You may play those \
cards this turn. If you cast a spell this way, pay life equal to its mana value rather than \
pay its mana cost.";

/// Find the `PlayFromExile` grant on `obj_id` bound to `grantee`.
fn play_from_exile_for(
    state: &engine::types::game_state::GameState,
    obj_id: engine::types::identifiers::ObjectId,
    grantee: engine::types::player::PlayerId,
) -> Option<&CastingPermission> {
    state.objects[&obj_id].casting_permissions.iter().find(|p| {
        matches!(
            p,
            CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == grantee
        )
    })
}

/// Shared rig: P0 has Inside Information in hand (mana cost `{X}{B}{B}`,
/// matching the card's real printed cost — verified against card-data.json)
/// with exactly enough mana for X=3 ({3} generic + {B}{B} = 5 units, so the
/// pool is provably EMPTY afterward). P1's library top-to-bottom is
/// [spell_a (MV 3, "Draw a card."), land, spell_c, buried, buried2] so X=3
/// exiles exactly the first three and leaves the rest untouched.
struct Rig {
    scenario: GameScenario,
    inside_information: engine::types::identifiers::ObjectId,
    spell_a: engine::types::identifiers::ObjectId,
    land: engine::types::identifiers::ObjectId,
    spell_c: engine::types::identifiers::ObjectId,
    buried: engine::types::identifiers::ObjectId,
}

fn build_rig() -> Rig {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 needs a small library so spell_a's "Draw a card." on resolution
    // doesn't draw from empty.
    scenario.add_card_to_library_top(P0, "P0 Filler");

    // P1's library, bottom-to-top insertion order (each call reseats at
    // library[0], so the LAST call ends up on top) — final top-to-bottom
    // order is [spell_a, land, spell_c, buried].
    let buried = scenario.add_card_to_library_top(P1, "Opp Buried");
    let spell_c = scenario
        .add_spell_to_library_top(P1, "Opp Filler Spell", true)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let land = scenario.add_card_to_library_top(P1, "Opp Exiled Forest");
    let spell_a = scenario
        .add_spell_to_library_top(P1, "Opp Draw Spell", true)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();

    let inside_information = scenario
        .add_spell_to_hand_from_oracle(P0, "Inside Information", false, ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();

    // X=3 costs {3}{B}{B}: 3 generic-payable + 2 black, exactly 5 units —
    // the pool is fully drained by casting Inside Information.
    let mut pool = vec![
        ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(9_999),
            false,
            vec![],
        );
        3
    ];
    pool.extend(vec![
        ManaUnit::new(
            ManaType::Black,
            engine::types::identifiers::ObjectId(9_998),
            false,
            vec![],
        );
        2
    ]);
    scenario.with_mana_pool(P0, pool);

    Rig {
        scenario,
        inside_information,
        spell_a,
        land,
        spell_c,
        buried,
    }
}

/// Claims (a)+(1): casting with X=3 targeting an opponent exiles the top
/// THREE cards of THEIR library (not the caster's own), and the resulting
/// `PlayFromExile` grant binds to the CASTER (P0), not the exiled cards'
/// owner (P1) — CR 109.5.
#[test]
fn inside_information_exiles_x_from_targeted_opponents_library() {
    let rig = build_rig();
    let mut runner = rig.scenario.build();

    let outcome = runner
        .cast(rig.inside_information)
        .x(3)
        .target_player(P1)
        .resolve();

    for id in [rig.spell_a, rig.land, rig.spell_c] {
        assert_eq!(
            outcome.zone_of(id),
            Zone::Exile,
            "X=3 must exile the top THREE cards of the targeted opponent's library"
        );
        assert_eq!(
            outcome.state().objects[&id].owner,
            P1,
            "the exiled cards remain owned by the targeted opponent, not the caster"
        );
        let grant = play_from_exile_for(outcome.state(), id, P0).unwrap_or_else(|| {
            panic!(
                "exiled card must carry a PlayFromExile grant bound to the CASTER P0, got {:?}",
                outcome.state().objects[&id].casting_permissions
            )
        });
        match grant {
            CastingPermission::PlayFromExile {
                duration,
                alt_ability_cost,
                ..
            } => {
                assert_eq!(
                    *duration,
                    Duration::UntilEndOfTurn,
                    "\"you may play those cards this turn\" is an until-end-of-turn grant"
                );
                assert_eq!(
                    *alt_ability_cost,
                    Some(AbilityCost::PayLife {
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::SelfManaValue
                        }
                    }),
                    "the alt-cost rider must fold onto the grant as pay-life-equal-to-mana-value"
                );
            }
            _ => unreachable!("matched PlayFromExile above"),
        }
        // CR 109.5: the grant is scoped to the CASTER, never the opponent
        // whose library was exiled from.
        assert!(
            play_from_exile_for(outcome.state(), id, P1).is_none(),
            "the targeted opponent (card owner) must NOT receive a play grant on their own cards"
        );
    }

    // The un-exiled remainder of the opponent's library stays put and ungranted.
    assert_eq!(outcome.zone_of(rig.buried), Zone::Library);
    assert!(outcome.state().objects[&rig.buried]
        .casting_permissions
        .is_empty());
}

/// Claim (b)+(3): casting one of the exiled spells THIS WAY pays life equal to
/// its mana value INSTEAD of its mana cost. With the mana pool fully drained
/// by the Inside Information cast (X=3 consumed exactly the 5 provided
/// units), a normal cast of a {2}{R} (MV 3) spell would be flatly rejected —
/// the cast succeeding at all, plus the -3 life delta and zero mana spent,
/// together prove the alternative cost actually replaced the mana cost.
#[test]
fn casting_an_exiled_spell_pays_life_instead_of_mana_cost() {
    let rig = build_rig();
    let mut runner = rig.scenario.build();

    runner
        .cast(rig.inside_information)
        .x(3)
        .target_player(P1)
        .resolve();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "reach-guard: X=3 must have fully drained P0's 5-unit pool"
    );

    let cast = runner.cast(rig.spell_a).resolve();
    cast.assert_life_delta(P0, -3);
    cast.assert_hand_drawn(P0, 1);
    assert_eq!(
        cast.state().players[0].mana_pool.total(),
        0,
        "CR 118.9: paying the alternative cost must not touch the (already empty) mana pool"
    );
    cast.assert_zone(&[rig.spell_a], Zone::Graveyard);
}

/// Claim (c)+(2): a LAND among the exiled cards is playable too, not just
/// spells — the alt-cost rider is spell-cast-only and must never block the
/// CR 701.18b land-play route the same grant also authorizes.
#[test]
fn an_exiled_land_can_be_played_not_just_cast() {
    let rig = build_rig();
    let mut runner = rig.scenario.build();

    // Bare `add_card_to_library_top` cards carry no core type — stamp this
    // one as a basic Forest before it gets exiled, mirroring `add_basic_land`.
    {
        let obj = runner.state_mut().objects.get_mut(&rig.land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    runner
        .cast(rig.inside_information)
        .x(3)
        .target_player(P1)
        .resolve();
    assert!(
        runner.state().objects[&rig.land]
            .card_types
            .core_types
            .contains(&CoreType::Land),
        "reach-guard: the exiled card is still typed as a Land after exile"
    );

    let land_card_id = runner.state().objects[&rig.land].card_id;
    let lands_before = runner.state().lands_played_this_turn;
    runner
        .act(GameAction::PlayLand {
            object_id: rig.land,
            card_id: land_card_id,
        })
        .expect("playing an exiled land granted by Inside Information must be legal");

    assert_eq!(
        runner.state().objects[&rig.land].zone,
        Zone::Battlefield,
        "playing the exiled land must move it to the battlefield"
    );
    assert_eq!(
        runner.state().lands_played_this_turn,
        lands_before + 1,
        "playing the exiled land consumes exactly one land play"
    );
}

/// Claim (d): after this turn ends, the exiled cards are no longer playable —
/// CR 514.2 prunes the until-end-of-turn `PlayFromExile` grant at cleanup,
/// mirroring the established Harnfel ("you may play those cards this turn")
/// expiry regression in `birgi.rs`.
#[test]
fn unplayed_exiled_cards_expire_at_end_of_turn() {
    let rig = build_rig();
    let mut runner = rig.scenario.build();

    runner
        .cast(rig.inside_information)
        .x(3)
        .target_player(P1)
        .resolve();

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&rig.spell_c),
        "the still-unplayed exiled spell must be castable before the turn ends"
    );

    // Cross the turn boundary into P1's upkeep (past P0's cleanup step).
    runner.advance_to_upkeep();

    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&rig.spell_c),
        "CR 514.2 + CR 611.2a: the unplayed exiled card must no longer be castable once the turn changes"
    );
    assert!(
        play_from_exile_for(runner.state(), rig.spell_c, P0).is_none(),
        "CR 514.2 + CR 611.2a: the expired PlayFromExile grant must be pruned at end of turn"
    );
}

/// Regression (PR #8007 review): CR 601.2a + CR 118.9a — the cost pipeline
/// must charge the alt cost from the `PlayFromExile` grant SELECTED for this
/// cast, never a different overlapping grant on the same exiled object. A
/// prior bug in `check_additional_cost_or_pay` scanned every exile
/// permission on the object (`obj.casting_permissions.iter().find_map(..)`)
/// instead of indexing the one `casting_permission_index` actually elected,
/// so a normally-selected plain `PlayFromExile` grant could still get
/// charged a SIBLING grant's alternative life cost (Inside Information
/// class) even though the mana-zeroing pipeline (`casting::cast_spell`)
/// correctly left its mana cost intact — the player paid both the full mana
/// cost AND an unrelated life cost.
///
/// Setup: `spell_a` sits in exile carrying TWO `PlayFromExile` grants for
/// P0 — a plain grant with no alt cost inserted at index 0 (so
/// `selected_object_cast_permission_index`'s first-match scan elects it),
/// and the pre-existing Inside Information alt-cost grant (pay life equal to
/// mana value) at index 1. Casting `spell_a` must pay its normal mana cost
/// and NOT touch life, proving the selected (index-0) grant's cost — not
/// index 1's — governs the cast.
#[test]
fn overlapping_exile_grant_pays_the_selected_permissions_cost_not_a_sibling_grants() {
    let rig = build_rig();
    let mut runner = rig.scenario.build();

    runner
        .cast(rig.inside_information)
        .x(3)
        .target_player(P1)
        .resolve();

    // spell_a now carries exactly one grant: the Inside Information alt-cost
    // `PlayFromExile`. Insert a plain grant BEFORE it (index 0) so cast-time
    // permission selection elects the plain grant, not the alt-cost one.
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&rig.spell_a).unwrap();
        assert_eq!(
            obj.casting_permissions.len(),
            1,
            "reach-guard: spell_a must start with exactly the Inside Information grant"
        );
        obj.casting_permissions.insert(
            0,
            CastingPermission::PlayFromExile {
                provenance: PlayFromExileProvenance::Impulse,
                duration: Duration::UntilEndOfTurn,
                granted_to: P0,
                mode: CardPlayMode::Play,
                frequency: CastFrequency::Unlimited,
                source_id: None,
                invalidation: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                alt_ability_cost: None,
                land_enter_tapped: EtbTapState::Unspecified,
            },
        );
        // The Inside Information cast (X=3) fully drained P0's pool. Top it
        // back up with exactly spell_a's real printed cost ({R}{2} = 3 mana
        // units) so a normal-mana cast through the plain grant is affordable.
        state.players[0].mana_pool.mana.extend(vec![
            ManaUnit::new(
                ManaType::Red,
                engine::types::identifiers::ObjectId(9_994),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(9_993),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(9_992),
                false,
                vec![],
            ),
        ]);
    }

    let cast = runner.cast(rig.spell_a).resolve();

    // The selected (plain) grant carries no alt cost — casting through it
    // must never charge the sibling Inside Information grant's pay-life
    // alternative cost.
    cast.assert_life_delta(P0, 0);
    cast.assert_hand_drawn(P0, 1);
    assert_eq!(
        cast.state().players[0].mana_pool.total(),
        0,
        "the selected plain grant must pay spell_a's real {{R}}{{2}} mana cost normally"
    );
    cast.assert_zone(&[rig.spell_a], Zone::Graveyard);
}
