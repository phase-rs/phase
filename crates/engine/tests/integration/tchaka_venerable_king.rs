//! Full engine coverage for "T'Chaka, Venerable King" (set `msc`, Scryfall
//! oracle_id `2a9f2d69-328c-46f4-be37-60b85b197b72`).
//!
//! Oracle text (verbatim):
//!   "When T'Chaka enters, mill three cards, then you may put an artifact or
//!    land card from among the milled cards into your hand.
//!    {3}, Exile this card from your graveyard: You become the monarch.
//!    Activate only if you control your commander."
//!
//! These tests drive the REAL parse -> synthesis -> apply pipeline (no
//! AST-shape assertions). Two mechanics are covered:
//!
//!  1. The graveyard-activated monarch ability's commander-referential
//!     activation restriction. This change teaches the parser/restriction layers
//!     to represent "Activate only if you control your commander" as
//!     `ParsedCondition::ControlsCommander { ownership: Own }`
//!     (CR 903.3 + CR 109.5 — owner-scoped) instead of dropping it to
//!     `Effect::Unimplemented`. Runtime evaluation delegates to the single
//!     `game::commander` authority.
//!
//!     The DISCRIMINATING fixture is the stolen-commander case: a player who
//!     controls an opponent's commander but not their own must NOT satisfy "your
//!     commander" (CR 903.3d is any-owner; "your commander" is owner-scoped).
//!     This fails if the runtime arm delegates to `controls_any_commander`, and
//!     the "no restriction at all" cases fail if the converter reverts to
//!     leaving the clause `Unimplemented`.
//!
//!  2. The ETB "mill three, then you may put an artifact or land card from among
//!     the milled cards into your hand" (CR 701.17c) — a regression guard on the
//!     already-correct tracked-set pipeline.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text. The self-reference "T'Chaka" is normalized against the
/// full card name by the same synthesis pipeline production uses.
const TCHAKA_ORACLE: &str = "When T'Chaka enters, mill three cards, then you may put an artifact or land card from among the milled cards into your hand.\n{3}, Exile this card from your graveyard: You become the monarch. Activate only if you control your commander.";

const TCHAKA_NAME: &str = "T'Chaka, Venerable King";

/// `n` units of colorless mana in the pool — funds the `{3}` activation cost.
fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// How the acting player's commander is positioned for the activation-gate tests.
#[derive(Clone, Copy)]
enum CommanderSetup {
    /// A commander on the battlefield, owned AND controlled by P0 (Lieutenant
    /// gate ON — CR 903.3 + CR 109.5).
    OwnOnBattlefield,
    /// A commander on the battlefield controlled by P0 but OWNED by P1 — a
    /// stolen opponent's commander. Satisfies "a commander" (CR 903.3d) but NOT
    /// "your commander" (CR 109.5).
    StolenOnly,
    /// P0's own commander sitting in the command zone (not on the battlefield),
    /// so it is not a permanent P0 controls (CR 903.3d requires the battlefield).
    OwnInCommandZone,
}

/// Build a scenario with T'Chaka in P0's graveyard, `{3}` funded, and P0's
/// commander positioned per `setup`. Returns `(runner, tchaka_id, commander_id)`
/// — the commander id is returned in every case so tests can reach-guard that the
/// fixture staged the commander it claims (owner, zone) before asserting the gate.
fn graveyard_scenario(setup: CommanderSetup) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tchaka = scenario
        .add_creature_to_graveyard(P0, TCHAKA_NAME, 2, 2)
        .from_oracle_text(TCHAKA_ORACLE)
        .id();

    scenario.with_mana_pool(P0, floating_colorless(3));

    // Stage the commander (always owned by P0 initially). The command-zone case is
    // moved before build; the battlefield cases are flagged after build (the
    // commander flag / stolen ownership are not builder-exposed).
    let commander = scenario.add_creature(P0, "Regal Vanguard", 3, 3).id();
    let on_battlefield = match setup {
        CommanderSetup::OwnOnBattlefield | CommanderSetup::StolenOnly => true,
        CommanderSetup::OwnInCommandZone => {
            scenario.with_commander(commander); // moves it to the command zone
            false
        }
    };

    let mut runner = scenario.build();

    if on_battlefield {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&commander)
            .expect("commander object exists");
        obj.is_commander = true;
        if matches!(setup, CommanderSetup::StolenOnly) {
            // Controlled by P0 (from add_creature) but owned by P1: it is P1's
            // commander, not P0's.
            obj.owner = P1;
        }
    }

    (runner, tchaka, commander)
}

/// Whether the AI candidate generator offers T'Chaka's graveyard activation.
fn activation_offered(runner: &GameRunner, tchaka: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(|a| {
        matches!(
            a,
            GameAction::ActivateAbility { source_id, .. } if *source_id == tchaka
        )
    })
}

