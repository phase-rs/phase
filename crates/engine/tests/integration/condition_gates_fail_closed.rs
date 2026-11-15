//! Condition gates fail closed: a printed gate the parser can type is carried on
//! the definition and enforced at runtime, instead of being dropped so the
//! ungated effect ships. A gated shield that prints a part the parser cannot
//! carry fails closed as a whole.
//!
//! Cards (Oracle text verbatim from card-data / Scryfall):
//!   * Bronze Horse — "As long as you control another creature, prevent all
//!     damage that would be dealt to this creature by spells that target it."
//!   * Woolly Razorback — "As long as this creature has an ice counter on it,
//!     prevent all combat damage it would deal and it has defender."
//!   * Champion Lancer — "Prevent all damage that would be dealt to this
//!     creature by creatures."
//!   * Argothian Treefolk — "Prevent all damage that would be dealt to this
//!     creature by artifact sources."
//!   * Enchanted Being — "Prevent all combat damage that would be dealt to this
//!     creature by enchanted creatures."
//!   * Ascendant Packleader — "This creature enters with a +1/+1 counter on it if
//!     you control a permanent with mana value 4 or greater."
//!   * Hagra Mauling — "This spell costs {1} less to cast if an opponent controls
//!     no basic lands."
//!
//! Runtime tests drive the production pipeline (cast, combat, castability); the
//! two lines that fail closed are pinned at `parse_oracle_text`, where the gap is
//! recorded.

use engine::game::casting::can_cast_object_now;
use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const BRONZE_HORSE: &str = "Trample\nAs long as you control another creature, prevent all damage that would be dealt to this creature by spells that target it.";

const WOOLLY_RAZORBACK: &str = "This creature enters with three ice counters on it.\nAs long as this creature has an ice counter on it, prevent all combat damage it would deal and it has defender.\nWhenever this creature blocks, remove an ice counter from it.";

const CHAMPION_LANCER: &str =
    "Prevent all damage that would be dealt to this creature by creatures.";

const ARGOTHIAN_TREEFOLK: &str =
    "Prevent all damage that would be dealt to this creature by artifact sources.";

const ENCHANTED_BEING: &str =
    "Prevent all combat damage that would be dealt to this creature by enchanted creatures.";

const FIREBREATHING: &str =
    "Enchant creature\n{R}: Enchanted creature gets +1/+0 until end of turn.";

const ASCENDANT_PACKLEADER: &str = "This creature enters with a +1/+1 counter on it if you control a permanent with mana value 4 or greater.\nWhenever you cast a spell with mana value 4 or greater, put a +1/+1 counter on this creature.";

const HAGRA_MAULING: &str =
    "This spell costs {1} less to cast if an opponent controls no basic lands.\nDestroy target creature.";

fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

// ── Gated shields that fail closed (CR 615.1a) ───────────────────────────────

/// Parse `oracle` as a creature and return the gap names of its `Unimplemented`
/// abilities, plus whether any `DamageDone` replacement was built.
fn parsed_gaps_and_damage_shield(
    oracle: &str,
    name: &str,
    keywords: &[&str],
) -> (Vec<String>, bool) {
    let keywords: Vec<String> = keywords.iter().map(|k| k.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &keywords, &["Creature".to_string()], &[]);
    let gaps = parsed
        .abilities
        .iter()
        .filter_map(|a| match &*a.effect {
            Effect::Unimplemented { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    let shield = parsed
        .replacements
        .iter()
        .any(|r| r.event == ReplacementEvent::DamageDone);
    (gaps, shield)
}

/// CR 615.1a: Bronze Horse's shield is scoped to "spells that target it", a
/// source subject no filter expresses ("that target" is not in the target
/// grammar). Its gate parses, so the line fails closed on the source scope: no
/// shield (which would prevent combat damage and every untargeted source while
/// the gate holds), one line-owned gap.
#[test]
fn bronze_horse_source_scope_fails_closed() {
    let (gaps, shield) = parsed_gaps_and_damage_shield(BRONZE_HORSE, "Bronze Horse", &["Trample"]);
    assert_eq!(gaps, vec!["prevention_source_scope".to_string()]);
    assert!(!shield, "no unscoped damage-prevention shield may be built");
}

/// CR 615.1a + CR 702.3a: Woolly Razorback's gated line also grants defender
/// ("and it has defender"); the shield alone would drop it, so the line fails
/// closed. Its other two lines still parse (the ice counters and the block
/// trigger carry no gap).
#[test]
fn woolly_razorback_coordinated_conjunct_fails_closed() {
    let (gaps, shield) = parsed_gaps_and_damage_shield(WOOLLY_RAZORBACK, "Woolly Razorback", &[]);
    assert_eq!(gaps, vec!["prevention_conjunct".to_string()]);
    assert!(!shield, "no shield may be built without the defender grant");
}

// ── "dealt to <recipient> by <subject>" source scope (CR 615.1a) ─────────────

fn advance_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..12 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            return;
        }
        runner.pass_both_players();
    }
    panic!(
        "never reached declare blockers; stuck at {:?}",
        runner.state().waiting_for
    );
}

