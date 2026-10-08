//! Regression for issue #4966: Waterbender Ascension's activated ability
//! ("Waterbend {4}: Target creature can't be blocked this turn.") never
//! actually made the targeted creature unblockable, because the engine
//! wrongly refused to let the player activate it in the first place.
//!
//! Oracle text and reminder wording were verified against the card and CR
//! 701.67a in `docs/MagicCompRules.txt`:
//! "Whenever a creature you control deals combat damage to a player, put a
//! quest counter on this enchantment. Then if it has four or more quest
//! counters on it, draw a card.
//! Waterbend {4}: Target creature can't be blocked this turn. (While paying
//! a waterbend cost, you can tap your artifacts and creatures to help. Each
//! one pays for {1}.)"
//!
//! Root cause: `AbilityCost::Waterbend`'s `is_payable` affordability
//! pre-check (`crates/engine/src/game/cost_payability.rs`) — consulted by
//! `casting.rs` before an activated ability is even offered as a legal
//! action — delegated to the plain `can_pay_cost_after_auto_tap` helper,
//! which only considers real mana-producing sources (lands, mana rocks) and
//! has no notion of Waterbend's own tap-artifacts-or-creatures-to-help
//! mechanic (CR 601.2b, the entire point of the keyword). A player with
//! zero floating/land mana for the generic leg but plenty of untapped
//! eligible creatures to tap was therefore told the ability wasn't payable
//! at all — the exact "remains blockable" symptom reported, since the
//! ability could never be activated to begin with. Fixed by delegating to
//! `can_feasibly_pay_activation_mana_cost_with_tap_payment_mode` — the
//! `PaymentContext::Activation` sibling of the helper the spell-cast
//! "additional cost: you may waterbend N" path uses — which falls back to
//! the plain auto-tap check first, so pool/land-funded payment is
//! unaffected. The activated ability's `ability_tag` is threaded into that
//! probe so CR 106.6 tag-scoped mana is judged exactly as the real payment
//! step judges it (unit-covered in `game::cost_payability`'s
//! `waterbend_payability_sees_tag_scoped_activation_mana`).
//!
//! No prior test paired a Waterbend-cost *activated* ability with a real
//! target (creature) selection driven through the full
//! `ActivateAbility -> pay cost -> choose target -> resolve` pipeline. This
//! test closes that gap end-to-end and asserts the concrete, observable
//! effect: the targeted creature must actually become unblockable, both
//! when the cost is pool-funded and when it's paid via real tap-to-help.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::combat::{can_block_pair, AttackTarget};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaRestriction, ManaType, ManaUnit};
use engine::types::phase::Phase;

const WATERBENDER_ASCENSION_ORACLE: &str = "Whenever a creature you control deals combat damage to a player, put a quest counter on this enchantment. Then if it has four or more quest counters on it, draw a card.\nWaterbend {4}: Target creature can't be blocked this turn.";