/// CR 903.3 + CR 109.5: "Activate only if you control your commander" is
/// owner-scoped. Own-on-battlefield activates and makes P0 the monarch; a
/// stolen opponent's commander and an own commander in the command zone both
/// leave the ability un-activatable.
#[test]
fn tchaka_monarch_activation_gated_on_owning_commander() {
    // (a) POSITIVE reach-guard: own commander on the battlefield. The gate can
    // pass and the ability resolves — proving the negative cases below are not
    // vacuous.
    let (mut runner, tchaka, commander) = graveyard_scenario(CommanderSetup::OwnOnBattlefield);
    // Fixture reach-guard: P0's own commander really is a battlefield permanent.
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Battlefield,
        "fixture: the own commander must be on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&commander].owner,
        P0,
        "fixture: the own commander must be owned by P0"
    );
    assert!(
        activation_offered(&runner, tchaka),
        "own commander on the battlefield: the graveyard activation must be legal"
    );
    let outcome = runner.activate(tchaka, 0).pay_with(&[tchaka]).resolve();
    assert_eq!(
        outcome.state().monarch,
        Some(P0),
        "CR 725.1: resolving BecomeMonarch makes P0 the monarch"
    );
    // CR 602.1a: "Exile this card from your graveyard" is part of the activation
    // cost (everything before the colon), so T'Chaka ends up in exile.
    outcome.assert_zone(&[tchaka], Zone::Exile);

    // (b) THE DISCRIMINATOR: only a stolen opponent's commander. "your commander"
    // (CR 109.5) is not satisfied — fails if the runtime arm uses
    // `controls_any_commander`, or if the restriction reverted to Unimplemented.
    let (mut runner, tchaka, commander) = graveyard_scenario(CommanderSetup::StolenOnly);
    // Fixture reach-guard: the commander is on the battlefield but OWNED by P1, so
    // "your commander" (owner-scoped) must be the thing failing — not a mis-stage.
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Battlefield,
        "fixture: the stolen commander must be on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&commander].owner,
        P1,
        "fixture: the stolen commander must be owned by the opponent (P1)"
    );
    assert!(
        !activation_offered(&runner, tchaka),
        "stolen commander (owned by opponent): 'your commander' is owner-scoped — must be illegal"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: tchaka,
                ability_index: 0,
            })
            .is_err(),
        "the apply path must also reject activation with only a stolen commander"
    );

    // (c) Own commander in the command zone (not a controlled permanent).
    let (mut runner, tchaka, commander) = graveyard_scenario(CommanderSetup::OwnInCommandZone);
    // Fixture reach-guard: P0's OWN commander really is sitting in the command zone
    // (CR 903.3d requires the battlefield), so the negative below tests the gate,
    // not a fixture that silently failed to place the commander.
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Command,
        "fixture: the commander must be in the command zone"
    );
    assert!(
        runner.state().objects[&commander].is_commander,
        "fixture: the command-zone object must be flagged as a commander"
    );
    assert_eq!(
        runner.state().objects[&commander].owner,
        P0,
        "fixture: the command-zone commander must be P0's own"
    );
    assert!(
        !activation_offered(&runner, tchaka),
        "commander in the command zone: not a permanent you control — must be illegal"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: tchaka,
                ability_index: 0,
            })
            .is_err(),
        "the apply path must also reject activation with the commander in the command zone"
    );
}

const DEADLY_ROLLICK_ORACLE: &str =
    "If you control a commander, you may cast this spell without paying its mana cost.\nExile target creature.";

const DEADLY_ROLLICK_NAME: &str = "Deadly Rollick";

#[derive(Clone, Copy)]
enum AnyCommanderSetup {
    OwnOnBattlefield,
    StolenOnBattlefield,
    NoCommander,
}

fn deadly_rollick_scenario(setup: AnyCommanderSetup) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rollick = scenario
        .add_spell_to_hand_from_oracle(P0, DEADLY_ROLLICK_NAME, true, DEADLY_ROLLICK_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .id();
    let bystander = scenario.add_creature(P1, "Bystander", 1, 1).id();

    let battlefield_commander = match setup {
        AnyCommanderSetup::OwnOnBattlefield | AnyCommanderSetup::StolenOnBattlefield => {
            Some(scenario.add_creature(P0, "Regal Vanguard", 3, 3).id())
        }
        AnyCommanderSetup::NoCommander => None,
    };

    let mut runner = scenario.build();
    if let Some(commander) = battlefield_commander {
        let commander = runner
            .state_mut()
            .objects
            .get_mut(&commander)
            .expect("commander object exists");
        commander.is_commander = true;
        if matches!(setup, AnyCommanderSetup::StolenOnBattlefield) {
            commander.owner = P1;
        }
    }

    (runner, rollick, bystander)
}

fn ordinary_cast_offered(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(|action| {
        matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == spell
        )
    })
}

