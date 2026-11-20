//! Regression for issues #11133 and #11194: an X that a "gets ±X/…" pump adds
//! is floored at 0, whatever defines it, so a negative characteristic never
//! lowers the recipient.
//!
//! https://github.com/phase-rs/phase/issues/11133
//! https://github.com/phase-rs/phase/issues/11194
//!
//! CR 107.1b: "If a calculation that would determine the result of an effect
//! yields a negative number, zero is used instead, unless that effect doubles,
//! triples, or sets to a specific value a player's life total or the power
//! and/or toughness of a creature or creature card."

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const NANTUKO_MENTOR: &str =
    "{2}{G}, {T}: Target creature gets +X/+X until end of turn, where X is that creature's power.";
const YEW_SPIRIT: &str =
    "{2}{G}{G}: This creature gets +X/+X until end of turn, where X is its power.";
const STALACTITE_STALKER: &str = "Menace\nAt the beginning of your end step, if you descended this turn, put a +1/+1 counter on this creature. (You descended if a permanent card was put into your graveyard from anywhere.)\n{2}{B}, Sacrifice this creature: Target creature gets -X/-X until end of turn, where X is this creature's power.";
const AURIOK_BLADEWARDEN: &str =
    "{T}: Target creature gets +X/+X until end of turn, where X is this creature's power.";
const DEATHS_SHADOW: &str = "This creature gets -X/-X, where X is your life total.";
const PLATINUM_ANGEL: &str =
    "Flying\nYou can't lose the game and your opponents can't win the game.";
const SIGN_IN_BLOOD: &str = "Target player draws two cards and loses 2 life.";
const OVERWHELMING_STAMPEDE: &str = "Until end of turn, creatures you control gain trample and get +X/+X, where X is the greatest power among creatures you control.";
const COALITION_SKYKNIGHT: &str = "Flying\nEnlist (As this creature attacks, you may tap a nonattacking creature you control without summoning sickness. When you do, add its power to this creature's until end of turn.)";
const BEFUDDLE: &str = "Target creature gets -4/-0 until end of turn.\nDraw a card.";

fn mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn pool(parts: &[(usize, ManaType)]) -> Vec<ManaUnit> {
    parts.iter().flat_map(|&(n, ty)| mana(n, ty)).collect()
}

/// "where X is that creature's power" on a target with power -1 adds +0/+0.
#[test]
fn nantuko_mentor_negative_target_power_adds_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(P0, pool(&[(2, ManaType::Colorless), (1, ManaType::Green)]));
    let mentor = s
        .add_creature_from_oracle(P0, "Nantuko Mentor", 1, 1, NANTUKO_MENTOR)
        .id();
    let neg = s.add_creature(P0, "Negative", -1, 3).id();
    let mut r = s.build();
    let out = r.activate(mentor, 0).target_object(neg).resolve();
    assert_eq!(out.power_toughness(neg), (-1, 3));
}

/// Yew Spirit ruling: "If this creature's power is negative as its ability
/// resolves, X is considered to be 0."
#[test]
fn yew_spirit_negative_power_adds_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(P0, pool(&[(2, ManaType::Colorless), (2, ManaType::Green)]));
    let yew = s
        .add_creature_from_oracle(P0, "Yew Spirit", -1, 3, YEW_SPIRIT)
        .id();
    let mut r = s.build();
    let out = r.activate(yew, 0).resolve();
    assert_eq!(out.power_toughness(yew), (-1, 3));
}

fn stalker_on(stalker_power: i32) -> (i32, i32) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(P0, pool(&[(2, ManaType::Colorless), (1, ManaType::Black)]));
    let stalker = s
        .add_creature_from_oracle(
            P0,
            "Stalactite Stalker",
            stalker_power,
            1,
            STALACTITE_STALKER,
        )
        .id();
    let bear = s.add_creature(P1, "Bear", 3, 3).id();
    let mut r = s.build();
    let index = r.state().objects[&stalker].abilities.len() - 1;
    let out = r.activate(stalker, index).target_object(bear).resolve();
    out.power_toughness(bear)
}

/// A sacrificed source with power -2 makes "-X/-X" give -0/-0, never +2/+2;
/// a power-2 source still gives -2/-2.
#[test]
fn stalactite_stalker_negative_power_never_pumps_the_target() {
    assert_eq!(stalker_on(2), (1, 1));
    assert_eq!(stalker_on(-2), (3, 3));
}

