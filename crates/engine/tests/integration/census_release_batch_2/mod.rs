//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 2: each shape's
//! Oracle wording runs through the public cast/activate/combat pipeline and behaves as printed,
//! including the element the misparse entry said was missing. For the first 20 rows, lines that
//! are not the cited clause (Cycling, Flash, Equip, transform, ETB tokens, keywords) are omitted
//! from the test cards. The last two rows (Soul-Scar Mage and Szadek, fixed in code by #9670) run at
//! their full printed Oracle text.
use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Pass priority and answer prompts until the stack is empty and nothing is pending.
fn settle(r: &mut GameRunner) {
    for _ in 0..60 {
        match &r.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    return;
                }
                r.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                r.choose_first_legal_target().expect("trigger target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                r.act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(r.state_mut());
            }
            o => panic!("settle: unexpected {o:?}"),
        }
    }
    panic!("settle: did not settle: {:?}", r.state().waiting_for);
}

/// True when the cast driver reports that the declared target is not in the engine's legal set.
fn target_rejected<F: FnOnce()>(f: F) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(()) => false,
        Err(e) => {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            assert!(
                msg.contains("no declared target matches"),
                "unexpected panic: {msg}"
            );
            true
        }
    }
}

fn zone_of(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

fn named(r: &GameRunner, name: &str) -> ObjectId {
    r.state()
        .objects
        .values()
        .find(|o| o.name == name)
        .unwrap()
        .id
}

fn pass_until(r: &mut GameRunner, want: fn(&WaitingFor) -> bool) {
    for _ in 0..40 {
        if want(&r.state().waiting_for) {
            return;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    panic!("did not reach wanted state: {:?}", r.state().waiting_for);
}

/// Attack the opponent with `attacker` and stop at the declare-blockers step.
fn attack_to_blockers(r: &mut GameRunner, attacker: ObjectId) {
    pass_until(r, |w| matches!(w, WaitingFor::DeclareAttackers { .. }));
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("attack");
    pass_until(r, |w| matches!(w, WaitingFor::DeclareBlockers { .. }));
}

// ---- Shape 1: "deals N damage to target opponent or planeswalker" / "target opponent" (Inferno
// Jet, Vampiric Touch, Syndicate Guildmage, Sif's Spearmaster). CR 115.1 + CR 102.2: an opponent
// is a player, so an opposing creature is never a legal target, and neither is the caster. ----
fn opp_or_pw(text: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let their_creature = s.add_creature(P1, "TheirCreature", 2, 2).id();
    let their_pw = s
        .add_planeswalker_from_oracle(P1, "TheirPW", "Jace", 5, "")
        .id();
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Burn", false, text)
        .id();
    (s.build(), spell, their_creature, their_pw)
}

#[test]
fn target_opponent_or_planeswalker_spell() {
    let t = "Inferno Jet deals 6 damage to target opponent or planeswalker.";
    let (mut r, sp, c, _pw) = opp_or_pw(t);
    assert!(
        target_rejected(|| {
            r.cast(sp).target_object(c).resolve();
        }),
        "opposing creature is not a legal target"
    );
    let (mut r, sp, _c, _pw) = opp_or_pw(t);
    assert!(
        target_rejected(|| {
            r.cast(sp).target_player(P0).resolve();
        }),
        "own player is not a legal target (opponent only)"
    );
    let (mut r, sp, c, _pw) = opp_or_pw(t);
    r.cast(sp).target_player(P1).resolve();
    assert_eq!(r.life(P1), 14, "opponent player takes 6");
    assert_eq!(r.state().objects[&c].damage_marked, 0, "creature untouched");
    let (mut r, sp, _c, pw) = opp_or_pw(t);
    r.cast(sp).target_object(pw).resolve();
    assert_eq!(r.life(P1), 20, "planeswalker target: no life loss");
    assert!(
        r.state().objects[&pw].loyalty.unwrap_or(0) < 5,
        "planeswalker lost loyalty"
    );
}

#[test]
fn target_opponent_or_planeswalker_with_life_gain_vampiric_touch() {
    let t = "Vampiric Touch deals 2 damage to target opponent or planeswalker and you gain 2 life.";
    let (mut r, sp, c, _) = opp_or_pw(t);
    assert!(
        target_rejected(|| {
            r.cast(sp).target_object(c).resolve();
        }),
        "opposing creature not legal"
    );
    let (mut r, sp, _, _) = opp_or_pw(t);
    r.cast(sp).target_player(P1).resolve();
    assert_eq!((r.life(P0), r.life(P1)), (22, 18));
}

#[test]
fn target_opponent_activated_ability() {
    // The activation driver declares the creature; it is only ignored when the engine finds a
    // single legal target (the opponent) and selects it itself. If the creature were legal, it
    // would be chosen and take the damage.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let their_creature = s.add_creature(P1, "TheirCreature", 2, 2).id();
    let sif = s
        .add_creature_from_oracle(
            P0,
            "Sif's Spearmaster",
            3,
            3,
            "{T}: This creature deals damage equal to its power to target opponent.",
        )
        .id();
    let mut r = s.build();
    r.activate(sif, 0).target_object(their_creature).resolve();
    assert_eq!(r.life(P1), 17, "power 3 damage went to the opponent player");
    assert_eq!(
        r.state().objects[&their_creature].damage_marked,
        0,
        "the opposing creature was not a legal target"
    );
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let c = s.add_creature(P1, "TheirCreature", 2, 2).id();
    let g = s
        .add_creature_from_oracle(
            P0,
            "Syndicate Guildmage",
            2,
            2,
            "{1}{W}, {T}: Tap target creature with power 4 or greater.\n{4}{B}, {T}: This creature deals 2 damage to target opponent or planeswalker.",
        )
        .id();
    for _ in 0..5 {
        s.add_basic_land(P0, ManaColor::Black);
    }
    let mut r = s.build();
    r.activate(g, 1).target_object(c).resolve();
    assert_eq!(r.life(P1), 18, "damage went to the opponent player");
    assert_eq!(
        r.state().objects[&c].damage_marked,
        0,
        "the opposing creature took nothing"
    );
}

// ---- Shape 2: "Whenever this creature deals damage to an opponent, you may draw a card."
// (Vedalken Heretic). Damage to a blocking creature an opponent controls is not damage to an
// opponent. ----
fn heretic_cards_drawn(blocked: bool) -> usize {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let h = s
        .add_creature_from_oracle(
            P0,
            "Vedalken Heretic",
            2,
            2,
            "Whenever this creature deals damage to an opponent, you may draw a card.",
        )
        .id();
    let blocker = s.add_creature(P1, "Blocker", 1, 6).id();
    s.with_library_top(P0, &["A", "B"]);
    let mut r = s.build();
    let before = r.state().players[0].hand.len();
    attack_to_blockers(&mut r, h);
    if blocked {
        r.declare_blockers(&[(blocker, h)]).expect("block");
    } else {
        r.declare_blockers(&[]).expect("no block");
    }
    for _ in 0..30 {
        if r.state().phase == Phase::EndCombat {
            break;
        }
        match &r.state().waiting_for {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                r.act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(r.state_mut());
            }
            _ => break,
        }
    }
    r.state().players[0].hand.len() - before
}

#[test]
fn damage_to_opponent_trigger_not_fired_by_damage_to_their_creature() {
    assert_eq!(
        heretic_cards_drawn(false),
        1,
        "unblocked: damage to the opponent draws a card"
    );
    assert_eq!(
        heretic_cards_drawn(true),
        0,
        "blocked: damage to an opponent's creature must not trigger"
    );
}

// ---- Shape 3: activated "Prevent all combat damage that would be dealt this turn."
// (Dawnstrider, Knight-Captain of Eos). CR 615.1 (prevention effects): all combat damage, to every
// recipient; a blocked attack checks creature recipients both ways, an unblocked one the player. ----
#[derive(Clone, Copy)]
enum Preventer {
    None,
    Dawnstrider,
    KnightCaptain,
}

/// Returns (opponent life, total damage marked on attacker and blocker) after an attack that the
/// opponent's creature blocks (`blocked`) or that goes unblocked.
fn combat(preventer: Preventer, blocked: bool) -> (i32, u32) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "Attacker", 2, 2).id();
    let b = s.add_creature(P1, "Blocker", 1, 6).id();
    let src = match preventer {
        Preventer::None => None,
        Preventer::Dawnstrider => {
            s.add_basic_land(P0, ManaColor::Green);
            s.add_card_to_hand(P0, "Discard Fodder");
            Some(
                s.add_creature_from_oracle(
                    P0,
                    "Dawnstrider",
                    1,
                    1,
                    "{G}, {T}, Discard a card: Prevent all combat damage that would be dealt this turn.",
                )
                .id(),
            )
        }
        Preventer::KnightCaptain => {
            s.add_basic_land(P0, ManaColor::White);
            s.add_creature(P0, "Soldier Token", 1, 1)
                .with_subtypes(vec!["Soldier"]);
            Some(
                s.add_creature_from_oracle(
                    P0,
                    "Knight-Captain of Eos",
                    2,
                    2,
                    "{W}, Sacrifice a Soldier: Prevent all combat damage that would be dealt this turn.",
                )
                .id(),
            )
        }
    };
    let mut r = s.build();
    if let Some(src) = src {
        let payment = match preventer {
            Preventer::Dawnstrider => named(&r, "Discard Fodder"),
            _ => named(&r, "Soldier Token"),
        };
        let o = r.activate(src, 0).pay_with(&[payment]).resolve();
        assert!(o.state().stack.is_empty(), "prevention resolved");
    }
    attack_to_blockers(&mut r, a);
    if blocked {
        r.declare_blockers(&[(b, a)]).expect("block");
    } else {
        r.declare_blockers(&[]).expect("no block");
    }
    r.combat_damage();
    (
        r.life(P1),
        r.state().objects[&b].damage_marked + r.state().objects[&a].damage_marked,
    )
}

