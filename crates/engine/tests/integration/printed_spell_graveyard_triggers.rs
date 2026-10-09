//! CR 113.6 + CR 113.6m + CR 603.2c: a spell's standalone "Whenever …" line is
//! a printed triggered ability that functions from the zone its own text names
//! (Killian's Confidence, Thunderblade Charge: the graveyard), not a delayed
//! trigger the spell creates on resolution.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const KILLIANS_CONFIDENCE: &str = "Target creature gets +1/+1 until end of turn. Draw a card.\nWhenever one or more creatures you control deal combat damage to a player, you may pay {W/B}. If you do, return this card from your graveyard to your hand.";
const THUNDERBLADE_CHARGE: &str = "Thunderblade Charge deals 3 damage to any target.\nWhenever one or more creatures you control deal combat damage to a player, if this card is in your graveyard, you may pay {2}{R}{R}{R}. If you do, you may cast it without paying its mana cost.";

const P2: PlayerId = PlayerId(2);

#[derive(Debug, Clone, Copy)]
enum CardZone {
    Graveyard,
    Hand,
}

/// Three players. P0's two attackers hit `defenders` (one attacker each), with
/// the printed spell in `zone`. Returns how many of its triggers are on the
/// stack after combat damage, before any resolves.
fn firings_after_combat(text: &str, zone: CardZone, defenders: &[PlayerId]) -> usize {
    let mut scenario = GameScenario::new_n_player(3, 9656);
    scenario.at_phase(Phase::PreCombatMain);
    let card = match zone {
        CardZone::Graveyard => scenario
            .add_spell_to_graveyard(P0, "Printed Spell", false)
            .from_oracle_text(text)
            .id(),
        CardZone::Hand => scenario
            .add_spell_to_hand_from_oracle(P0, "Printed Spell", false, text)
            .id(),
    };
    let attackers: Vec<ObjectId> = defenders
        .iter()
        .enumerate()
        .map(|(i, _)| {
            scenario
                .add_creature(P0, &format!("Attacker {i}"), 2, 2)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    drive_to_declare_attackers(&mut runner);
    let attacks: Vec<_> = attackers
        .iter()
        .zip(defenders)
        .map(|(&id, &defender)| (id, AttackTarget::Player(defender)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
    for _ in 0..64 {
        let fired = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == card)
            .count();
        if fired > 0 {
            return fired;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blockers");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::PostCombatMain => {
                return 0;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }
    panic!("combat never finished");
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }
    panic!("never reached declare attackers");
}

/// CR 113.6m + CR 603.2c: Killian's Confidence functions from the graveyard
/// (its effect moves the card out of the graveyard) and triggers once per
/// player dealt combat damage, as a printed "one or more … to a player"
/// trigger. Two players hit → two firings; one player → one; the card in
/// hand → none.
#[test]
fn killians_confidence_triggers_from_the_graveyard_per_damaged_player() {
    assert_eq!(
        firings_after_combat(KILLIANS_CONFIDENCE, CardZone::Graveyard, &[P1, P2]),
        2,
        "P1 and P2 dealt combat damage"
    );
    assert_eq!(
        firings_after_combat(KILLIANS_CONFIDENCE, CardZone::Graveyard, &[P1]),
        1,
        "only P1 dealt combat damage"
    );
    assert_eq!(
        firings_after_combat(KILLIANS_CONFIDENCE, CardZone::Hand, &[P1, P2]),
        0,
        "the ability doesn't function from the hand"
    );
}

/// CR 113.6 + CR 603.4: Thunderblade Charge's "if this card is in your
/// graveyard" ability triggers from the graveyard, once per damaged player.
#[test]
fn thunderblade_charge_triggers_from_the_graveyard_per_damaged_player() {
    assert_eq!(
        firings_after_combat(THUNDERBLADE_CHARGE, CardZone::Graveyard, &[P1, P2]),
        2
    );
    assert_eq!(
        firings_after_combat(THUNDERBLADE_CHARGE, CardZone::Hand, &[P1, P2]),
        0
    );
}

const SEVENTEEN_YEAR_CICADAS: &str = "Create ten 1/1 white Insect creature tokens with flying. Exile 17-Year Cicadas with seventeen time counters on it.\nSuspend 17\u{2014}{0} (Rather than cast this card from your hand, you may pay {0} and exile it with seventeen time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)\nWhenever you cast a spell, if this card is suspended, remove a time counter from it.";

#[derive(Debug, Clone, Copy)]
enum CicadasState {
    /// In exile with time counters: suspended (CR 702.62b).
    Suspended,
    /// In exile without time counters: not suspended.
    ExiledWithoutCounters,
}

/// Returns (time counters before, time counters after, cards P0 drew from the
/// cast spell) after P0 casts a free "Draw a card." with Cicadas in `state`.
fn cicadas_after_a_cast(state: CicadasState) -> (u32, u32, usize) {
    use engine::types::counter::CounterType;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Drawn Card"]);
    let cicadas = scenario
        .add_spell_to_exile(P0, "17-Year Cicadas", false)
        .from_oracle_text_with_keywords(&["Suspend"], SEVENTEEN_YEAR_CICADAS)
        .id();
    if let CicadasState::Suspended = state {
        scenario.with_counter(cicadas, CounterType::Time, 5);
    }
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw Spell", true, "Draw a card.")
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let time = |r: &GameRunner| {
        r.state().objects[&cicadas]
            .counters
            .get(&CounterType::Time)
            .copied()
            .unwrap_or(0)
    };
    let before = time(&runner);
    let library = runner.state().players[0].library.len();
    runner.cast(draw).resolve();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => break,
        }
    }
    let drew = library - runner.state().players[0].library.len();
    (before, time(&runner), drew)
}

/// CR 702.62b + CR 603.4 + CR 113.6: 17-Year Cicadas' standalone printed
/// "Whenever you cast a spell, if this card is suspended, remove a time counter
/// from it." functions from exile, through the whole-line dispatch (the full
/// card text). Suspended with 5 time counters: casting a spell leaves 4.
/// Control: in exile with no time counters it isn't suspended and nothing
/// happens, while the cast spell still resolved.
#[test]
fn seventeen_year_cicadas_loses_a_time_counter_when_its_owner_casts_a_spell() {
    let (before, after, drew) = cicadas_after_a_cast(CicadasState::Suspended);
    assert_eq!(drew, 1, "reach guard: the cast spell resolved");
    assert_eq!(
        (before, after),
        (5, 4),
        "suspended: one time counter removed"
    );

    let (before, after, drew) = cicadas_after_a_cast(CicadasState::ExiledWithoutCounters);
    assert_eq!(drew, 1, "reach guard: the cast spell resolved");
    assert_eq!((before, after), (0, 0), "not suspended: no trigger");
}