#[test]
fn waterbend_activated_ability_makes_targeted_creature_unblockable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ascension = scenario
        .add_creature(P0, "Waterbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(WATERBENDER_ASCENSION_ORACLE)
        .with_mana_cost(ManaCost::NoCost)
        .id();

    let attacker = scenario.add_creature(P0, "Swift Raider", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();

    // Fund the Waterbend {4} generic leg straight from the mana pool. This
    // doesn't exercise the affordability bug itself (pool-funded payment
    // was never broken -- see the sibling `waterbend_tap_to_help_...` test
    // for that), but it's a faithful, minimal baseline that the ability's
    // targeting and effect application work at all before layering the
    // tap-to-help payment path on top.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 4],
    );

    let mut runner = scenario.build();

    assert!(
        can_block_pair(runner.state(), blocker, attacker),
        "sanity check: the blocker must be a legal block target before the \
         ability resolves"
    );

    runner
        .activate(ascension, 0)
        .target_object(attacker)
        .resolve();

    assert!(
        !can_block_pair(runner.state(), blocker, attacker),
        "issue #4966: \"Waterbend {{4}}: Target creature can't be blocked \
         this turn.\" must make the targeted creature unblockable"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    assert!(
        runner.declare_blockers(&[(blocker, attacker)]).is_err(),
        "declaring the unblockable creature as blocked must be rejected"
    );
}

/// Same regression, but paying the Waterbend {4} cost via the actual
/// tap-to-help mechanic (`GameAction::TapForConvoke`) instead of a pre-funded
/// mana pool -- the mechanic the reminder text and issue are actually about.
/// `AbilityActivation::resolve()`'s sugar only knows how to finalize
/// `ManaPayment` via a bare `PassPriority` (see its doc comment), so this
/// drives the lower-level `GameAction` sequence directly, mirroring
/// `secret_of_bloodbending_control_window.rs`'s hand-rolled driver loop.
#[test]
fn waterbend_tap_to_help_makes_targeted_creature_unblockable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ascension = scenario
        .add_creature(P0, "Waterbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(WATERBENDER_ASCENSION_ORACLE)
        .with_mana_cost(ManaCost::NoCost)
        .id();

    let attacker = scenario.add_creature(P0, "Swift Raider", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();

    // Four otherwise-uninvolved creatures to tap for the Waterbend {4} cost --
    // no floating mana at all, so the ManaPayment window can ONLY be finished
    // by tapping.
    let helpers: Vec<ObjectId> = (0..4)
        .map(|i| scenario.add_creature(P0, &format!("Helper {i}"), 1, 1).id())
        .collect();

    let mut runner = scenario.build();

    assert!(
        can_block_pair(runner.state(), blocker, attacker),
        "sanity check: the blocker must be a legal block target before the \
         ability resolves"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: ascension,
            ability_index: 0,
        })
        .expect("ActivateAbility must be accepted");

    let mut tapped_for_cost = 0;
    let mut target_chosen = false;
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                if tapped_for_cost < helpers.len() {
                    runner
                        .act(GameAction::TapForConvoke {
                            object_id: helpers[tapped_for_cost],
                            mana_type: ManaType::Colorless,
                        })
                        .expect("TapForConvoke must be accepted");
                    tapped_for_cost += 1;
                } else {
                    runner
                        .act(GameAction::PassPriority)
                        .expect("finalize Waterbend mana payment");
                }
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(attacker)),
                    })
                    .expect("ChooseTarget must be accepted");
                target_chosen = true;
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected WaitingFor::{other:?} while paying Waterbend via tap"),
        }
    }

    assert_eq!(tapped_for_cost, 4, "must tap all 4 helpers to pay {{4}}");
    assert!(target_chosen, "target selection must have been reached");
    for helper in &helpers {
        assert!(
            runner.state().objects[helper].tapped,
            "each helper creature must be tapped after paying its Waterbend leg"
        );
    }

    // Pass priority on both players so the ability resolves off the stack.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to resolve the ability");

    assert!(
        !can_block_pair(runner.state(), blocker, attacker),
        "issue #4966: \"Waterbend {{4}}: Target creature can't be blocked \
         this turn.\" must make the targeted creature unblockable even when \
         the cost is paid via tap-to-help, not a pre-funded mana pool"
    );
}

// ---------------------------------------------------------------------------
// Payment-context boundary — maintainer review on PR #6097
// ---------------------------------------------------------------------------
//
// CR 106.6: the affordability pre-check for an ACTIVATED ability must consult
// the activation half of every mana restriction, exactly like the later
// payment step (`PaymentContext::Activation`), never the spell half. The two
// halves diverge in both directions:
//   - activation-only mana (`ManaRestriction::OnlyForActivation`) counts for
//     the activation but not for a spell;
//   - spell-only mana (`ManaRestriction::OnlyForSpell`) counts for a spell
//     but not for the activation.
// Probing the Waterbend activation with a SPELL context therefore (a)
// suppressed a payable activation when the player's only funding was
// activation-only mana, and (b) offered an unpayable activation when the only
// funding was spell-only mana. Both directions are pinned below; the runtime
// gate surfaces as `ActivateAbility` being accepted vs. rejected
// ("Cannot pay activation cost").

/// Direction (a): activation-only restricted mana MUST make the Waterbend
/// activation available — it is fully spendable by the actual activation
/// payment. With the old spell-context probe, `allows_spell(OnlyForActivation)`
/// is false, no other funding exists, and the activation was wrongly rejected.
#[test]
fn waterbend_activation_only_mana_makes_ability_available() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ascension = scenario
        .add_creature(P0, "Waterbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(WATERBENDER_ASCENSION_ORACLE)
        .with_mana_cost(ManaCost::NoCost)
        .id();

    let attacker = scenario.add_creature(P0, "Swift Raider", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();

    // The ONLY funding: 4 colorless restricted to ability activation. No
    // helpers to tap, no other mana sources.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                ObjectId(9_999),
                false,
                vec![ManaRestriction::OnlyForActivation],
            );
            4
        ],
    );

    let mut runner = scenario.build();

    runner
        .activate(ascension, 0)
        .target_object(attacker)
        .resolve();

    assert!(
        !can_block_pair(runner.state(), blocker, attacker),
        "activation-only mana must fund the Waterbend activation: the \
         affordability gate must consult allows_activation, not allows_spell"
    );
}