#[test]
fn prevent_all_combat_damage_activated_covers_every_recipient() {
    for (preventer, name) in [
        (Preventer::Dawnstrider, "Dawnstrider"),
        (Preventer::KnightCaptain, "Knight-Captain"),
    ] {
        assert_eq!(
            combat(Preventer::None, true),
            (20, 3),
            "control: blocked, damage dealt both ways"
        );
        assert_eq!(
            combat(Preventer::None, false),
            (18, 0),
            "control: unblocked, the player takes 2"
        );
        assert_eq!(
            combat(preventer, true),
            (20, 0),
            "{name}: blocked, no damage to either creature"
        );
        assert_eq!(
            combat(preventer, false),
            (20, 0),
            "{name}: unblocked, no damage to the player"
        );
    }
}

// ---- Shape 4: "Whenever you cast an artifact or creature spell using mana produced by Barracks
// of the Thousand, ..." (Barracks of the Thousand; transform line omitted). The trigger requires
// mana from this permanent. ----
const BARRACKS: &str = "{T}: Add {W}.\nWhenever you cast an artifact or creature spell using mana produced by Barracks of the Thousand, create a white Gnome Soldier artifact creature token with \"This token's power and toughness are each equal to the number of artifacts and/or creatures you control.\"";

/// (colors, is an artifact creature, power, toughness) of each Gnome token on the battlefield.
type TokenFacts = (Vec<ManaColor>, bool, Option<i32>, Option<i32>);

