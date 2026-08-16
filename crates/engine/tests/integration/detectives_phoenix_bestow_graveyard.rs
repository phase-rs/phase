//! Regression coverage for issue #2365 — Detective's Phoenix bestow-cast from
//! the GRAVEYARD.
//!
//! Detective's Phoenix (Murders at Karlov Manor) is an Enchantment Creature
//! whose bestow cost is COMPOUND: "Bestow—{R}, Collect evidence 6." It also
//! carries the static "You may cast this card from your graveyard using its
//! bestow ability." Casting it from the graveyard must:
//!   * route through the bestow alternative-cost lane (CR 702.103a) even though
//!     the card is in the graveyard, authorized by its own graveyard-cast
//!     permission (CR 601.2a),
//!   * split the compound bestow cost into a mana sub-cost ({R}, paid through
//!     the normal mana flow per CR 601.2g) and a non-mana residual (Collect
//!     evidence 6, paid via the additional-cost path per CR 601.2h / CR 701.59a),
//!   * apply the bestow type-changing effect (CR 702.103b: become an Aura with
//!     "enchant creature", lose the Creature type), and
//!   * resolve onto the battlefield as an Aura attached to the chosen creature
//!     (CR 303.4f).
//!
//! The capability under test is composed from shipped building blocks: the
//! bestow alt-cost lane (mirrors Evoke), `split_bestow_cost_components` (twin of
//! `split_evoke_cost_components`), the `GraveyardCastPermission` static, and the
//! `CollectEvidence` additional cost — no Detective's-Phoenix special case.
//!
//! CARD TEXT below is Detective's Phoenix's actual Oracle text as carried by the
//! engine's authoritative card data (verified in card-data.json per the issue).

use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{BestowCost, Keyword};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Detective's Phoenix Oracle text (em-dash bestow cost + graveyard-cast rider).
const PHOENIX_ORACLE: &str = "Bestow\u{2014}{R}, Collect evidence 6. (To pay this bestow \
cost, pay {R} and exile cards with total mana value 6 or greater from your graveyard.)\n\
Flying, haste\n\
Enchanted creature gets +2/+2 and has flying and haste.\n\
You may cast this card from your graveyard using its bestow ability.";

/// Put a card with mana value `cmc` into P0's graveyard to serve as Collect
/// Evidence fodder (CR 701.59a — exile graveyard cards with total mana value ≥ N).
fn add_graveyard_fodder(runner: &mut GameRunner, name: &str, cmc: u32) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        Zone::Graveyard,
    );
    runner.state_mut().objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(cmc);
    id
}

