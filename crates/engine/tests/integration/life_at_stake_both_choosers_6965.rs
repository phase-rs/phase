//! Issue #6965 (follow-up): Life at Stake — *"You and target creature's
//! controller each secretly choose a number 0 or greater."*
//!
//! The compound subject's second conjunct TARGETS: it names a player through an
//! announced object. Unioning it into one subject filter would lose the target
//! binding, so the parser instead splits the shared predicate into two chained
//! halves and declares the announced creature with a slot-only
//! `Effect::TargetOnly` head (CR 115.1). The half for "you" is the unscoped
//! resolver default (CR 109.5); the half for "target creature's controller"
//! binds its acting player on the ABILITY via
//! `player_scope: ParentObjectTargetController` (CR 109.4), because
//! `Effect::Choose` carries no recipient field of its own.
//!
//! This test drives the REAL parse → cast → resolution pipeline and asserts the
//! only thing that matters at runtime: TWO number choices are raised, and the
//! second one prompts the TARGETED creature's controller, not the caster.
//!
//! Fail-on-revert: before the fix, the whole sentence lowered to
//! `Effect::Unimplemented { name: "unbound_subject" }` — no target slot, no
//! `NamedChoice` at all, so both assertions below fail.
//!
//! CR 109.4: only objects on the stack or battlefield have a controller — the
//! anchor "target creature's controller" reads.
//! CR 109.5: "you" on an object refers to that object's controller.
//! CR 115.1: targets are declared as the spell is put on the stack.
//! CR 601.2c: the caster announces a legal object for each target the spell
//! requires.
//! CR 608.2c: the controller follows the instructions in the order written.
//! CR 608.2d: a choice offered by a resolving spell is announced while applying
//! the effect.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Oracle text (MTGJSON). A paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const LIFE_AT_STAKE: &str = "You and target creature's controller each secretly choose a number 0 or greater. Then, reveal the chosen numbers. If your number was highest or tied for the highest, exile that creature. Each player who chose the highest number loses that much life.";

#[test]
fn life_at_stake_prompts_the_caster_then_the_targets_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B", "Lib C"]);
    scenario.with_library_top(P1, &["Lib D", "Lib E", "Lib F"]);

    // The announced target is a creature P1 controls, so "target creature's
    // controller" is a player DIFFERENT from the caster — the discriminating
    // setup. A caster-controlled creature would let a wrong `Controller`
    // binding pass.
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut spell_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Life at Stake", true, LIFE_AT_STAKE);
    spell_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Black],
    });
    let spell = spell_builder.id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, spell, false, vec![])],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Life at Stake must start");

    // Every player prompted for a number, in the order the engine raised the
    // prompts. This is the whole assertion surface: who chooses.
    let mut number_choosers: Vec<PlayerId> = Vec::new();

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { target_slots, .. } => {
                // CR 115.1 + CR 601.2c: the possessive subject must have
                // declared a real creature slot. Pre-fix there was none.
                assert_eq!(
                    target_slots.len(),
                    1,
                    "the announced creature must be exactly one target slot"
                );
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(victim)),
                    "P1's creature must be a legal target; got {:?}",
                    target_slots[0].legal_targets
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(victim)],
                    })
                    .expect("selecting P1's creature must succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("mana payment must auto-finalize");
            }
            WaitingFor::NamedChoice {
                player,
                choice_type,
                options,
                ..
            } => {
                if matches!(choice_type, ChoiceType::NumberRange { .. }) {
                    number_choosers.push(player);
                }
                let choice = options
                    .first()
                    .cloned()
                    .expect("a number choice must offer options");
                runner
                    .act(GameAction::ChooseOption { choice })
                    .expect("answering the number choice must succeed");
                if number_choosers.len() == 2 {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    // CR 109.4 + CR 109.5: both conjuncts choose, and they are DIFFERENT
    // players — the caster, then the targeted creature's controller. A single
    // prompt (the pre-fix gap) or two prompts both aimed at P0 (an unbound
    // chooser) both fail here.
    assert_eq!(
        number_choosers,
        vec![P0, P1],
        "Life at Stake must prompt the caster and then the target creature's controller"
    );
}