fn barracks_tokens(use_barracks: bool) -> Vec<TokenFacts> {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let b = s
        .add_land_from_oracle(P0, "Barracks of the Thousand", BARRACKS)
        .id();
    s.add_basic_land(P0, ManaColor::White);
    let c = s
        .add_creature_to_hand(P0, "Cast Creature", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let mut r = s.build();
    if use_barracks {
        r.activate(b, 0).resolve();
    } else {
        r.state_mut().objects.get_mut(&b).unwrap().tapped = true;
    }
    r.cast(c).resolve();
    settle(&mut r);
    assert_eq!(zone_of(&r, c), Zone::Battlefield, "creature spell resolved");
    r.state()
        .objects
        .values()
        .filter(|o| o.name.contains("Gnome") && o.zone == Zone::Battlefield)
        .map(|o| {
            let types = &o.card_types.core_types;
            (
                o.color.clone(),
                types.contains(&CoreType::Artifact) && types.contains(&CoreType::Creature),
                o.power,
                o.toughness,
            )
        })
        .collect()
}

#[test]
fn cast_using_mana_produced_by_this_permanent() {
    assert_eq!(
        barracks_tokens(true),
        vec![(vec![ManaColor::White], true, Some(2), Some(2))],
        "paid with Barracks mana: one white artifact creature token, 2/2 (the cast creature and itself)"
    );
    assert!(
        barracks_tokens(false).is_empty(),
        "paid with a Plains: no token"
    );
}

// ---- Shape 5: "When this Equipment enters, attach it to target creature you control. That
// creature gains first strike until end of turn." (Squire's Lightblade, Hidden Footblade). "It"
// is this Equipment. ----
#[test]
fn equipment_etb_attaches_itself() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    // "creature you control": an opponent's creature is not a legal target. It is added first so
    // it would be the first legal target (what `settle` picks) if the qualifier were dropped.
    let theirs = s.add_creature(P1, "TheirCreature", 2, 2).id();
    let host = s.add_creature(P0, "Host", 2, 2).id();
    let eq = s
        .add_artifact_to_hand_from_oracle(
            P0,
            "Blade",
            "When this Equipment enters, attach it to target creature you control. That creature gains first strike until end of turn.",
        )
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut r = s.build();
    r.cast(eq).resolve();
    settle(&mut r);
    assert_eq!(zone_of(&r, eq), Zone::Battlefield, "equipment entered");
    assert_eq!(
        r.state().objects[&eq].attached_to,
        Some(AttachTarget::Object(host)),
        "equipment attached to the host creature"
    );
    assert!(
        r.state().objects[&host].attachments.contains(&eq),
        "host lists the equipment"
    );
    assert!(
        r.state().objects[&host].has_keyword(&Keyword::FirstStrike),
        "host gained first strike"
    );
    assert!(
        r.state().objects[&theirs].attachments.is_empty(),
        "an opponent's creature is not attached to"
    );
}

