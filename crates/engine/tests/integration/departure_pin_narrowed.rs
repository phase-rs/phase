//! Narrowed departure pin: announcement epoch without trigger provenance.
//!
//! CR 113.7a + CR 602.2a + CR 608.2h + CR 400.7: an activated ability pins its
//! announcement-time source identity (id + `announced_source_incarnation`) at
//! announcement and never builds a `TriggerSourceContext`. `source_incarnation`
//! keeps the post-cost stack-push epoch; plot never stack-pushes so it stays
//! `None` there. These tests lock the three observable legs through real cards
//! with verbatim Oracle text (pool export `data/card-data.json`):
//! - Anointer Priest: "Whenever a creature token you control enters, you gain
//!   1 life.\nEmbalm {3}{W} ({3}{W}, Exile this card from your graveyard:
//!   Create a token that's a copy of it, except it's a white Zombie Human
//!   Cleric with no mana cost. Embalm only as a sorcery.)"
//! - Djinn of Fool's Fall: "Flying\nPlot {3}{U} (You may pay {3}{U} and exile
//!   this card from your hand. Cast it as a sorcery on a later turn without
//!   paying its mana cost. Plot only as a sorcery.)"
//! - Ghost-Lit Raider: "{2}{R}, {T}: This creature deals 2 damage to target
//!   creature.\nChannel — {3}{R}, Discard this card: It deals 4 damage
//!   to target creature."

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::CastingPermission;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ANOINTER_PRIEST_ORACLE: &str = "Whenever a creature token you control enters, you gain 1 life.\nEmbalm {3}{W} ({3}{W}, Exile this card from your graveyard: Create a token that's a copy of it, except it's a white Zombie Human Cleric with no mana cost. Embalm only as a sorcery.)";
const DJINN_OF_FOOLS_FALL_ORACLE: &str = "Flying\nPlot {3}{U} (You may pay {3}{U} and exile this card from your hand. Cast it as a sorcery on a later turn without paying its mana cost. Plot only as a sorcery.)";
const GHOST_LIT_RAIDER_ORACLE: &str = "{2}{R}, {T}: This creature deals 2 damage to target creature.\nChannel \u{2014} {3}{R}, Discard this card: It deals 4 damage to target creature.";

fn pool_mana(runner: &mut GameRunner, color: ManaType, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            color,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// The single activation the engine offers for `source` (every card below
/// carries exactly one activatable ability in its staged zone).
fn offered_activation_index(runner: &GameRunner, source: ObjectId) -> usize {
    let mut indices: Vec<usize> = legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == source => Some(ability_index),
            _ => None,
        })
        .collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(
        indices.len(),
        1,
        "the staged card must offer exactly one activation, got {indices:?}"
    );
    indices[0]
}

/// The unresolved activated ability for `source` on the stack.
fn stacked_ability(
    runner: &GameRunner,
    source: ObjectId,
) -> engine::types::ability::ResolvedAbility {
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the activation must be on the stack"
    );
    let entry = runner.state().stack.front().expect("stack has one entry");
    let StackEntryKind::ActivatedAbility { source_id, ability } = &entry.kind else {
        panic!(
            "expected an activated ability on the stack, got {:?}",
            entry.kind
        );
    };
    assert_eq!(*source_id, source, "the stack entry must name the source");
    (**ability).clone()
}

