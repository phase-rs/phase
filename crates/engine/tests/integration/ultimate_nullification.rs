//! Runtime pipeline coverage — Ultimate Nullification ({4}{W} sorcery).
//!
//! Verbatim Oracle text (Scryfall oracle_id 2fe0ebf5-52ed-4e92-9b93-81e9ae564439):
//!   "As an additional cost to cast this spell, sacrifice a legendary creature.
//!    Exile all creatures and graveyards. Put Ultimate Nullification on the
//!    bottom of its owner's library."
//!
//! Before the parser fix, "Exile all creatures and graveyards" lowered to a
//! creature-only `ChangeZoneAll` plus an orphaned `Unimplemented { "graveyards" }`
//! — the graveyard wipe was silently dropped. These tests drive the REAL
//! cast -> pay-the-sacrifice -> resolve pipeline.
//!
//! DISCRIMINATING: with the fix reverted, `Effect::Unimplemented { "graveyards" }`
//! is a no-op, so every card in every graveyard stays put — the graveyard-exile
//! assertions below flip red. The battlefield-creature assertions do NOT
//! discriminate (the creature-only `ChangeZoneAll` still exiles them), so the
//! graveyard assertions (including the just-sacrificed legendary, in the caster's
//! own graveyard, exiled by a `controller: None` leg) are the revert-failing
//! authority. Surviving noncreature permanents are the paired reach-guard proving
//! the mass exile is not a nuke of everything.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::EngineError;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Built from the real card's exact Oracle text (the card refers to itself by its
// printed name, exercising the `~`-normalization -> `SelfRef` path).
const ULTIMATE_NULLIFICATION: &str = "As an additional cost to cast this spell, sacrifice a legendary creature.\n\
     Exile all creatures and graveyards. Put Ultimate Nullification on the bottom of its owner's library.";

// The printed cost: {4}{W}. Both fixtures set this on the spell and seed matching
// mana so the REAL total cost is paid — the sacrifice is an ADDITIONAL cost on
// top of it (CR 601.2f), never a stand-in for the mana cost.
fn ultimate_nullification_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 4,
    }
}

// {4}{W} worth of mana: one white plus four generic (colorless satisfies generic).
fn ultimate_nullification_mana() -> Vec<ManaUnit> {
    let unit = |color| ManaUnit::new(color, ObjectId(0), false, vec![]);
    vec![
        unit(ManaType::White),
        unit(ManaType::Colorless),
        unit(ManaType::Colorless),
        unit(ManaType::Colorless),
        unit(ManaType::Colorless),
    ]
}