// ---- Shape 6: "that creature's power" where the entering creature is the referent (Arni,
// Renowned Champion; Hamletback Goliath; Eldrazi Mimic; Yorvo, Lord of Garenbrig). The amount
// reads the entering creature, not the host. ----
fn enter_creature(
    host_name: &str,
    host_pt: (i32, i32),
    host_text: &str,
    enterer_pt: (i32, i32),
    green: bool,
    host_counters: u32,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mut h = s.add_creature_from_oracle(P0, host_name, host_pt.0, host_pt.1, host_text);
    if host_counters > 0 {
        h.with_plus_counters(host_counters);
    }
    let host = h.id();
    let mut e = s.add_creature_to_hand(P0, "Enterer", enterer_pt.0, enterer_pt.1);
    if green {
        e.with_color(vec![ManaColor::Green]);
    }
    let enterer = e.id();
    (s.build(), host, enterer)
}

fn enter_and_settle(r: &mut GameRunner, enterer: ObjectId) {
    r.cast(enterer).accept_optional().resolve();
    settle(r);
}

#[test]
fn entering_creature_power_arni_pump() {
    let (mut r, arni, e) = enter_creature(
        "Arni, Renowned Champion",
        (2, 2),
        "Whenever another creature you control enters, Arni gets +X/+0 until end of turn, where X is that creature's power.",
        (4, 4),
        false,
        0,
    );
    enter_and_settle(&mut r, e);
    assert_eq!(
        r.state().objects[&arni].power,
        Some(6),
        "2 + entering creature's power 4"
    );
}

#[test]
fn entering_creature_power_arni_ignores_an_opponents_creature() {
    // "another creature you control": Arni is P1's, the enterer is P0's, so nothing triggers.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let arni = s
        .add_creature_from_oracle(
            P1,
            "Arni, Renowned Champion",
            2,
            2,
            "Whenever another creature you control enters, Arni gets +X/+0 until end of turn, where X is that creature's power.",
        )
        .id();
    let e = s.add_creature_to_hand(P0, "Enterer", 4, 4).id();
    let mut r = s.build();
    enter_and_settle(&mut r, e);
    assert_eq!(
        r.state().objects[&arni].power,
        Some(2),
        "opponent's creature entering does not pump Arni"
    );
}

#[test]
fn entering_creature_power_hamletback_counters() {
    let (mut r, h, e) = enter_creature(
        "Hamletback Goliath",
        (6, 6),
        "Whenever another creature enters, you may put X +1/+1 counters on this creature, where X is that creature's power.",
        (3, 3),
        false,
        0,
    );
    enter_and_settle(&mut r, e);
    let counters: u32 = r.state().objects[&h].counters.values().sum();
    assert_eq!(counters, 3, "entering creature's power, not the host's 6");
}

#[test]
fn entering_creature_pt_eldrazi_mimic() {
    let (mut r, m, e) = enter_creature(
        "Eldrazi Mimic",
        (2, 1),
        "Whenever another colorless creature you control enters, you may have this creature's base power and toughness become equal to that creature's power and toughness until end of turn.",
        (5, 4),
        false,
        0,
    );
    enter_and_settle(&mut r, e);
    let o = &r.state().objects[&m];
    assert_eq!(
        (o.power, o.toughness),
        (Some(5), Some(4)),
        "Mimic became 5/4"
    );
}