/// Bolt the shielded creature; returns the damage marked on it.
fn bolt_shielded(name: &str, oracle: &str, power: i32, toughness: i32) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shielded = scenario
        .add_creature(P0, name, power, toughness)
        .from_oracle_text(oracle)
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    runner
        .cast(bolt)
        .target_object(shielded)
        .resolve()
        .damage_marked(shielded)
}

/// What attacks the shielded blocker.
enum Attacker {
    Creature,
    ArtifactCreature,
    /// A creature with an Aura (Firebreathing) attached.
    EnchantedCreature,
}

/// The shielded creature blocks an attacking 2/2; returns the damage marked on
/// the blocker after combat damage.
fn shielded_blocks(
    name: &str,
    oracle: &str,
    power: i32,
    toughness: i32,
    attacker: Attacker,
) -> u32 {
    let (outcome, blocker) = shielded_block_outcome(name, oracle, power, toughness, attacker);
    outcome.damage_marked(blocker)
}

/// The shielded creature blocks an attacking 2/2; returns the combat outcome
/// and the blocker.
fn shielded_block_outcome(
    name: &str,
    oracle: &str,
    power: i32,
    toughness: i32,
    attacker: Attacker,
) -> (Outcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A"]);
    scenario.with_library_top(P1, &["Filler B"]);
    let mut bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    if matches!(attacker, Attacker::ArtifactCreature) {
        bear.as_artifact_creature();
    }
    let bear = bear.id();
    let aura = matches!(attacker, Attacker::EnchantedCreature).then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Firebreathing", FIREBREATHING)
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(&["Enchant"], FIREBREATHING)
            .id()
    });
    let blocker = scenario
        .add_creature(P1, name, power, toughness)
        .from_oracle_text(oracle)
        .id();
    let mut runner = scenario.build();
    if let Some(aura) = aura {
        attach_to(runner.state_mut(), aura, bear);
        assert_eq!(
            runner.state().objects[&bear].attachments,
            vec![aura],
            "reach-guard: the Aura must be attached to the attacker"
        );
    }

    runner.pass_both_players();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("Grizzly Bears may attack");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, bear)])
        .expect("the shielded creature may block");
    (runner.combat_damage(), blocker)
}

/// CR 615.1a: Champion Lancer's shield prevents only damage from creatures. A
/// Bolt is an instant, so its damage is dealt (red at the candidate: the source
/// scope was dropped and the shield prevented every source).
#[test]
fn champion_lancer_prevents_creature_damage_only() {
    assert_eq!(
        bolt_shielded("Champion Lancer", CHAMPION_LANCER, 3, 3),
        3,
        "an instant is not a creature — its damage is not prevented"
    );
    // Positive control: a creature's combat damage is prevented.
    assert_eq!(
        shielded_blocks("Champion Lancer", CHAMPION_LANCER, 3, 3, Attacker::Creature),
        0
    );
}

/// CR 615.1a: Argothian Treefolk's shield prevents only damage from artifact
/// sources (red at the candidate: the unscoped shield prevented the Bolt).
#[test]
fn argothian_treefolk_prevents_artifact_source_damage_only() {
    assert_eq!(
        bolt_shielded("Argothian Treefolk", ARGOTHIAN_TREEFOLK, 3, 5),
        3,
        "a red instant is not an artifact source"
    );
    // Positive control: an artifact creature's combat damage is prevented.
    assert_eq!(
        shielded_blocks(
            "Argothian Treefolk",
            ARGOTHIAN_TREEFOLK,
            3,
            5,
            Attacker::ArtifactCreature
        ),
        0
    );
    // Hostile: a non-artifact creature is not an artifact source.
    assert_eq!(
        shielded_blocks(
            "Argothian Treefolk",
            ARGOTHIAN_TREEFOLK,
            3,
            5,
            Attacker::Creature
        ),
        2
    );
}

/// CR 615.1a + CR 303.4b: Enchanted Being's shield prevents combat damage from
/// creatures with an Aura attached. The 2/2 blocks an attacking 2/2: with an
/// Aura on the attacker its damage is prevented; without one the Being takes 2
/// and dies (CR 704.5g).
#[test]
fn enchanted_being_prevents_only_enchanted_creatures_damage() {
    let (outcome, being) = shielded_block_outcome(
        "Enchanted Being",
        ENCHANTED_BEING,
        2,
        2,
        Attacker::EnchantedCreature,
    );
    assert_eq!(outcome.damage_marked(being), 0);
    assert_eq!(outcome.zone_of(being), Zone::Battlefield);
    // Hostile: an attacker with no Aura is not an enchanted creature.
    let (outcome, being) =
        shielded_block_outcome("Enchanted Being", ENCHANTED_BEING, 2, 2, Attacker::Creature);
    assert_eq!(outcome.zone_of(being), Zone::Graveyard);
}