/// Direction (b): spell-only restricted mana must NOT make the Waterbend
/// activation available — the actual payment step could never spend it. With
/// the old spell-context probe, `allows_spell(OnlyForSpell)` is true and the
/// gate offered an activation the payment pipeline must reject.
#[test]
fn waterbend_spell_only_mana_does_not_make_ability_available() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ascension = scenario
        .add_creature(P0, "Waterbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(WATERBENDER_ASCENSION_ORACLE)
        .with_mana_cost(ManaCost::NoCost)
        .id();

    scenario.add_creature(P0, "Swift Raider", 2, 2);

    // The ONLY funding: 4 colorless restricted to spell casting. No helpers
    // to tap, no other mana sources — the activation is genuinely unpayable.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                ObjectId(9_999),
                false,
                vec![ManaRestriction::OnlyForSpell],
            );
            4
        ],
    );

    let mut runner = scenario.build();

    let result = runner.act(GameAction::ActivateAbility {
        source_id: ascension,
        ability_index: 0,
    });
    assert!(
        result.is_err(),
        "spell-only mana must not satisfy the activation affordability gate — \
         offering this activation would strand the player in an unpayable \
         ManaPayment window; got {result:?}"
    );
}

/// Tag-scoped sibling of direction (a), at the OFFER gate — maintainer review
/// on PR #6097, round 2.
///
/// CR 106.6: `ManaRestriction::OnlyForTaggedActivation(tag)` mana is spendable
/// only when the activated ability's `ability_tag` matches. There are TWO
/// activation affordability authorities:
///   - the submit gate (`handle_activate_ability` →
///     `activation_cost_passes_early_affordability_gate` →
///     `is_payable_for_activation`), and
///   - the offer gate (`can_activate_ability_now` → `can_pay_ability_cost_now`
///     → `costs::can_pay`'s `PaymentScope::Activation` arm).
///
/// If the offer gate drops the tag (probing `is_payable` with `None`),
/// tag-scoped mana is invisible to it: the ability is never generated as a
/// legal action for the UI/AI even though submitting `ActivateAbility`
/// directly would succeed — the same false-unactivatable symptom class as
/// issue #4966, one gate upstream. This test therefore asserts the activation
/// is OFFERED (via `legal_actions` and `can_activate_ability_now`), not
/// merely accepted on submit, then drives it end to end.
#[test]
fn tagged_waterbend_with_tag_scoped_mana_is_offered_as_legal_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A PowerUp-tagged Waterbend ability, built through the real parser
    // ("Power-up — <cost>: <effect>" sets `ability_tag = Some(PowerUp)` and
    // parses the cost text as `AbilityCost::Waterbend`).
    let ascension = scenario
        .add_creature(P0, "Waterbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(
            "Power-up \u{2014} Waterbend {4}: Target creature can't be blocked this turn.",
        )
        .with_mana_cost(ManaCost::NoCost)
        .id();

    let attacker = scenario.add_creature(P0, "Swift Raider", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();

    // The ONLY funding: 4 colorless restricted to PowerUp-tagged activations.
    // No helpers to tap (the two creatures are needed as target/blocker, and
    // tap-to-help is not what is under test), no other mana sources.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                ObjectId(9_999),
                false,
                vec![ManaRestriction::OnlyForTaggedActivation(
                    AbilityTag::PowerUp
                )],
            );
            4
        ],
    );

    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&ascension]
        .abilities
        .iter()
        .position(|a| a.ability_tag == Some(AbilityTag::PowerUp))
        .expect("parser must produce a PowerUp-tagged activated ability");

    assert!(
        can_activate_ability_now(runner.state(), P0, ascension, ability_index),
        "offer gate: can_activate_ability_now must see PowerUp-tag-scoped mana \
         as funding for a PowerUp-tagged Waterbend activation (costs::can_pay's \
         activation arm must thread ability_tag into is_payable_for_activation)"
    );
    assert!(
        legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility { source_id, ability_index: idx }
                if *source_id == ascension && *idx == ability_index
        )),
        "legal-action generation must OFFER the tagged Waterbend activation \
         when its only funding is OnlyForTaggedActivation(PowerUp) mana"
    );

    // And the offered action is genuinely playable end to end.
    runner
        .activate(ascension, ability_index)
        .target_object(attacker)
        .resolve();

    assert!(
        !can_block_pair(runner.state(), blocker, attacker),
        "the offered tagged Waterbend activation must resolve: tag-scoped mana \
         funds it and the targeted creature becomes unblockable"
    );
}
