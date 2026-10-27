//! Intrinsic permanent statics that previously mis-routed into a kind:"Spell"
//! GenericEffect (never registered with the layer system) now land as
//! top-level `static_abilities` and resolve through the real continuous-effect
//! layer pipeline.
//!
//! Gap A — "All <subtype> have <keyword>" universal-quantifier grant
//! (Crystalline Sliver: "All Slivers have shroud."). CR 205.3 (subtypes),
//! CR 604.1 (static abilities are continuously true), CR 702.18a (Shroud).
//!
//! Gap B — self/typed "can't be blocked except by <filter>" evasion static.
//! CR 509.1b (the defending player checks each creature for block
//! restrictions). The restriction must be live in declare-blockers, so it has
//! to be a top-level continuous static, not a spell-resolution effect.
//!
//! These tests drive the REAL pipeline through `apply()` (priority passing /
//! declare-attackers-blockers) — they never hand-construct expected state. The
//! assertions read the post-layer keyword set / the engine's own
//! `find_legal_targets` and block-legality validation.

use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

/// Crystalline Sliver's printed static (reminder text stripped by the parser).
const CRYSTALLINE_SLIVER: &str = "All Slivers have shroud.";

#[test]
fn regal_full_loaders_preserve_the_complete_granted_if_otherwise_child() {
    let database = super::search_result_conditional_destination::load_finite_printed_records(
        REGAL_SLIVER_PRINTED,
    );
    let face = database.get_face_by_name("Regal Sliver").unwrap();
    assert!(face.abilities.is_empty());
    assert_eq!(face.static_abilities.len(), 1);
    assert!(
        engine::game::coverage::card_face_gaps(face).is_empty(),
        "the entire quoted conditional must be owned: {:?}",
        engine::game::coverage::card_face_gaps(face)
    );
}

#[test]
fn regal_granted_entry_takes_monarch_from_an_opponent_without_pumping() {
    use engine::game::printed_cards::apply_card_face_to_object;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};
    let database = super::search_result_conditional_destination::load_finite_printed_records(
        REGAL_SLIVER_PRINTED,
    );
    let face = database.get_face_by_name("Regal Sliver").unwrap();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let regal = scenario
        .add_creature(P0, "Regal Sliver", 3, 3)
        .with_subtypes(vec!["Sliver"])
        .id();
    let entering = scenario
        .add_creature_to_hand_from_oracle(P0, "Metallic Sliver", 1, 1, "")
        .as_artifact_creature()
        .with_subtypes(vec!["Sliver"])
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let own_decoy = scenario.add_creature(P0, "Own nonsliver", 2, 2).id();
    let opponent = scenario
        .add_creature(P1, "Opponent Sliver", 2, 2)
        .with_subtypes(vec!["Sliver"])
        .id();
    let mut runner = scenario.build();
    apply_card_face_to_object(runner.state_mut().objects.get_mut(&regal).unwrap(), face);
    runner.state_mut().monarch = Some(P1);
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    runner.cast(entering).resolve();
    assert_eq!(
        runner.state().monarch,
        Some(P0),
        "the granted child's otherwise branch must execute for its controller"
    );
    assert_eq!(runner.state().objects[&regal].power, Some(3));
    assert_eq!(runner.state().objects[&entering].power, Some(1));
    assert_eq!(runner.state().objects[&own_decoy].power, Some(2));
    assert_eq!(runner.state().objects[&opponent].power, Some(2));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
}

/// CR 205.3 + CR 604.1 + CR 702.18a: Crystalline Sliver's "All Slivers have
/// shroud" must grant Shroud to OTHER Slivers on the battlefield through the
/// continuous layer system, and that Shroud must make the affected Sliver
/// untargetable by an opponent's spell.
#[test]
fn crystalline_sliver_grants_shroud_to_other_slivers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _crystalline = scenario
        .add_creature_from_oracle(P0, "Crystalline Sliver", 1, 1, CRYSTALLINE_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();
    let other_sliver = scenario
        .add_creature(P0, "Muscle Sliver", 2, 2)
        .with_subtypes(vec!["Sliver"])
        .id();
    // A non-Sliver control creature proves the grant is subtype-selective.
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    // Run the engine's own continuous-effect layer pipeline (the same
    // `evaluate_layers` path `apply()` invokes) so the continuous Shroud grant
    // is written onto the effective keyword set. We mark layers full + flush
    // rather than hand-construct the result.
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    let other = &runner.state().objects[&other_sliver];
    assert!(
        other.has_keyword(&Keyword::Shroud),
        "the other Sliver should have Shroud granted by Crystalline Sliver, \
         got keywords {:?}",
        other.keywords
    );
    assert!(
        !runner.state().objects[&bear].has_keyword(&Keyword::Shroud),
        "a non-Sliver creature must NOT receive the Sliver-only Shroud grant"
    );

    // Discriminating: an opponent (P1) targeting a creature on the battlefield
    // must not be able to select the shrouded Sliver. Source id is irrelevant
    // here (any opponent source is barred by Shroud — CR 702.18a).
    let creature_filter = TargetFilter::Typed(TypedFilter::creature());
    let legal_for_opponent =
        find_legal_targets(runner.state(), &creature_filter, P1, ObjectId(9999));
    assert!(
        !legal_for_opponent.contains(&TargetRef::Object(other_sliver)),
        "the shrouded Sliver must not be a legal target for an opponent's spell"
    );
    // Sanity: the non-Sliver bear (no Shroud) IS still targetable by the opponent.
    assert!(
        legal_for_opponent.contains(&TargetRef::Object(bear)),
        "the un-shrouded bear should remain a legal opponent target"
    );
}