// ── Ascendant Packleader (CR 614.1c + CR 614.1d) ─────────────────────────────

/// Which permanent the Packleader board carries besides the Packleader.
enum PackleaderBoard {
    Empty,
    OwnManaValueFour,
    OpponentManaValueFour,
    OwnManaValueThree,
}

/// Cast Ascendant Packleader; returns its +1/+1 counters once it resolves.
fn cast_packleader(board: PackleaderBoard) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mana_value = |generic: u32| ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic,
    };
    match board {
        PackleaderBoard::Empty => {}
        PackleaderBoard::OwnManaValueFour => {
            scenario
                .add_creature(P0, "Four Drop", 4, 4)
                .with_mana_cost(mana_value(3));
        }
        PackleaderBoard::OpponentManaValueFour => {
            scenario
                .add_creature(P1, "Four Drop", 4, 4)
                .with_mana_cost(mana_value(3));
        }
        PackleaderBoard::OwnManaValueThree => {
            scenario
                .add_creature(P0, "Three Drop", 3, 3)
                .with_mana_cost(mana_value(2));
        }
    }
    let packleader = scenario
        .add_creature_to_hand_from_oracle(P0, "Ascendant Packleader", 2, 1, ASCENDANT_PACKLEADER)
        .with_mana_cost(mana_value(0))
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Green]));
    let mut runner = scenario.build();
    let outcome = runner.cast(packleader).resolve();
    assert_eq!(outcome.zone_of(packleader), Zone::Battlefield);
    outcome.counters(packleader, CounterType::Plus1Plus1)
}

/// CR 614.1c + CR 614.1d: the enters-with counter is gated on controlling a
/// permanent with mana value 4 or greater (red at base: the ungated replacement
/// always placed it).
#[test]
fn ascendant_packleader_enters_without_counter_when_no_mv4_permanent() {
    assert_eq!(cast_packleader(PackleaderBoard::Empty), 0);
    // Positive control: you control a mana value 4 permanent.
    assert_eq!(cast_packleader(PackleaderBoard::OwnManaValueFour), 1);
    // Hostile: the mana value 4 permanent is an opponent's.
    assert_eq!(cast_packleader(PackleaderBoard::OpponentManaValueFour), 0);
    // Hostile: your permanent's mana value is only 3.
    assert_eq!(cast_packleader(PackleaderBoard::OwnManaValueThree), 0);
}

// ── Hagra Mauling (CR 601.2f + CR 109.4) ─────────────────────────────────────

/// Which land, if any, the Hagra board carries.
enum HagraLand {
    None,
    OpponentBasic,
    OwnBasic,
    OpponentNonbasic,
}

/// Can P0 cast Hagra Mauling ({2}{B}{B}) with exactly B, B, C in pool?
fn hagra_castable_with_three_mana(land: HagraLand) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hagra = scenario
        .add_spell_to_hand_from_oracle(P0, "Hagra Mauling", true, HAGRA_MAULING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 2,
        })
        .id();
    // The spell's legal target — `can_cast_object_now` requires one, so without
    // it every board would read "uncastable" vacuously.
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    match land {
        HagraLand::None => {}
        HagraLand::OpponentBasic => {
            scenario.add_basic_land(P1, ManaColor::Black);
        }
        HagraLand::OwnBasic => {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        HagraLand::OpponentNonbasic => {
            scenario.add_land_from_oracle(P1, "Hagra Broodpit", "");
        }
    }
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Black, ManaType::Black, ManaType::Colorless]),
    );
    let runner = scenario.build();
    can_cast_object_now(runner.state(), P0, hagra)
}

/// CR 601.2f + CR 109.4: the {1} reduction applies only while an opponent
/// controls no basic lands; with an opposing basic Hagra costs full and three
/// mana is not enough (red at base: the ungated reduction made it castable).
#[test]
fn hagra_mauling_costs_full_when_an_opponent_controls_a_basic_land() {
    assert!(
        !hagra_castable_with_three_mana(HagraLand::OpponentBasic),
        "an opposing basic land turns the reduction off"
    );
    // Positive control: no opposing basic → reduced to three mana.
    assert!(hagra_castable_with_three_mana(HagraLand::None));
    // Hostile: the caster's own basic land does not count.
    assert!(hagra_castable_with_three_mana(HagraLand::OwnBasic));
    // Hostile: an opposing nonbasic land is not a basic land.
    assert!(hagra_castable_with_three_mana(HagraLand::OpponentNonbasic));
}