#[test]
fn entering_creature_pt_eldrazi_mimic_ignores_a_colored_creature() {
    // "another colorless creature": a green enterer does not trigger it.
    let (mut r, m, e) = enter_creature(
        "Eldrazi Mimic",
        (2, 1),
        "Whenever another colorless creature you control enters, you may have this creature's base power and toughness become equal to that creature's power and toughness until end of turn.",
        (5, 4),
        true,
        0,
    );
    enter_and_settle(&mut r, e);
    let o = &r.state().objects[&m];
    assert_eq!(
        (o.power, o.toughness),
        (Some(2), Some(1)),
        "colored enterer: no change"
    );
}

#[test]
fn entering_creature_power_yorvo_compare() {
    let text = "Whenever another green creature you control enters, put a +1/+1 counter on Yorvo. Then if that creature's power is greater than Yorvo's power, put another +1/+1 counter on Yorvo.";
    // Yorvo 0/0 with four counters = 4/4 (its "enters with" line is not under test).
    let cases = [
        ((7, 7), true, 6, "entering power 7 > 5: another counter"),
        ((3, 3), true, 5, "entering power 3 not greater: one counter"),
        (
            (5, 5),
            true,
            5,
            "entering power 5 equals Yorvo's 5 after the first counter: not greater (checked after it, not before)",
        ),
        ((7, 7), false, 4, "non-green enterer does not trigger"),
    ];
    for (pt, green, want, why) in cases {
        let (mut r, y, e) = enter_creature("Yorvo, Lord of Garenbrig", (0, 0), text, pt, green, 4);
        enter_and_settle(&mut r, e);
        assert_eq!(r.state().objects[&y].power, Some(want), "{why}");
    }
}

// ---- Shape 7: "If a permanent entering causes a triggered ability of a permanent you control
// to trigger, that ability triggers an additional time." (Virtue of Knowledge, Yarok, Starfield
// Vocalist, Elesh Norn, Mother of Machines). ----
const DOUBLER: &str = "If a permanent entering causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";

fn life_gained_on_enter(
    doubler_text: &str,
    doubler_player: PlayerId,
    watcher_player: PlayerId,
) -> i32 {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(doubler_player, "Doubler", 1, 1, doubler_text);
    s.add_creature_from_oracle(
        watcher_player,
        "Watcher",
        1,
        1,
        "Whenever another creature enters, you gain 1 life.",
    );
    let e = s.add_creature_to_hand(P0, "Enterer", 1, 1).id();
    let mut r = s.build();
    let before = r.life(watcher_player);
    enter_and_settle(&mut r, e);
    r.life(watcher_player) - before
}

/// Starfield Vocalist's printed wording of the same line ("entering the battlefield").
const DOUBLER_BATTLEFIELD: &str = "If a permanent entering the battlefield causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";

#[test]
fn double_entry_triggers() {
    assert_eq!(
        life_gained_on_enter(DOUBLER_BATTLEFIELD, P0, P0),
        2,
        "Starfield Vocalist's wording: own watcher triggers twice"
    );
    assert_eq!(
        life_gained_on_enter(DOUBLER_BATTLEFIELD, P1, P0),
        1,
        "Starfield Vocalist's wording: opponent's doubler does not double my triggers"
    );
    assert_eq!(
        life_gained_on_enter(DOUBLER, P0, P0),
        2,
        "own watcher triggers twice"
    );
    assert_eq!(
        life_gained_on_enter(DOUBLER, P1, P0),
        1,
        "opponent's doubler does not double my triggers"
    );
    assert_eq!(
        life_gained_on_enter(DOUBLER, P0, P1),
        1,
        "my doubler does not double the opponent's watcher"
    );
}

#[test]
fn double_entry_triggers_not_leaves_battlefield() {
    // Only entering causes are doubled: a dies trigger fires once.
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Doubler", 1, 1, DOUBLER);
    let d = s
        .add_creature_from_oracle(
            P0,
            "Dier",
            1,
            1,
            "When this creature dies, you gain 1 life.",
        )
        .id();
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Smite",
            true,
            "Smite deals 5 damage to target creature.",
        )
        .id();
    let mut r = s.build();
    r.cast(spell).target_object(d).resolve();
    settle(&mut r);
    assert_eq!(zone_of(&r, d), Zone::Graveyard, "the creature died");
    assert_eq!(r.life(P0), 21, "dies trigger fires once");
}

#[test]
fn elesh_norn_opponents_entry_triggers_suppressed() {
    // Elesh Norn, Mother of Machines (Vigilance omitted): both lines.
    let t = format!(
        "{DOUBLER}\nPermanents entering don't cause abilities of permanents your opponents control to trigger."
    );
    assert_eq!(life_gained_on_enter(&t, P0, P0), 2, "line one doubles mine");
    assert_eq!(
        life_gained_on_enter(&t, P0, P1),
        0,
        "line two: opponent's watcher does not trigger"
    );
}

