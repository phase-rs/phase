//! Regression: Vivien's Invocation's reflexive-trigger damage must read the
//! ENTERING creature's power, not the spell's (which has none).
//!
//! Oracle: "Look at the top seven cards of your library. You may put a creature
//! card from among them onto the battlefield. ... When a creature is put onto
//! the battlefield this way, it deals damage equal to its power to target
//! creature an opponent controls."
//!
//! In "it deals damage equal to its power", "it"/"its" is the creature that was
//! just put onto the battlefield (CR 603.2 — the trigger's bound subject), NOT
//! the resolving spell and NOT the damage recipient. The amount must resolve to
//! the entering creature's power.
//!
//! This drives the PRODUCTION-parsed damage clause through the real damage
//! pipeline (`game/effects/deal_damage.rs` → `game/quantity.rs`
//! `resolve_object_pt`), in the reflexive-trigger context the card creates: the
//! resolving ability's source is the spell (a sorcery, no power); the trigger
//! event is the entering creature's zone change (CR 603.7c). The amount's
//! `QuantityRef::Power` scope decides the referent:
//!   - `ObjectScope::Source` → the spell → power 0 (WRONG: recipient takes 0).
//!   - `ObjectScope::Anaphoric` → the trigger-event source (the entering
//!     creature) → power 4 (CORRECT: recipient takes 4).
//!
//! The recipient (toughness 7) and the entering creature (power 4) are sized so
//! the assertion DISCRIMINATES the referent: 4 != 0 (no-effect / wrong Source
//! scope) and 4 != 7 (the recipient's own toughness / a target misread). The
//! amount used is read VERBATIM from the production parse, so reverting the
//! parser change (amount back to `Power{Source}`) flips the recipient's marked
//! damage from 4 to 0 and fails this test.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VIVIEN: &str = "Look at the top seven cards of your library. You may put a creature card \
from among them onto the battlefield. Put the rest on the bottom of your library in a random \
order. When a creature is put onto the battlefield this way, it deals damage equal to its power \
to target creature an opponent controls.";

/// CR 603.2 + CR 208.1 + CR 608.2: the reflexive trigger deals damage equal to
/// the ENTERING creature's power. A power-4 entering creature deals 4 to a 4/7
/// recipient (survives, but marked exactly 4 — discriminating both 0 and 7).
#[test]
fn vivien_invocation_reflexive_trigger_deals_entering_creatures_power() {
    // The damage clause as the PRODUCTION parser lowers it (the amount scope is
    // exactly what the regression touched). Extract it verbatim — do not
    // hand-author the amount.
    let parsed = parse_oracle_text(
        VIVIEN,
        "Vivien's Invocation",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let spell_ability = parsed
        .abilities
        .first()
        .expect("Vivien parses to one spell ability");
    let damage_effect: Effect = (*spell_ability
        .sub_ability
        .as_ref()
        .expect("reflexive damage sub-ability")
        .effect
        .clone())
    .clone();
    assert!(
        matches!(damage_effect, Effect::DealDamage { .. }),
        "the reflexive sub-ability must be DealDamage, got {damage_effect:?}"
    );

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // The entering creature: a 4/4 P0 controls (the one "put onto the
    // battlefield this way"). Power 4 is the value "its power" must read.
    let entering = scenario.add_vanilla(P0, 4, 4);
    // The recipient: an opponent's 4/7. Toughness 7 > 4 so it survives, and 7
    // differs from the entering power (4) so a target-misread (7) is caught.
    let recipient = scenario.add_vanilla(P1, 4, 7);
    // The resolving object: Vivien's Invocation, a sorcery with NO power. If the
    // amount reads `Source`, it reads this object → 0.
    let spell = scenario
        .add_spell_to_graveyard(P0, "Vivien's Invocation", false)
        .id();

    let mut runner = scenario.build();

    // Build the reflexive triggered ability exactly as it resolves: source = the
    // spell (no power), the single chosen recipient as its target, and the
    // trigger event = the entering creature's zone change (CR 603.7c), which
    // seeds `ObjectScope::Anaphoric`'s trigger-event-source referent. The amount
    // (`Power{Anaphoric}` after the fix) is taken verbatim from the production
    // parse; the recipient slot is supplied directly because Vivien's parsed
    // recipient is `ParentTarget` (a separate, out-of-scope concern) — the
    // variable under test is the per-power AMOUNT referent, not the recipient.
    let resolved =
        ResolvedAbility::new(damage_effect, vec![TargetRef::Object(recipient)], spell, P0);
    let entering_snapshot = runner.state().objects[&entering].snapshot_for_zone_change(
        entering,
        Some(Zone::Library),
        Zone::Battlefield,
    );
    let trigger_event = GameEvent::ZoneChanged {
        object_id: entering,
        from: Some(Zone::Library),
        to: Zone::Battlefield,
        record: Box::new(entering_snapshot),
    };

    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P0,
        kind: StackEntryKind::TriggeredAbility {
            source_id: spell,
            ability: Box::new(resolved),
            condition: None,
            trigger_event: Some(trigger_event),
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    });
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Resolve through the real pipeline (CR 608.2) — both players pass priority,
    // the trigger resolves, and `current_trigger_event` is set from the entry.
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    let marked = runner.state().objects[&recipient].damage_marked;
    assert_eq!(
        marked, 4,
        "CR 603.2 + CR 208.1: the recipient must be dealt the ENTERING creature's \
         power (4); got {marked}. 0 means the amount read `Power{{Source}}` (the \
         spell, no power); 7 means it read the recipient's own toughness."
    );
    // The entering creature is the source, not a recipient — it takes no damage.
    assert_eq!(
        runner.state().objects[&entering].damage_marked,
        0,
        "the entering creature is the damage source and must take 0"
    );
}