/// Add `count` units of `ty` mana to P0's pool for deterministic payment.
fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// RUNTIME (issue #2365) — Detective's Phoenix in the graveyard, cast via its
/// bestow ability onto a legal creature. Pays {R} from pool + Collect evidence 6
/// by exiling two MV-3 cards. Asserts the spell resolves as an Aura attached to
/// the creature, the Phoenix left the graveyard, the {R} was spent, and the
/// collect-evidence fodder was exiled (the residual non-mana cost was paid).
///
/// CR 702.103a (bestow from any zone the card can be played) + CR 601.2a
/// (graveyard cast permission) + CR 601.2g/h (mana + additional-cost split) +
/// CR 701.59a (collect evidence) + CR 303.4f (Aura attaches on resolution).
#[test]
fn detectives_phoenix_bestow_cast_from_graveyard_resolves_as_aura() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Detective's Phoenix in P0's graveyard. Built as an Enchantment Creature so
    // the bestow form removes only Creature (CR 702.103b). Running the Oracle
    // synthesis through the builder (production parser→synthesis path) wires the
    // compound Bestow keyword AND the graveyard-cast permission static.
    let mut builder = scenario.add_creature_to_graveyard(P0, "Detective's Phoenix", 2, 2);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 3,
    });
    // Real bestow cards are "Enchantment Creature — <subtype>"; the bestow form
    // removes only Creature (CR 702.103b), leaving Enchantment for the Aura.
    builder.with_subtypes(vec!["Phoenix"]);
    // Synthesize abilities/keywords/statics from the actual Oracle text
    // (production parser→synthesis path).
    builder.from_oracle_text_with_keywords(&["Flying", "Haste"], PHOENIX_ORACLE);
    let phoenix = builder.id();

    // A legal creature to host the bestowed Aura (P0's own creature).
    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // CR 702.103b: bestow cards are Enchantment Creatures so the bestow form
    // removes only Creature and leaves Enchantment. Synthesis preserved the
    // builder's Creature core type; add Enchantment to match the printed type
    // line (a pure type-line concern — the abilities were already synthesized).
    {
        let obj = runner.state_mut().objects.get_mut(&phoenix).unwrap();
        if !obj.card_types.core_types.contains(&CoreType::Enchantment) {
            obj.card_types.core_types.push(CoreType::Enchantment);
        }
        if !obj
            .base_card_types
            .core_types
            .contains(&CoreType::Enchantment)
        {
            obj.base_card_types.core_types.push(CoreType::Enchantment);
        }
    }

    // Verify the parser produced the compound bestow cost (discriminator: the
    // fix is what lets the runtime SPLIT this into {R} + Collect evidence 6).
    let has_compound_bestow = runner.state().objects[&phoenix]
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Bestow(BestowCost::NonMana(_))));
    assert!(
        has_compound_bestow,
        "Detective's Phoenix must carry a compound (non-mana) bestow cost; got {:?}",
        runner.state().objects[&phoenix].keywords
    );

    // Collect Evidence 6 fodder: two MV-3 cards (total mana value 6).
    let fodder_a = add_graveyard_fodder(&mut runner, "Evidence A", 3);
    let fodder_b = add_graveyard_fodder(&mut runner, "Evidence B", 3);

    // {R} for the bestow mana sub-cost.
    add_mana(&mut runner, ManaType::Red, 1);

    let card_id = runner.state().objects[&phoenix].card_id;

    // CR 601.2a: cast the Phoenix from the graveyard. The bestow alt-cost lane
    // recognizes the graveyard permission and routes to the bestow path.
    runner
        .act(GameAction::CastSpell {
            object_id: phoenix,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("casting Detective's Phoenix from the graveyard must be accepted");

    // CR 701.59a / CR 601.2h: the residual non-mana cost (Collect evidence 6)
    // surfaces as the next prompt. Exile the two MV-3 fodder cards (total MV 6) —
    // NOT the Phoenix itself (it is mid-cast, still in the graveyard).
    match runner.state().waiting_for.clone() {
        WaitingFor::CollectEvidenceChoice {
            minimum_mana_value,
            cards,
            ..
        } => {
            assert_eq!(minimum_mana_value, 6, "Collect evidence 6");
            assert!(
                cards.contains(&fodder_a) && cards.contains(&fodder_b),
                "collect-evidence offer must include the MV-3 fodder, got {cards:?}"
            );
        }
        other => panic!(
            "expected CollectEvidenceChoice after casting bestow-from-graveyard, got {other:?}"
        ),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder_a, fodder_b],
        })
        .expect("paying collect evidence with two MV-3 cards must be accepted");

    // CR 601.2c: if a single legal host requires a target prompt, answer it.
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .choose_first_legal_target()
            .expect("choosing the single legal Aura host must be accepted");
    }

    // CR 601.2i / resolution: drain the stack so the Aura resolves and attaches.
    runner.advance_until_stack_empty();

    let resolved = &runner.state().objects[&phoenix];

    // CR 303.4f + CR 702.103b: resolves to the battlefield as an Aura attached
    // to the host.
    assert_eq!(
        resolved.zone,
        Zone::Battlefield,
        "the bestowed Phoenix must resolve onto the battlefield"
    );
    assert!(
        resolved.card_types.subtypes.iter().any(|s| s == "Aura"),
        "CR 702.103b: the bestowed permanent must be an Aura, got {:?}",
        resolved.card_types.subtypes
    );
    assert!(
        !resolved.card_types.core_types.contains(&CoreType::Creature),
        "CR 702.103b: the bestowed permanent must NOT be a creature, got {:?}",
        resolved.card_types.core_types
    );
    assert_eq!(
        resolved.attached_to.and_then(|t| t.as_object()),
        Some(host),
        "CR 303.4f: the bestowed Aura must attach to its host on resolution"
    );

    // CR 601.2h / CR 701.59a: the collect-evidence cost was paid — the two MV-3
    // fodder cards were exiled from the graveyard.
    assert_eq!(
        runner.state().objects[&fodder_a].zone,
        Zone::Exile,
        "collect-evidence fodder A must be exiled (the residual bestow cost was paid)"
    );
    assert_eq!(
        runner.state().objects[&fodder_b].zone,
        Zone::Exile,
        "collect-evidence fodder B must be exiled (the residual bestow cost was paid)"
    );

    // CR 601.2g: the {R} mana sub-cost was spent (pool emptied).
    assert!(
        runner.state().players[0].mana_pool.mana.is_empty(),
        "the {{R}} bestow mana sub-cost must have been spent from the pool"
    );
}

#[test]
fn detectives_phoenix_graveyard_permission_does_not_allow_normal_creature_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder = scenario.add_creature_to_graveyard(P0, "Detective's Phoenix", 2, 2);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 3,
    });
    builder.with_subtypes(vec!["Phoenix"]);
    builder.from_oracle_text_with_keywords(&["Flying", "Haste"], PHOENIX_ORACLE);
    let phoenix = builder.id();

    let mut runner = scenario.build();
    for _ in 0..5 {
        add_mana(&mut runner, ManaType::Red, 1);
    }

    let card_id = runner.state().objects[&phoenix].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: phoenix,
        card_id,
        targets: vec![],
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    });

    assert!(
        result.is_err(),
        "graveyard permission says using bestow; without a legal Aura target it must not fall back to a normal creature cast"
    );
}