#[test]
fn ultimate_nullification_wipes_creatures_and_all_graveyards_then_self_tucks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A filler on top of the caster's library so "bottom" placement is observable
    // (the spell must land BELOW it, not merely somewhere in the library).
    scenario.with_library_top(P0, &["Filler Top"]);

    // Caster (P0): the spell in hand, the legendary sacrificed to pay the cost, a
    // plain creature, and a noncreature permanent (land) that must survive.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ultimate Nullification", false, ULTIMATE_NULLIFICATION)
        .with_mana_cost(ultimate_nullification_cost())
        .id();
    // Fund the {4}{W} mana cost from the pool so the cast harness auto-pays it;
    // the sacrifice is the additional cost paid on top.
    scenario.with_mana_pool(P0, ultimate_nullification_mana());
    let legendary = scenario
        .add_creature(P0, "Legendary Bear", 2, 2)
        .as_legendary()
        .id();
    let own_creature = scenario.add_vanilla(P0, 1, 1);
    let own_land = scenario.add_basic_land(P0, ManaColor::White);

    // Opponent (P1): a battlefield creature and a battlefield land — proving the
    // creature leg spans ALL controllers and the land (noncreature) survives.
    let opp_creature = scenario.add_vanilla(P1, 3, 3);
    let opp_land = scenario.add_basic_land(P1, ManaColor::Green);

    // Seed both graveyards with a creature card AND a noncreature card, proving
    // the graveyard leg is "all cards, every type, every owner".
    let p0_gy_creature = scenario
        .add_creature_to_graveyard(P0, "Dead Bear", 2, 2)
        .id();
    let p0_gy_spell = scenario.add_spell_to_graveyard(P0, "Spent Bolt", true).id();
    let p1_gy_creature = scenario
        .add_creature_to_graveyard(P1, "Dead Wolf", 2, 2)
        .id();
    let p1_gy_spell = scenario
        .add_spell_to_graveyard(P1, "Spent Counterspell", true)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).sacrifice_with(&[legendary]).resolve();

    // --- Battlefield creatures (both controllers) are exiled. ---
    assert_eq!(
        outcome.zone_of(own_creature),
        Zone::Exile,
        "the caster's battlefield creature must be exiled"
    );
    assert_eq!(
        outcome.zone_of(opp_creature),
        Zone::Exile,
        "the opponent's battlefield creature must be exiled (creature leg spans all controllers)"
    );

    // --- Every graveyard card, every owner, every type, is exiled. This is the
    //     revert-failing authority: on the pre-fix parse these stay in graveyard. ---
    for (id, label) in [
        (p0_gy_creature, "caster graveyard creature card"),
        (p0_gy_spell, "caster graveyard instant card"),
        (p1_gy_creature, "opponent graveyard creature card"),
        (p1_gy_spell, "opponent graveyard instant card"),
    ] {
        assert_eq!(
            outcome.zone_of(id),
            Zone::Exile,
            "{label} must be exiled by the whole-graveyard leg"
        );
    }
    // The legendary sacrificed to pay the cost lands in the caster's graveyard,
    // then the same `controller: None` graveyard leg exiles it — proving the leg
    // is NOT scoped to the caster and really spans all owners.
    assert_eq!(
        outcome.zone_of(legendary),
        Zone::Exile,
        "the sacrificed legendary (caster's graveyard) must also be exiled by the graveyard leg"
    );

    // --- Reach-guard: noncreature battlefield permanents survive. ---
    assert_eq!(
        outcome.zone_of(own_land),
        Zone::Battlefield,
        "the caster's land is neither a creature nor a graveyard card and must survive"
    );
    assert_eq!(
        outcome.zone_of(opp_land),
        Zone::Battlefield,
        "the opponent's land must survive"
    );

    // --- Mechanic 3: the spell tucks itself to the BOTTOM of its owner's library
    //     (CR 400.3 owner-keyed library routing), never the graveyard. ---
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Library,
        "Ultimate Nullification must end in its owner's library, not the graveyard"
    );
    let p0_library: Vec<_> = outcome
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        p0_library.last().copied(),
        Some(spell),
        "the spell must be on the BOTTOM of the caster's library, below the filler; got {p0_library:?}"
    );
    assert!(
        p0_library.first() != Some(&spell),
        "the spell must NOT be on top (it went to the bottom)"
    );
}

/// Control (mechanic 1): the additional cost requires a *legendary* creature.
/// With only a nonlegendary creature available, the cast cannot be paid: the
/// engine either rejects the announcement or surfaces a sacrifice prompt with no
/// legal choice — and the nonlegendary creature is never sacrificed.
#[test]
fn ultimate_nullification_requires_a_legendary_creature_to_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ultimate Nullification", false, ULTIMATE_NULLIFICATION)
        .with_mana_cost(ultimate_nullification_cost())
        .id();
    // Only a NONlegendary creature — not a legal sacrifice for this cost.
    let plain_creature = scenario.add_vanilla(P0, 1, 1);
    // Seed the full {4}{W} so mana is payable: the ONLY unpayable component is the
    // legendary sacrifice, so the announcement can't fail for a mana reason.
    scenario.with_mana_pool(P0, ultimate_nullification_mana());

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    let cast = runner.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });

    match cast {
        // CR 601.2h: the engine rejected the announcement outright because its
        // total cost cannot be paid — here the unpayable component is the
        // mandatory legendary-creature sacrifice (CR 601.2f additional cost).
        // Assert the SPECIFIC required-additional-cost rejection so an unrelated
        // parser, mana, or implementation failure can't satisfy this branch.
        Err(e) => {
            assert!(
                matches!(&e, EngineError::ActionNotAllowed(msg) if msg.contains("required additional cost")),
                "cast must fail specifically because the required sacrifice is unpayable, got {e:?}"
            );
        }
        Ok(_) => {
            // Otherwise it must surface the mandatory sacrifice with NO legal
            // legendary to choose.
            match &runner.state().waiting_for {
                WaitingFor::PayCost {
                    kind: PayCostKind::Sacrifice,
                    choices,
                    ..
                } => {
                    assert!(
                        !choices.contains(&plain_creature),
                        "a nonlegendary creature must never be a legal sacrifice for this cost"
                    );
                    assert!(
                        choices.is_empty(),
                        "with no legendary creature there must be no legal sacrifice, got {choices:?}"
                    );
                }
                other => panic!(
                    "expected either cast rejection or an unsatisfiable Sacrifice prompt, got {other:?}"
                ),
            }
        }
    }

    // Whatever path the engine took, the nonlegendary creature is never sacrificed.
    assert_eq!(
        runner.state().objects[&plain_creature].zone,
        Zone::Battlefield,
        "the nonlegendary creature must not have been sacrificed"
    );
}