// ---- Shape 8: triggered abilities that add mana (Su-Chi Cave Guard, The Vision and Scarlet
// Witch). They trigger on a death and on a cast, not from a mana ability or mana being added, so
// CR 605.1b does not make them mana abilities: they use the stack. ----
#[test]
fn su_chi_death_trigger_uses_the_stack() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let su = s
        .add_creature_from_oracle(
            P1,
            "Su-Chi Cave Guard",
            2,
            2,
            "When this creature dies, add eight {C}. Until end of turn, you don't lose this mana as steps and phases end.",
        )
        .id();
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Smite",
            true,
            "Smite deals 5 damage to target creature.",
        )
        .id();
    let mut r = s.build();
    drop(r.cast(spell).target_object(su).commit());
    // Pass until Smite has resolved and Su-Chi has died; its trigger must then wait on the stack.
    for _ in 0..6 {
        if zone_of(&r, su) == Zone::Graveyard && !r.state().stack.is_empty() {
            break;
        }
        if matches!(r.state().waiting_for, WaitingFor::Priority { .. }) {
            r.act(GameAction::PassPriority).expect("pass");
        } else {
            break;
        }
    }
    assert_eq!(zone_of(&r, su), Zone::Graveyard, "Su-Chi died");
    assert!(!r.state().stack.is_empty(), "death trigger is on the stack");
    assert_eq!(
        r.state().players[1].mana_pool.total(),
        0,
        "no mana before the trigger resolves"
    );
    settle(&mut r);
    assert_eq!(
        r.state().players[1].mana_pool.total(),
        8,
        "eight colorless after resolution"
    );
    // Second sentence: "Until end of turn, you don't lose this mana as steps and phases end."
    // Move past the precombat main phase; the mana must survive the phase change.
    for _ in 0..8 {
        if r.state().phase != Phase::PreCombatMain {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    assert_ne!(
        r.state().phase,
        Phase::PreCombatMain,
        "the phase changed after passing priority"
    );
    assert_eq!(
        r.state().players[1].mana_pool.total(),
        8,
        "mana is kept as the phase ends"
    );
}

#[test]
fn vision_and_scarlet_witch_cast_trigger_uses_the_stack() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let v = s
        .add_creature_from_oracle(
            P0,
            "The Vision and Scarlet Witch",
            3,
            3,
            "Whenever you cast a spell, add {R} and put a +1/+1 counter on The Vision and Scarlet Witch.",
        )
        .id();
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Gain Me", true, "You gain 1 life.")
        .id();
    let mut r = s.build();
    let c = r.cast(spell).commit();
    let stack_len = c.state().stack.len();
    assert!(
        stack_len >= 2,
        "spell and the cast trigger are both on the stack, got {stack_len}"
    );
    assert_eq!(
        c.state().players[0].mana_pool.total(),
        0,
        "no mana before the trigger resolves"
    );
    drop(c);
    settle(&mut r);
    let counters: u32 = r.state().objects[&v].counters.values().sum();
    assert_eq!(counters, 1, "counter added");
    assert_eq!(r.state().players[0].mana_pool.total(), 1, "{{R}} added");
}

// ---- Rows released by upstream's damage-to-counters substitution (#9670): Soul-Scar Mage and
// Szadek, Lord of Secrets. Each card runs at its full printed Oracle text (every line, keywords
// included). CR 614.1a + CR 614.6: the damage is replaced, so it is never dealt;
// CR 615 prevention would leave a `DamagePrevented` event and would not substitute the counters. ----
const SOUL_SCAR_MAGE: &str = "Prowess\nIf a source you control would deal noncombat damage to a creature an opponent controls, put that many -1/-1 counters on that creature instead.";
const SZADEK: &str = "Flying\nIf Szadek would deal combat damage to a player, instead put that many +1/+1 counters on Szadek and that player mills that many cards.";

fn red_mana() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])]
}

fn give_priority(r: &mut GameRunner, player: PlayerId) {
    let state = r.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn counters_on(r: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    r.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}

fn damage_dealt_to(events: &[GameEvent], target: TargetRef) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::DamageDealt { target: t, .. } if *t == target))
}

fn damage_prevented(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::DamagePrevented { .. }))
}

/// Soul-Scar Mage's controller (P0) or P1 casts a 3-damage burn spell at `target`.
enum Burn {
    P0AtOpponentCreature,
    P0AtOpponentPlayer,
    P0AtOwnCreature,
    P1AtOwnCreature,
}