/// Synthetic self-referential evasion static: "~ can't be blocked except by
/// creatures with flying." Mirrors the Crystalline-class root cause for Gap B.
const EXCEPT_BY_FLYING: &str = "~ can't be blocked except by creatures with flying.";

/// CR 509.1b + CR 604.1: An attacker with "can't be blocked except by creatures
/// with flying" must reject a non-flying blocker in declare-blockers and accept
/// a flying one — driven through the real combat pipeline via `apply()`.
#[test]
fn cant_be_blocked_except_by_flying_rejects_nonflying_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario
        .add_creature_from_oracle(P0, "Evasive Striker", 2, 2, EXCEPT_BY_FLYING)
        .id();
    let ground_blocker = scenario.add_creature(P1, "Ground Wall", 0, 4).id();
    let flying_blocker = {
        let mut b = scenario.add_creature(P1, "Air Wall", 0, 4);
        b.flying();
        b.id()
    };

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("attacker should be able to attack");

    // Advance to the declare-blockers window (drain any attack-trigger stack).
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected to reach declare-blockers, got {:?}",
        runner.state().waiting_for
    );

    // CR 509.1b: a non-flying blocker is an illegal block — the declaration
    // must be rejected by the engine's own validation.
    let illegal = runner.act(GameAction::DeclareBlockers {
        assignments: vec![(ground_blocker, attacker)],
    });
    assert!(
        illegal.is_err(),
        "a non-flying creature must NOT be allowed to block a \
         'can't be blocked except by creatures with flying' attacker"
    );
    // The attacker is still attacking (declaration was rejected, not committed).
    assert_eq!(runner.state().objects[&attacker].zone, Zone::Battlefield);

    // A flying blocker IS a legal block.
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(flying_blocker, attacker)],
        })
        .expect("a flying creature should be a legal blocker for the evasion attacker");
}