#[test]
fn deadly_rollick_ordinary_cast_offer_requires_controlling_any_commander() {
    let (runner, rollick, _) = deadly_rollick_scenario(AnyCommanderSetup::OwnOnBattlefield);
    assert!(
        ordinary_cast_offered(&runner, rollick),
        "an owned commander must enable the ordinary CastSpell offer with an empty mana pool"
    );

    let (runner, rollick, _) = deadly_rollick_scenario(AnyCommanderSetup::NoCommander);
    assert!(
        !ordinary_cast_offered(&runner, rollick),
        "without a commander, Deadly Rollick's unpayable printed cost must not be offered"
    );
}

#[test]
fn deadly_rollick_stolen_commander_enables_ordinary_free_cast_and_resolution() {
    let (mut runner, rollick, bystander) =
        deadly_rollick_scenario(AnyCommanderSetup::StolenOnBattlefield);
    assert!(
        ordinary_cast_offered(&runner, rollick),
        "a controlled opponent-owned commander must enable the ordinary CastSpell offer"
    );

    let outcome = runner
        .cast(rollick)
        .accept_optional()
        .target_object(bystander)
        .resolve();

    outcome.assert_zone(&[bystander], Zone::Exile);
}

/// Stage three cards atop P0's library: an artifact and a land (both eligible for
/// the "artifact or land card" filter) and a sorcery (ineligible). Types are set
/// via double-cast so each card carries exactly one core type.
fn stage_milled_library(scenario: &mut GameScenario) -> (ObjectId, ObjectId, ObjectId) {
    let artifact = scenario
        .add_spell_to_library_top(P0, "Milled Artifact", false)
        .as_creature()
        .as_artifact()
        .id();
    let land = scenario
        .add_spell_to_library_top(P0, "Milled Land", false)
        .as_creature()
        .as_land()
        .id();
    // A plain sorcery: neither artifact nor land, so the ETB filter excludes it.
    let dud = scenario
        .add_spell_to_library_top(P0, "Milled Sorcery", false)
        .id();
    (artifact, land, dud)
}

/// Put T'Chaka into P0's hand with its real `{G}{W}` cost and fund the pool so
/// the cast auto-pays.
fn hand_tchaka(scenario: &mut GameScenario) -> ObjectId {
    let tchaka = scenario
        .add_creature_to_hand_from_oracle(P0, TCHAKA_NAME, 2, 2, TCHAKA_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ],
    );
    tchaka
}

/// CR 701.17c: the ETB mills three, then the controller may move an artifact or
/// land card from among the milled cards to hand. Selecting the artifact moves
/// exactly it; the land and the ineligible sorcery stay in the graveyard.
#[test]
fn tchaka_etb_mill_then_put_selected_artifact_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (artifact, land, dud) = stage_milled_library(&mut scenario);
    let tchaka = hand_tchaka(&mut scenario);

    let mut runner = scenario.build();

    // Cast T'Chaka; its ETB trigger mills the three staged cards, and we pick the
    // artifact from among the milled cards. The "artifact in hand" assertion is
    // the reach-guard: if the tracked-set filter failed to offer the artifact,
    // the up-to-one choice (min 0) would submit nothing and this would fail.
    let outcome = runner.cast(tchaka).effect_zone(&[artifact]).resolve();

    outcome.assert_zone(&[tchaka], Zone::Battlefield);
    outcome.assert_zone(&[artifact], Zone::Hand);
    outcome.assert_zone(&[land, dud], Zone::Graveyard);
}

/// Declining the optional "you may put ... into your hand" leaves all three
/// milled cards in the graveyard (CR 701.17a — they were milled; the optional
/// move is skipped).
#[test]
fn tchaka_etb_mill_then_decline_keeps_all_milled_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (artifact, land, dud) = stage_milled_library(&mut scenario);
    let tchaka = hand_tchaka(&mut scenario);

    let mut runner = scenario.build();

    // Resolve the cast without declaring an effect-zone pick — the driver halts
    // at the up-to-one `EffectZoneChoice`.
    let outcome = runner.cast(tchaka).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::EffectZoneChoice { .. }
        ),
        "the optional 'you may put ...' surfaces an up-to-one EffectZoneChoice, got {:?}",
        outcome.final_waiting_for()
    );

    // Decline by submitting an empty selection (up_to => min 0).
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the optional put must be accepted");

    for card in [artifact, land, dud] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Graveyard,
            "declined milled card {} must remain in the graveyard",
            card.0
        );
    }
    assert!(
        !runner.state().players[P0.0 as usize]
            .hand
            .iter()
            .any(|&c| c == artifact || c == land || c == dud),
        "no milled card may reach hand when the optional put is declined"
    );

    // Prove the decline COMPLETED the cast/ETB flow rather than stalling on the
    // consumed prompt: the ETB choice was drained (priority, no lingering choice),
    // the stack is empty, and T'Chaka actually entered the battlefield. A stalled
    // continuation would leave the graveyard assertions above true while failing
    // here.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "after declining, the ETB choice must be consumed and priority restored, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "after declining, T'Chaka's spell/ETB must have fully resolved off the stack"
    );
    assert_eq!(
        runner.state().objects[&tchaka].zone,
        Zone::Battlefield,
        "T'Chaka must have entered the battlefield once its cast/ETB completed"
    );
}
