//! Regression: GitHub issue #2359 — Rod of Absorption.
//!
//! Oracle:
//!   "Whenever a player casts an instant or sorcery spell, exile it instead of
//!    putting it into a graveyard as it resolves.
//!    {X}, {T}, Sacrifice this artifact: You may cast any number of spells from
//!    among cards exiled with this artifact with total mana value X or less
//!    without paying their mana costs."
//!
//! Three reported defects:
//!   1. The exile-on-resolve trigger only tracked the controller's spells —
//!      opponents' instants/sorceries were not exiled with Rod.
//!   2. Only one spell was recorded, not all accumulated exiled spells.
//!   3. Activating {X},{T},Sacrifice ignored the exiled cards (no free-cast
//!      offer) yet still sacrificed Rod.
//!
//! CR 614.1a / CR 608.2n: "exile it instead of putting it into a graveyard as it
//! resolves" is a self-replacement rider applied to the resolving spell.
//! CR 607.2b / CR 406.6: cards exiled this way are "exiled with" Rod (a linked-
//! exile pool that ACCUMULATES across resolutions).
//! CR 601.2b / CR 118.9: the activated ability lets the controller cast those
//! exiled cards without paying their mana costs.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ROD_TEXT: &str = "Whenever a player casts an instant or sorcery spell, exile it instead of \
     putting it into a graveyard as it resolves.\n{X}, {T}, Sacrifice this artifact: You may cast \
     any number of spells from among cards exiled with this artifact with total mana value X or \
     less without paying their mana costs.";