// Complete unchanged records from pinned MTGJSON 2026-10-04, version 5.3.0+20261004.
const REGAL_SLIVER_PRINTED: &[&str] = &[
    r####"{"colorIdentity":["W"],"colors":["W"],"convertedManaCost":4.0,"edhrecRank":9914,"edhrecSaltiness":0.57,"foreignData":[{"language":"Chinese Simplified","name":"\u5a01\u4e25\u88c2\u7247\u5996","skuIds":{"foil":"5359e223-e283-5ff6-bca2-55536ae9987f","nonfoil":"9d5a08a1-99aa-5b24-bb4f-43b55af73a96"},"text":"\u7531\u4f60\u64cd\u63a7\u7684\u88c2\u7247\u5996\u751f\u7269\u5177\u6709\u300c\u5f53\u6b64\u751f\u7269\u8fdb\u6218\u573a\u65f6\uff0c\u5982\u679c\u4f60\u662f\u541b\u4e3b\uff0c\u5219\u7531\u4f60\u64cd\u63a7\u7684\u88c2\u7247\u5996\u5f97+1/+1\u76f4\u5230\u56de\u5408\u7ed3\u675f\u3002\u82e5\u5426\uff0c\u5219\u4f60\u6210\u4e3a\u541b\u4e3b\u3002\u300d","type":"\u751f\u7269 \u2014 \u88c2\u7247\u5996"},{"language":"French","name":"Slivo\u00efde royal","skuIds":{"foil":"af9febf9-abf5-5119-be3c-02af0c59b1db","nonfoil":"fc70a637-67f6-54c6-bde9-cea69c1a19a3"},"text":"Les cr\u00e9atures Slivo\u00efde que vous contr\u00f4lez ont \u00ab Quand cette cr\u00e9ature arrive sur le champ de bataille, les slivo\u00efdes que vous contr\u00f4lez gagnent +1/+1 jusqu'\u00e0 la fin du tour si vous \u00eates le monarque. Sinon, vous devenez le monarque. \u00bb","type":"Cr\u00e9ature \u2014 slivo\u00efde"},{"language":"German","name":"K\u00f6niglicher Remasuri","skuIds":{"foil":"3c7d60ad-6b3f-56ad-ade2-14b3d9c63ca7","nonfoil":"ec980ea5-9e3a-5de9-b086-e4e548f025d2"},"text":"Remasuri-Kreaturen, die du kontrollierst, haben \u201eWenn diese Kreatur ins Spiel kommt, erhalten Remasuris, die du kontrollierst, +1/+1 bis zum Ende des Zuges, falls du der Monarch bist. Anderenfalls wirst du zum Monarchen.\"","type":"Kreatur \u2014 Remasuri"},{"language":"Japanese","name":"\u5a01\u53b3\u30b9\u30ea\u30f4\u30a1\u30fc","skuIds":{"foil":"0e26d2d3-fb07-5829-b8b0-35e0d533391c","nonfoil":"4fe238fc-682c-5aee-85a9-88a978e0c8e0"},"text":"\u3042\u306a\u305f\u304c\u30b3\u30f3\u30c8\u30ed\u30fc\u30eb\u3057\u3066\u3044\u308b\u3059\u3079\u3066\u306e\u30b9\u30ea\u30f4\u30a1\u30fc\u30fb\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u306f\u300c\u3053\u306e\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u304c\u6226\u5834\u306b\u51fa\u305f\u3068\u304d\u3001\u3042\u306a\u305f\u304c\u7d71\u6cbb\u8005\u3067\u3042\u308b\u306a\u3089\u3001\u30bf\u30fc\u30f3\u7d42\u4e86\u6642\u307e\u3067\u3001\u3042\u306a\u305f\u304c\u30b3\u30f3\u30c8\u30ed\u30fc\u30eb\u3057\u3066\u3044\u308b\u3059\u3079\u3066\u306e\u30b9\u30ea\u30f4\u30a1\u30fc\u306f\uff0b\uff11/\uff0b\uff11\u306e\u4fee\u6574\u3092\u53d7\u3051\u308b\u3002\u305d\u3046\u3067\u306a\u3044\u306a\u3089\u3001\u3042\u306a\u305f\u304c\u7d71\u6cbb\u8005\u306b\u306a\u308b\u3002\u300d\u3092\u6301\u3064\u3002","type":"\u30af\u30ea\u30fc\u30c1\u30e3\u30fc \u2014 \u30b9\u30ea\u30f4\u30a1\u30fc"}],"identifiers":{"scryfallOracleId":"3a68915d-3e8b-4ede-bdd9-6c138ca63220"},"layout":"normal","legalities":{"commander":"Legal","duel":"Legal","legacy":"Legal","oathbreaker":"Legal","vintage":"Legal"},"manaCost":"{3}{W}","manaValue":4.0,"name":"Regal Sliver","power":"3","printings":["CMM"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/492ab0ca9ba8635e","cardKingdomFoil":"https://mtgjson.com/links/14d30baf0305eac9","cardmarket":"https://mtgjson.com/links/6ab0c6ba8579dbae","cardmarketFoil":"https://mtgjson.com/links/e02162586ce1f306","tcgplayer":"https://mtgjson.com/links/b899e335509d676e"},"rulings":[{"date":"2023-07-28","text":"If combat damage dealt to the monarch causes that player to lose the game, the triggered ability that causes the controller of the attacking creature to become the monarch doesn't resolve. In most cases, the controller of the attacking creature will still become the monarch as it is likely their turn."},{"date":"2023-07-28","text":"If the monarch leaves the game during another player's turn, that player becomes the monarch. If the monarch leaves the game during their turn, the next player in turn order becomes the monarch."},{"date":"2023-07-28","text":"If the triggered ability that causes the monarch to draw a card goes on the stack and a different player becomes the monarch before that ability resolves, the first player will still draw the card."},{"date":"2023-07-28","text":"The game starts with no monarch. Once an effect makes one player the monarch, the game will have exactly one monarch from that point forward. As a player becomes the monarch, the current monarch (if any) ceases being the monarch."},{"date":"2023-07-28","text":"There are two inherent triggered abilities associated with being the monarch. These triggered abilities have no source and are controlled by the player who was the monarch at the time the abilities triggered. The full texts of these abilities are \"At the beginning of the monarch's end step, that player draws a card\" and \"Whenever a creature deals combat damage to the monarch, its controller becomes the monarch.\""}],"subtypes":["Sliver"],"supertypes":[],"text":"Sliver creatures you control have \"When this creature enters, Slivers you control get +1/+1 until end of turn if you're the monarch. Otherwise, you become the monarch.\"","toughness":"3","type":"Creature \u2014 Sliver","types":["Creature"]}"####,
];