fn auriok_on(source_power: i32) -> (i32, i32) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let auriok = s
        .add_creature_from_oracle(
            P0,
            "Auriok Bladewarden",
            source_power,
            1,
            AURIOK_BLADEWARDEN,
        )
        .id();
    let bear = s.add_creature(P0, "Bear", 3, 2).id();
    let mut r = s.build();
    let out = r.activate(auriok, 0).target_object(bear).resolve();
    out.power_toughness(bear)
}

/// Brambleguard Captain ruling (same "this creature's power" shape): a negative
/// source power gives the target +0/+0.
#[test]
fn auriok_bladewarden_negative_source_power_gives_nothing() {
    assert_eq!(auriok_on(2), (5, 4));
    assert_eq!(auriok_on(-1), (3, 2));
}

fn deaths_shadow_after_sign_in_blood(life: i32) -> (i32, i32, i32) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_life(P0, life);
    s.with_library_top(P0, &["Card A", "Card B"]);
    s.with_mana_pool(P0, mana(2, ManaType::Black));
    s.add_creature_from_oracle(P0, "Platinum Angel", 4, 4, PLATINUM_ANGEL);
    let shadow = s
        .add_creature_from_oracle(P0, "Death's Shadow", 13, 13, DEATHS_SHADOW)
        .id();
    let sign = s
        .add_spell_to_hand_from_oracle(P0, "Sign in Blood", false, SIGN_IN_BLOOD)
        .id();
    let mut r = s.build();
    let out = r.cast(sign).target_player(P0).resolve();
    let (p, t) = out.power_toughness(shadow);
    let life = out
        .state()
        .players
        .iter()
        .find(|pl| pl.id == P0)
        .unwrap()
        .life;
    assert_eq!(r.waiting_for_kind(), "Priority");
    (life, p, t)
}

/// Death's Shadow ruling: "If your life total is negative, X is considered to
/// be 0." Platinum Angel keeps the game going at -1 life.
#[test]
fn deaths_shadow_negative_life_does_not_grow_it() {
    assert_eq!(deaths_shadow_after_sign_in_blood(3), (1, 12, 12));
    assert_eq!(deaths_shadow_after_sign_in_blood(1), (-1, 13, 13));
}

/// "the greatest power among creatures you control" of -2 adds +0/+0.
#[test]
fn overwhelming_stampede_negative_greatest_power_adds_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(P0, pool(&[(3, ManaType::Colorless), (2, ManaType::Green)]));
    let neg = s.add_creature(P0, "Negative", -2, 3).id();
    let stampede = s
        .add_spell_to_hand_from_oracle(P0, "Overwhelming Stampede", false, OVERWHELMING_STAMPEDE)
        .id();
    let mut r = s.build();
    let out = r.cast(stampede).resolve();
    assert_eq!(out.power_toughness(neg), (-2, 3));
}

fn skyknight_after_enlisting(befuddle_helper: bool) -> i32 {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Card A"]);
    s.with_mana_pool(P0, pool(&[(2, ManaType::Colorless), (1, ManaType::Blue)]));
    let skyknight = s
        .add_creature_from_oracle(P0, "Coalition Skyknight", 2, 2, COALITION_SKYKNIGHT)
        .id();
    let helper = s.add_creature(P0, "Helper", 3, 3).id();
    let befuddle = s
        .add_spell_to_hand_from_oracle(P0, "Befuddle", true, BEFUDDLE)
        .id();
    let mut r = s.build();
    if befuddle_helper {
        let out = r.cast(befuddle).target_object(helper).resolve();
        assert_eq!(out.power_toughness(helper), (-1, 3));
    }
    r.advance_to_combat();
    r.declare_attackers(&[(skyknight, AttackTarget::Player(P1))])
        .expect("declare attackers");
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::EnlistChoice { attacker, ref eligible, .. }
                if attacker == skyknight && eligible.contains(&helper)
        ),
        "{:?}",
        r.state().waiting_for
    );
    r.act(GameAction::ChooseEnlist {
        target: Some(helper),
    })
    .expect("enlist");
    assert!(r.state().objects[&helper].tapped);
    r.resolve_top();
    assert!(r.state().stack.is_empty());
    r.state().objects[&skyknight].power.unwrap()
}

/// CR 702.154a + CR 107.1b: enlisting a creature with power -1 adds +0/+0;
/// enlisting a 3-power creature still adds +3/+0.
#[test]
fn enlist_negative_power_creature_does_not_lower_the_attacker() {
    assert_eq!(skyknight_after_enlisting(false), 5);
    assert_eq!(skyknight_after_enlisting(true), 2);
}