/// Returns (mage, theirs, ours, outcome) after the burn resolves.
fn soul_scar_burn(case: Burn) -> (ObjectId, ObjectId, ObjectId, Outcome) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mage = s
        .add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_MAGE)
        .id();
    let theirs = s.add_creature(P1, "Hill Ogre", 4, 4).id();
    let ours = s.add_creature(P0, "Own Golem", 4, 4).id();
    let caster = match case {
        Burn::P1AtOwnCreature => P1,
        _ => P0,
    };
    let bolt = s.add_bolt_to_hand(caster);
    s.with_mana_pool(caster, red_mana());
    let mut r = s.build();
    give_priority(&mut r, caster);
    let cast = r.cast(bolt);
    let outcome = match case {
        Burn::P0AtOpponentCreature | Burn::P1AtOwnCreature => cast.target_object(theirs).resolve(),
        Burn::P0AtOpponentPlayer => cast.target_player(P1).resolve(),
        Burn::P0AtOwnCreature => cast.target_object(ours).resolve(),
    };
    (mage, theirs, ours, outcome)
}

/// Prowess line present and working, and the replacement line: burn at an opponent's creature puts
/// that many -1/-1 counters on it and deals no damage.
#[test]
fn soul_scar_mage_burn_at_opponents_creature_becomes_minus_counters() {
    let (mage, ogre, _ours, outcome) = soul_scar_burn(Burn::P0AtOpponentCreature);
    assert_eq!(
        outcome.damage_marked(ogre),
        0,
        "replaced damage is never dealt"
    );
    outcome.assert_counters(ogre, CounterType::Minus1Minus1, 3);
    assert_eq!(
        outcome.power_toughness(ogre),
        (1, 1),
        "4/4 with three -1/-1 counters"
    );
    assert!(
        !damage_dealt_to(outcome.events(), TargetRef::Object(ogre)),
        "no damage event to the creature"
    );
    assert!(
        !damage_prevented(outcome.events()),
        "a replacement is not a prevention"
    );
    // The Prowess line (full printed text): casting the noncreature burn spell gave +1/+1.
    assert!(outcome.state().objects[&mage]
        .keywords
        .contains(&Keyword::Prowess));
    assert_eq!(
        outcome.power_toughness(mage),
        (2, 3),
        "prowess trigger resolved"
    );
}

/// CR 614.1a ("instead" is a replacement effect): the recipient must be a creature an opponent controls
/// and the source must be one you control. Each leg of the scope has a hostile case that is dealt normally.
#[test]
fn soul_scar_mage_scope_legs_outside_it_deal_normal_damage() {
    let (_m, _t, _o, outcome) = soul_scar_burn(Burn::P0AtOpponentPlayer);
    assert_eq!(outcome.life_delta(P1), -3, "a player is not a creature");
    let (_m, _t, ours, outcome) = soul_scar_burn(Burn::P0AtOwnCreature);
    assert_eq!(
        outcome.damage_marked(ours),
        3,
        "own creature is not 'an opponent controls'"
    );
    outcome.assert_counters(ours, CounterType::Minus1Minus1, 0);
    let (_m, theirs, _o, outcome) = soul_scar_burn(Burn::P1AtOwnCreature);
    assert_eq!(
        outcome.damage_marked(theirs),
        3,
        "the source is not one you control"
    );
    outcome.assert_counters(theirs, CounterType::Minus1Minus1, 0);
}

/// CR 120.2a-b + CR 510.2: combat damage is not noncombat damage.
#[test]
fn soul_scar_mage_combat_damage_is_dealt_normally() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Soul-Scar Mage", 1, 2, SOUL_SCAR_MAGE);
    let attacker = s.add_creature(P0, "Attacker", 3, 3).id();
    let blocker = s.add_creature(P1, "Blocker", 1, 5).id();
    let mut r = s.build();
    attack_to_blockers(&mut r, attacker);
    r.declare_blockers(&[(blocker, attacker)]).expect("block");
    let outcome = r.combat_damage();
    assert_eq!(outcome.damage_marked(blocker), 3);
    outcome.assert_counters(blocker, CounterType::Minus1Minus1, 0);
}