/// Cast a no-target spell from `caster`'s hand and resolve it to completion,
/// returning once the stack is empty again. Passes priority until `caster`
/// holds it so an opponent's instant can be cast during the active player's turn.
fn cast_and_resolve(
    runner: &mut engine::game::scenario::GameRunner,
    caster: P,
    spell: engine::types::identifiers::ObjectId,
) {
    // CR 117.1: give `caster` priority before casting.
    for _ in 0..4 {
        if runner.state().priority_player == caster {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass to caster");
    }
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must succeed");
    // Drain the stack: both players pass priority repeatedly.
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

use engine::types::player::PlayerId as P;

/// DEFECT 1 + 2: both players' instant/sorcery spells are exiled instead of
/// going to the graveyard when they resolve, AND every such spell accumulates
/// in Rod's exiled-with pool.
#[test]
fn rod_exiles_both_players_spells_and_accumulates() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Libraries so "Draw a card" never decks a player (avoids a loss SBA).
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C", "Filler D"]);
    scenario.with_library_top(P1, &["Filler E", "Filler F", "Filler G", "Filler H"]);

    // Rod of Absorption on P0's battlefield.
    let _rod = scenario
        .add_creature(P0, "Rod of Absorption", 0, 0)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text(ROD_TEXT)
        .id();

    // P0's instant and sorcery (in hand) — bare, no targets, so they resolve
    // cleanly straight to the graveyard (modulo the Rod replacement).
    let p0_instant = scenario
        .add_spell_to_hand(P0, "P0 Instant", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p0_sorcery = scenario
        .add_spell_to_hand(P0, "P0 Sorcery", false)
        .with_mana_cost(ManaCost::zero())
        .id();

    // P1's instant — cast by the OPPONENT (defect 1).
    let p1_instant = scenario
        .add_spell_to_hand(P1, "P1 Instant", true)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, P0, p0_instant);
    cast_and_resolve(&mut runner, P0, p0_sorcery);
    cast_and_resolve(&mut runner, P1, p1_instant);

    // DEFECT 1: the opponent's instant must be exiled (not in graveyard).
    assert_eq!(
        runner.state().objects[&p1_instant].zone,
        Zone::Exile,
        "opponent's instant must be exiled instead of going to the graveyard (CR 614.1a)"
    );
    // All three resolved spells must be in exile, not the graveyard.
    for (id, label) in [
        (p0_instant, "P0 instant"),
        (p0_sorcery, "P0 sorcery"),
        (p1_instant, "P1 instant"),
    ] {
        let obj = &runner.state().objects[&id];
        assert_eq!(
            obj.zone,
            Zone::Exile,
            "{label} must be exiled instead of going to the graveyard"
        );
        assert_eq!(
            obj.exile_from_stack_linked_source, None,
            "{label}'s stack-exile marker must be transient and cleared after the zone change"
        );
    }

    // DEFECT 2: all three spells must be linked to Rod (accumulated), not just
    // the most recent one.
    let rod_id = _rod;
    let linked: Vec<_> =
        engine::game::players::linked_exile_cards_for_source(runner.state(), rod_id)
            .iter()
            .map(|s| s.exiled_id)
            .collect();
    assert!(
        linked.contains(&p0_instant),
        "P0 instant must be exiled-with Rod; linked = {linked:?}"
    );
    assert!(
        linked.contains(&p0_sorcery),
        "P0 sorcery must be exiled-with Rod (accumulated); linked = {linked:?}"
    );
    assert!(
        linked.contains(&p1_instant),
        "opponent's instant must be exiled-with Rod; linked = {linked:?}"
    );
    assert_eq!(
        linked.len(),
        3,
        "all three resolved instant/sorcery spells must accumulate in Rod's exiled-with pool"
    );
}

/// Index of Rod's `{X},{T},Sacrifice` activated ability — the only one whose
/// top-level effect casts from a zone.
fn rod_activated_index(
    state: &engine::types::game_state::GameState,
    rod: engine::types::identifiers::ObjectId,
) -> usize {
    use engine::types::ability::Effect;
    state.objects[&rod]
        .abilities
        .iter()
        .position(|def| matches!(&*def.effect, Effect::CastFromZone { .. }))
        .expect("Rod must have a cast-from-exile activated ability")
}

/// DEFECT 3: activating `{X},{T},Sacrifice` with exiled spells underneath must
/// offer those cards for free-casting (CR 601.2b / CR 118.9) — not silently
/// sacrifice Rod and do nothing.
#[test]
fn rod_activation_free_casts_exiled_spells_and_sacrifices_rod() {
    use engine::types::ability::CastingPermission;
    use engine::types::mana::{ManaType, ManaUnit};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rod = scenario
        .add_creature(P0, "Rod of Absorption", 0, 0)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text(ROD_TEXT)
        .id();

    // Two of P0's instants accumulate under Rod via the exile-on-resolve trigger.
    let bolt_a = scenario
        .add_spell_to_hand(P0, "Bolt A", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let bolt_b = scenario
        .add_spell_to_hand(P0, "Bolt B", true)
        .with_mana_cost(ManaCost::zero())
        .id();

    // {X} mana for the activation (X = 2 → pay {2}); the free casts cost nothing.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();

    // Resolve both bolts so they exile under Rod (the accumulated set).
    cast_and_resolve(&mut runner, P0, bolt_a);
    cast_and_resolve(&mut runner, P0, bolt_b);

    assert_eq!(runner.state().objects[&bolt_a].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&bolt_b].zone, Zone::Exile);

    // Pass priority back to P0 so the active player can activate the ability.
    for _ in 0..4 {
        if runner.state().priority_player == P0 && runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    let idx = rod_activated_index(runner.state(), rod);
    // CR 601.2b: a free cast forces X = 0; the activation's own {X} is announced
    // here as 2 (paid from the pool), capping the exiled cards' total MV ≤ 2.
    // CR 609.3: accept the "you may cast …" optional so the grant resolves.
    runner.activate(rod, idx).x(2).accept_optional().resolve();

    // CR 701.21 / CR 118.9: Rod was sacrificed as part of the cost — it must be
    // gone from the battlefield.
    assert_ne!(
        runner.state().objects[&rod].zone,
        Zone::Battlefield,
        "Rod must be sacrificed as part of the activation cost"
    );

    // CR 601.2b / CR 118.9: BOTH exiled bolts must now carry a zero-cost casting
    // permission so the controller can cast them for free. On the bug the
    // activation sacrificed Rod and granted nothing.
    for (id, label) in [(bolt_a, "Bolt A"), (bolt_b, "Bolt B")] {
        let obj = &runner.state().objects[&id];
        assert!(
            obj.casting_permissions.iter().any(|p| matches!(
                p,
                CastingPermission::ExileWithAltCost { cost, .. }
                    if cost.is_without_paying_mana()
            )),
            "{label} (exiled with Rod) must be free-castable after activation; \
             permissions = {:?}",
            obj.casting_permissions
        );
    }
}