/// CR 113.7a + CR 602.2a + CR 608.2h: activating Anointer Priest's embalm
/// (exile-self cost from the graveyard) pins the announcement epoch and no
/// trigger provenance. Fails while the pin writes `trigger_source` (the
/// granted-token copy reads drop through `self_ref_binding`); passes with the
/// narrowed pin.
#[test]
fn embalm_activation_pins_announcement_epoch_without_trigger_provenance() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let priest = scenario
        .add_creature_to_graveyard(P0, "Anointer Priest", 1, 3)
        .from_oracle_text(ANOINTER_PRIEST_ORACLE)
        .id();
    let mut runner = scenario.build();
    pool_mana(&mut runner, ManaType::Colorless, 3);
    pool_mana(&mut runner, ManaType::White, 1);

    let announced = runner.state().objects[&priest].incarnation;
    let index = offered_activation_index(&runner, priest);
    runner
        .act(GameAction::ActivateAbility {
            source_id: priest,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating embalm from the graveyard must succeed");

    // CR 118.3: the exile-self cost paid — same id now sits in exile.
    assert_eq!(runner.state().objects[&priest].zone, Zone::Exile);
    let ability = stacked_ability(&runner, priest);
    assert!(
        ability.trigger_source.is_none(),
        "the narrowed pin must not build triggered provenance for an activated ability"
    );
    assert_eq!(
        ability.announced_source_incarnation,
        Some(announced),
        "the pin must carry the pre-cost announcement epoch"
    );
    let live = runner.state().objects[&priest].incarnation;
    assert_ne!(
        live, announced,
        "reach guard: the cost moved the source, so the epochs differ"
    );
    assert_eq!(
        ability.source_incarnation,
        Some(live),
        "the stack push must stamp the post-cost epoch (never the pin)"
    );
}

/// CR 702.170b + CR 113.7a: plotting Djinn of Fool's Fall from hand carries
/// no departure pin — no `trigger_source`, no `source_incarnation` — so the
/// immediate `Plotted` grant resolves through `self_ref_binding`'s fail-open
/// `None` arm. Fails while the pin writes either field (the grant drops and
/// the card is exiled unplotted); passes with the narrowed pin. The passing
/// grant IS the pin-absence proof: `self_ref_binding` only reaches the grant
/// through the `None` arm.
#[test]
fn plot_carries_no_departure_pin_and_grants_plotted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let djinn = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Djinn of Fool's Fall",
            4,
            3,
            DJINN_OF_FOOLS_FALL_ORACLE,
        )
        .id();
    let mut runner = scenario.build();
    pool_mana(&mut runner, ManaType::Colorless, 3);
    pool_mana(&mut runner, ManaType::Blue, 1);

    let index = offered_activation_index(&runner, djinn);
    runner
        .act(GameAction::ActivateAbility {
            source_id: djinn,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("plotting from hand must succeed");

    // CR 702.170b: special action — exiled face up, no stack entry.
    assert_eq!(runner.state().objects[&djinn].zone, Zone::Exile);
    assert!(
        !runner.state().objects[&djinn].face_down,
        "plot exiles face up (CR 702.170 has no face-down clause)"
    );
    assert!(
        runner.state().stack.is_empty(),
        "plot is a special action — it must NOT use the stack"
    );
    assert!(
        runner.state().objects[&djinn]
            .casting_permissions
            .iter()
            .any(|permission| matches!(permission, CastingPermission::Plotted { .. })),
        "the card must be plotted immediately as part of the special action"
    );
}

/// CR 608.2h + CR 400.7: a channel activation is announced in hand and its
/// discard cost moves the source hand-to-graveyard — a departure that records
/// no exit LKI. The pinned incarnation therefore names an epoch with no
/// surviving record, and the damage read degrades to the live fallback:
/// Ghost-Lit Raider's channel still deals its 4 damage. A fail-closed read
/// (no record, no damage) leaves the victim alive; this locks the graceful
/// degradation. Passes with and without the pin — it guards the branch, not
/// the flip.
#[test]
fn channel_damage_from_hand_reads_live_without_exit_record() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raider = scenario
        .add_creature_to_hand_from_oracle(P0, "Ghost-Lit Raider", 2, 1, GHOST_LIT_RAIDER_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 4, 4).id();
    let mut runner = scenario.build();
    pool_mana(&mut runner, ManaType::Colorless, 3);
    pool_mana(&mut runner, ManaType::Red, 1);

    let announced = runner.state().objects[&raider].incarnation;
    let index = offered_activation_index(&runner, raider);
    runner
        .act(GameAction::ActivateAbility {
            source_id: raider,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating channel from hand must succeed");
    // The victim is the only legal creature target, so the announcement binds
    // it without a prompt; the ability waits on the stack at Priority.
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "channel with a single legal target binds it at announcement, got {}",
        runner.waiting_for_kind()
    );

    // The discard cost paid: hand-to-graveyard records no exit LKI.
    assert_eq!(
        runner.state().objects[&raider].zone,
        Zone::Graveyard,
        "the channel discard cost must move the Raider to the graveyard"
    );
    let ability = stacked_ability(&runner, raider);
    assert!(
        ability.trigger_source.is_none(),
        "the narrowed pin must not build triggered provenance for a channel activation"
    );
    assert_eq!(
        ability.announced_source_incarnation,
        Some(announced),
        "the pin must carry the pre-cost hand epoch"
    );
    assert_ne!(
        ability.source_incarnation,
        Some(announced),
        "the post-cost epoch must differ: the cost moved the source"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "CR 608.2h: the 4 channel damage must land via the live fallback — a fail-closed read would leave the 4/4 alive"
    );
}