/// Declare `attackers` against P1 and pass priority until the declare-blockers prompt (returns true)
/// or the combat-damage step (returns false: the engine skipped the prompt because no creature of
/// P1's could legally block).
fn attack_and_offer_blocks(r: &mut GameRunner, attackers: &[ObjectId]) -> bool {
    pass_until(r, |w| matches!(w, WaitingFor::DeclareAttackers { .. }));
    r.act(GameAction::DeclareAttackers {
        attacks: attackers
            .iter()
            .map(|a| (*a, AttackTarget::Player(P1)))
            .collect(),
        bands: vec![],
    })
    .expect("attack");
    for _ in 0..12 {
        if matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. }) {
            return true;
        }
        if r.state().phase == Phase::CombatDamage {
            return false;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    panic!(
        "combat did not reach blockers or damage: {:?}",
        r.state().waiting_for
    );
}

fn library_len(r: &GameRunner, p: PlayerId) -> usize {
    r.state()
        .players
        .iter()
        .find(|x| x.id == p)
        .unwrap()
        .library
        .len()
}

/// Szadek (5/5 Flying) attacking P1 with a ten-card library. `blocker`: a creature P1 controls.
fn szadek_board(
    blocker: Option<(&str, i32, i32, bool)>,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let szadek = s
        .add_creature_from_oracle(P0, "Szadek, Lord of Secrets", 5, 5, SZADEK)
        .id();
    let blocker = blocker.map(|(name, p, t, flying)| {
        let mut b = s.add_creature(P1, name, p, t);
        if flying {
            b.with_keyword(Keyword::Flying);
        }
        b.id()
    });
    s.with_library_top(
        P1,
        &["C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8", "C9", "C10"],
    );
    (s.build(), szadek, blocker)
}

/// Both lines of the printed text. Unblocked, Szadek's 5 combat damage to the player becomes five
/// +1/+1 counters on Szadek and a mill of five; no life is lost. A ground creature cannot block it
/// (Flying line), so the engine offers no block at all.
#[test]
fn szadek_unblocked_combat_damage_becomes_counters_and_mill() {
    let (mut r, szadek, wall) = szadek_board(Some(("Ground Wall", 0, 7, false)));
    assert!(r.state().objects[&szadek]
        .keywords
        .contains(&Keyword::Flying));
    let wall = wall.unwrap();
    assert!(
        !attack_and_offer_blocks(&mut r, &[szadek]),
        "a ground creature cannot block a flyer"
    );
    let outcome = r.combat_damage();
    assert_eq!(r.life(P1), 20, "replaced damage is never dealt");
    assert_eq!(counters_on(&r, szadek, &CounterType::Plus1Plus1), 5);
    assert_eq!(outcome.power_toughness(szadek), (10, 10));
    assert_eq!(library_len(&r, P1), 5, "ten minus five milled");
    outcome.assert_zone_count(P1, Zone::Graveyard, 5);
    assert_eq!(outcome.damage_marked(wall), 0);
    assert!(!damage_dealt_to(outcome.events(), TargetRef::Player(P1)));
    assert!(
        !damage_prevented(outcome.events()),
        "a replacement is not a prevention"
    );
}

/// CR 614.1a ("instead" is a replacement effect): the recipient scope is "a player": combat damage to a blocking creature (here one with
/// Flying, so the block is legal) is dealt normally and nothing is substituted.
#[test]
fn szadek_blocked_combat_damage_to_a_creature_is_dealt_normally() {
    let (mut r, szadek, blocker) = szadek_board(Some(("Flying Wall", 0, 7, true)));
    let blocker = blocker.unwrap();
    assert!(
        attack_and_offer_blocks(&mut r, &[szadek]),
        "a flyer may block a flyer"
    );
    r.declare_blockers(&[(blocker, szadek)]).expect("block");
    let outcome = r.combat_damage();
    assert_eq!(outcome.damage_marked(blocker), 5);
    assert_eq!(counters_on(&r, szadek, &CounterType::Plus1Plus1), 0);
    assert_eq!(r.life(P1), 20);
    assert_eq!(library_len(&r, P1), 10, "nothing milled");
}

/// CR 614.1a ("instead" is a replacement effect): the source scope is "Szadek": another creature's combat damage is dealt as usual.
#[test]
fn szadek_other_creatures_damage_is_dealt_normally() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Szadek, Lord of Secrets", 5, 5, SZADEK);
    let bear = s.add_creature(P0, "Bear", 2, 2).id();
    s.with_library_top(P1, &["C1", "C2", "C3", "C4"]);
    let mut r = s.build();
    attack_and_offer_blocks(&mut r, &[bear]);
    let outcome = r.combat_damage();
    assert_eq!(r.life(P1), 18, "the bear's 2 is dealt");
    assert_eq!(library_len(&r, P1), 4);
    outcome.assert_zone_count(P1, Zone::Graveyard, 0);
}
