//! Grub, Storied Matriarch // Grub, Notorious Auntie (ECL) — issue #769.
//!
//! > Grub, Storied Matriarch: Menace. Whenever this creature enters or
//! > transforms into Grub, Storied Matriarch, return up to one target Goblin
//! > card from your graveyard to your hand. At the beginning of your first main
//! > phase, you may pay {R}. If you do, transform Grub.
//! >
//! > Grub, Notorious Auntie: Menace. Whenever Grub attacks, you may blight 1. If
//! > you do, create a tapped and attacking token that's a copy of the blighted
//! > creature, except it has "At the beginning of the end step, sacrifice this
//! > token." At the beginning of your first main phase, you may pay {B}. If you
//! > do, transform Grub.
//!
//! CR 701.68c: "the blighted creature" is the object chosen when blighting, so
//! the copy source comes from the blight in this resolution, not from a target.
//! CR 701.68b: a player unable to put the counters on a creature they control
//! can't choose to blight, so every optional blight is gated on that.
//! CR 508.4: the token is attacking without having been declared as an attacker.
//! CR 707.9a: the copy gains the "except" end-step sacrifice trigger.
//! CR 702.26b + CR 702.26d: a phased-out creature is treated as though it doesn't
//! exist, so it is neither offered for blight nor a reason blight is possible; a
//! phased-out "can't" source forbids nothing.
//! CR 118.12 + CR 118.3 + CR 608.2d: an optional pick from a zone with nothing
//! eligible is not offered, so an "If you do" rider stays closed and an "If you
//! don't" branch runs.
//! CR 707.2 + CR 613.1: a stored face is that face's copiable values.
//! CR 707.8a: a token copy of a double-faced object has both faces, up as its
//! source was, with the copy's exceptions on both (CR 707.9a/b/d).
//! CR 709.5: a copy whose copied values are a Room's keeps each half's text
//! under its door, and the gained end-step sacrifice trigger is outside both
//! doors, so it functions with both doors locked.
//! CR 709.4: a Room card off the battlefield has both halves' characteristics,
//! so a token copy of it has each half's text under its own door.
//! CR 730.2i + CR 730.2a: a turned merged component changes the pile's copiable values.
//! CR 702.26b: day/night waits until a phased-out permanent exists again.
//! CR 111.10i: an Incubator token is double-faced, and a reloaded copy of one
//! keeps its stored face's identity.
//! CR 701.27g: a "transformed permanent" is a double-faced permanent on the battlefield
//! with its back face up — a modal card cast back face up and its copy included, never a
//! front-face-up permanent, a Room cast as its right half or a merged permanent.
//! CR 702.145b + CR 702.145e: daybound and nightbound turn a permanent over by the face
//! that is up, not by whether it last transformed.
//! CR 702.145c + CR 702.145f: a permanent whose face disagrees with day or night transforms at
//! the next checkpoint after its entry is observed — a token copy of a daybound card made at
//! night enters front face up, triggers, then transforms.
//! CR 701.27g: a latched source carries the answer at its observation; the raw face-swap bit
//! stays for reconstruction.
//! CR 603.10: a transform's triggers are checked immediately after it, before the state-based
//! check that may remove its subject. CR 800.4d: one whose controller that check eliminates
//! isn't put on the stack. CR 701.27a: only a double-faced token or card transforms, so a
//! single-faced Room copy given daybound stays as it is.

//! CR 702.145b + CR 702.145e: ordinary instructions cannot transform daybound or nightbound permanents.
//! CR 613.7g + CR 730.2i: transforming merged permanents keep their timestamps.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::derived_views::derive_views;
use engine::game::game_object::{BackFaceData, MergeKind, RoomDoor, RoomUnlockState};
use engine::game::layers::evaluate_layers;
use engine::game::merge::MergeSide;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::transform::is_double_faced_permanent;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    ChosenAttribute, Effect, RoomCopiableHalves, RoomHalfIdentity, TargetFilter, TargetRef,
    TriggerDefinition,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card::LayoutKind;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, DayNight, GameState, ReplacementChoiceKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const MATRIARCH: &str = "Grub, Storied Matriarch";
const AUNTIE: &str = "Grub, Notorious Auntie";
const GOBLIN: &str = "Goblin Piker";
const SOLEMNITY: &str = "Solemnity";
const SLIP: &str = "Slip Out the Back";
const SLIP_ORACLE: &str = "Put a +1/+1 counter on target creature. It phases out. (Treat it and anything attached to it as though they don't exist until its controller's next turn.)";
const CONCEALMENT: &str = "Clever Concealment";
const CHIEFTAIN: &str = "Goblin Chieftain";
const GRAVEDIGGER: &str = "Gravedigger";
const ANTHEM: &str = "Glorious Anthem";
const VALENTIN: &str = "Valentin, Dean of the Vein";
const LISETTE: &str = "Lisette, Dean of the Root";
const PAINTER: &str = "Painter's Servant";
const PROWLER: &str = "Kessig Prowler";
const PREDATOR: &str = "Sinuous Predator";
const SAGE: &str = "Sage of Ancient Lore";
const WEREWOLF_OF_ANCIENT_HUNGER: &str = "Werewolf of Ancient Hunger";
const FELDON: &str = "Feldon of the Third Path";
const STIFLE: &str = "Stifle";
const SOUL_SEPARATOR: &str = "Soul Separator";
const THING: &str = "Thing in the Ice";
const HORROR: &str = "Awoken Horror";
const KIKI: &str = "Kiki-Jiki, Mirror Breaker";
const CACKLING: &str = "Cackling Counterpart";
const MIRRORWEAVE: &str = "Mirrorweave";
const ELITE_VANGUARD: &str = "Elite Vanguard";
const MYSTIC_REFLECTION: &str = "Mystic Reflection";
const RAISE_THE_ALARM: &str = "Raise the Alarm";
const DOUBLING_SEASON: &str = "Doubling Season";
const BEARS: &str = "Grizzly Bears";
const CYBER_CONVERSION: &str = "Cyber Conversion";
const MOONMIST: &str = "Moonmist";
const AVIATOR: &str = "Encouraging Aviator";
const BONECRUSHER: &str = "Bonecrusher Giant";
const STOMP: &str = "Stomp";
const WAXING_MOON: &str = "Waxing Moon";
const CONVERTER_BEAST: &str = "Converter Beast";
const KARNS_TOUCH: &str = "Karn's Touch";
const MIRROR_ROOM: &str = "Mirror Room";
const FRACTURED_REALM: &str = "Fractured Realm";
const OPALESCENCE: &str = "Opalescence";
const GIDEON: &str = "Gideon, Ally of Zendikar";
const SIN: &str = "Sin, Spira's Punishment";
const CONNOISSEUR: &str = "Mutagen Connoisseur";
const GEMRAZER: &str = "Gemrazer";
const SCOUNDREL: &str = "Baneblade Scoundrel";
const MARAUDER: &str = "Baneclaw Marauder";
const INTO_THE_NIGHT: &str = "Into the Night";
const CELESTUS: &str = "The Celestus";
const BRUTAL_CATHAR: &str = "Brutal Cathar";
const MOONRAGE_BRUTE: &str = "Moonrage Brute";
const ELESH_NORN: &str = "Elesh Norn, Grand Cenobite";
const CULT: &str = "Cult of the Waxing Moon";
const ENGINEERED_PLAGUE: &str = "Engineered Plague";
const ARCANE_ARTISAN: &str = "Arcane Artisan";
const TOVOLAR: &str = "Tovolar, Dire Overlord";
const MIDNIGHT_SCOURGE: &str = "Tovolar, the Midnight Scourge";

const AUNTIE_ORACLE: &str = "Menace\nWhenever Grub attacks, you may blight 1. If you do, create a tapped and attacking token that's a copy of the blighted creature, except it has \"At the beginning of the end step, sacrifice this token.\"\nAt the beginning of your first main phase, you may pay {B}. If you do, transform Grub.";
const NEXUS_ORACLE: &str = "At the beginning of combat on your turn, draw a card. Then you may exile an artifact or creature card from your hand. If you do, create a token that's a copy of the exiled card, except it's a 3/3 Golem artifact creature in addition to its other types.";
const HAKBAL_ORACLE: &str = "At the beginning of combat on your turn, each Merfolk creature you control explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on the exploring creature, then put the card back or put it into your graveyard.)\nWhenever Hakbal attacks, you may put a land card from your hand onto the battlefield. If you don't, draw a card.";
const GODS_GIFT_ORACLE: &str = "At the beginning of combat on your turn, you may exile a creature card from your graveyard. If you do, create a token that's a copy of that card, except it's a 4/4 black Zombie. It gains haste until end of turn.";
const AEGIS_ORACLE: &str = "Flying\nWard {2} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {2}.)\nAt the beginning of your upkeep, you may exile two cards from your graveyard. If you do, put a +1/+1 counter on this creature.";
const KEEPERS_ORACLE: &str = "This creature can't have counters put on it.";
const GANG_ORACLE: &str = "At the beginning of your first main phase, you may blight 2. If you don't, you lose 3 life. (To blight 2, put two -1/-1 counters on a creature you control.)";
const SCROUNGER_ORACLE: &str = "At the beginning of your first main phase, you may blight 1. If you do, create a Treasure token. (To blight 1, put a -1/-1 counter on a creature you control. A Treasure token is an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";
const TORCHMASTER_ORACLE: &str = "At the beginning of combat on your turn, you may blight 1. When you do, target creature gains haste until end of turn. (To blight 1, put a -1/-1 counter on a creature you control.)";
const SHOW_AND_TELL_ORACLE: &str = "Each player may put an artifact, creature, enchantment, or land card from their hand onto the battlefield.";
const CHIEFTAIN_ORACLE: &str = "Haste (This creature can attack and {T} as soon as it comes under your control.)\nOther Goblin creatures you control get +1/+1 and have haste.";
const GRAVEDIGGER_ORACLE: &str = "When this creature enters, you may return target creature card from your graveyard to your hand.";
const ANTHEM_ORACLE: &str = "Creatures you control get +1/+1.";
const VALENTIN_ORACLE: &str = "Menace, lifelink\nIf a nontoken creature an opponent controls would die, exile it instead. When you do, you may pay {2}. If you do, create a 1/1 black and green Pest creature token with \"When this token dies, you gain 1 life.\"";
const LISETTE_ORACLE: &str = "Whenever you gain life, you may pay {1}. If you do, put a +1/+1 counter on each creature you control and those creatures gain trample until end of turn.";
const PAINTER_ORACLE: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";
const PROWLER_ORACLE: &str = "{4}{G}: Transform this creature.";
const PREDATOR_ORACLE: &str = "This creature can't be blocked by more than one creature.";
const SAGE_ORACLE: &str = "Sage of Ancient Lore's power and toughness are each equal to the number of cards in your hand.\nWhen this creature enters, draw a card.\nAt the beginning of each upkeep, if no spells were cast last turn, transform this creature.";
const WEREWOLF_OF_ANCIENT_HUNGER_ORACLE: &str = "Vigilance, trample\nWerewolf of Ancient Hunger's power and toughness are each equal to the total number of cards in all players' hands.\nAt the beginning of each upkeep, if a player cast two or more spells last turn, transform this creature.";
const FELDON_ORACLE: &str = "{2}{R}, {T}: Create a token that's a copy of target creature card in your graveyard, except it's an artifact in addition to its other types. It gains haste. Sacrifice it at the beginning of the next end step.";
const STIFLE_ORACLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";
const SOUL_SEPARATOR_ORACLE: &str = "{5}, {T}, Sacrifice this artifact: Exile target creature card from your graveyard. Create a token that's a copy of that card, except it's 1/1, it's a Spirit in addition to its other types, and it has flying. Create a black Zombie creature token with power equal to that card's power and toughness equal to that card's toughness.";
const THING_ORACLE: &str = "Defender\nThis creature enters with four ice counters on it.\nWhenever you cast an instant or sorcery spell, remove an ice counter from this creature. Then if it has no ice counters on it, transform it.";
const HORROR_ORACLE: &str = "When this creature transforms into Awoken Horror, return all non-Horror creatures to their owners' hands.";
const KIKI_ORACLE: &str = "Haste\n{T}: Create a token that's a copy of target nonlegendary creature you control, except it has haste. Sacrifice it at the beginning of the next end step.";
const CACKLING_ORACLE: &str = "Create a token that's a copy of target creature you control.\nFlashback {5}{U}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const MIRRORWEAVE_ORACLE: &str =
    "Each other creature becomes a copy of target nonlegendary creature until end of turn.";
const MYSTIC_REFLECTION_ORACLE: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.\nForetell {U} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
const RAISE_THE_ALARM_ORACLE: &str = "Create two 1/1 white Soldier creature tokens.";
const CYBER_CONVERSION_ORACLE: &str =
    "Turn target creature face down. It's a 2/2 Cyberman artifact creature.";
const MOONMIST_ORACLE: &str = "Transform all Humans. Prevent all combat damage that would be dealt this turn by creatures other than Werewolves and Wolves. (Only double-faced cards can be transformed.)";
const AVIATOR_ORACLE: &str = "Flying\nWhenever this creature attacks, it becomes prepared. (While it's prepared, you may cast a copy of its spell. Doing so unprepares it.)";
const BONECRUSHER_ORACLE: &str = "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.";
const WAXING_MOON_ORACLE: &str = "Transform up to one target Werewolf you control. Creatures you control gain trample until end of turn.";
const CONVERTER_BEAST_ORACLE: &str = "When this creature enters, incubate 5. (Create an Incubator token with five +1/+1 counters on it and \"{2}: Transform this token.\" It transforms into a 0/0 Phyrexian artifact creature.)";
const KARNS_TOUCH_ORACLE: &str = "Target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value until end of turn. (It retains its abilities.)";
const STOMP_ORACLE: &str =
    "Damage can't be prevented this turn. Stomp deals 2 damage to any target.";
const MIRROR_ROOM_ORACLE: &str = "When you unlock this door, create a token that's a copy of target creature you control, except it's a Reflection in addition to its other creature types.\n(You may cast either half. That door unlocks on the battlefield. As a sorcery, you may pay the mana cost of a locked door to unlock it.)";
const FRACTURED_REALM_ORACLE: &str = "If a triggered ability of a permanent you control triggers, that ability triggers an additional time.\n(You may cast either half. That door unlocks on the battlefield. As a sorcery, you may pay the mana cost of a locked door to unlock it.)";
const OPALESCENCE_ORACLE: &str = "Each other non-Aura enchantment is a creature in addition to its other types and has base power and base toughness each equal to its mana value.";
const GIDEON_ORACLE: &str = "[+1]: Until end of turn, Gideon becomes a 5/5 Human Soldier Ally creature with indestructible that's still a planeswalker. Prevent all damage that would be dealt to him this turn.\n[0]: Create a 2/2 white Knight Ally creature token.\n[−4]: You get an emblem with \"Creatures you control get +1/+1.\"";
const SIN_ORACLE: &str = "Flying\nWhenever Sin enters or attacks, exile a permanent card from your graveyard at random, then create a tapped token that's a copy of that card. If the exiled card is a land card, repeat this process.";
const CONNOISSEUR_ORACLE: &str =
    "Flying, vigilance\nThis creature gets +1/+0 for each transformed permanent you control.";
const GEMRAZER_ORACLE: &str = "Mutate {1}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nReach, trample\nWhenever this creature mutates, destroy target artifact or enchantment an opponent controls.";
const SCOUNDREL_ORACLE: &str = "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.\nDaybound (If a player casts no spells during their own turn, it becomes night next turn.)";
const MARAUDER_ORACLE: &str = "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.\nWhenever a creature blocking this creature dies, that creature's controller loses 1 life.\nNightbound (If a player casts at least two spells during their own turn, it becomes day next turn.)";
const MEEKSTONE: &str = "Meekstone";
const MEEKSTONE_ORACLE: &str =
    "Creatures with power 3 or greater don't untap during their controllers' untap steps.";
const REALITY_RIPPLE: &str = "Reality Ripple";
const REALITY_RIPPLE_ORACLE: &str = "Target artifact, creature, or land phases out. (While it's phased out, it's treated as though it doesn't exist. It phases in before its controller untaps during their next untap step.)";
const GATSTAF_SHEPHERD: &str = "Gatstaf Shepherd";
const GATSTAF_SHEPHERD_ORACLE: &str =
    "At the beginning of each upkeep, if no spells were cast last turn, transform this creature.";
const GATSTAF_HOWLER: &str = "Gatstaf Howler";
const GATSTAF_HOWLER_ORACLE: &str = "Intimidate (This creature can't be blocked except by artifact creatures and/or creatures that share a color with it.)\nAt the beginning of each upkeep, if a player cast two or more spells last turn, transform this creature.";
const BRUTAL_CATHAR_ORACLE: &str = "Whenever this creature enters or transforms into Brutal Cathar, exile target creature an opponent controls until this creature leaves the battlefield.\nDaybound (If a player casts no spells during their own turn, it becomes night next turn.)";
const MOONRAGE_BRUTE_ORACLE: &str = "First strike\nWard—Pay 3 life.\nNightbound (If a player casts at least two spells during their own turn, it becomes day next turn.)";
const ELESH_NORN_ORACLE: &str = "Vigilance\nOther creatures you control get +2/+2.\nCreatures your opponents control get -2/-2.";
const CULT_ORACLE: &str = "Whenever a permanent you control transforms into a non-Human creature, create a 2/2 green Wolf creature token.";
const ENGINEERED_PLAGUE_ORACLE: &str =
    "As this enchantment enters, choose a creature type.\nAll creatures of the chosen type get -1/-1.";
const ARCANE_ARTISAN_ORACLE: &str = "{2}{U}, {T}: Target player draws a card, then exiles a card from their hand. If a creature card is exiled this way, that player creates a token that's a copy of that card.\nWhen this creature leaves the battlefield, exile all tokens created with it at the beginning of the next end step.";
const TOVOLAR_ORACLE: &str = "Whenever a Wolf or Werewolf you control deals combat damage to a player, draw a card.\nAt the beginning of your upkeep, if you control three or more Wolves and/or Werewolves, it becomes night. Then transform any number of Human Werewolves you control.\nDaybound";
const MIDNIGHT_SCOURGE_ORACLE: &str = "Whenever a Wolf or Werewolf you control deals combat damage to a player, draw a card.\n{X}{R}{G}: Target Wolf or Werewolf you control gets +X/+0 and gains trample until end of turn.\nNightbound";
const UNTAP_UPKEEP_DRAW: &str = "Untap, Upkeep, Draw";
const UNTAP_UPKEEP_DRAW_ORACLE: &str = "Choose one \u{2014}\n\u{2022} After this phase, there is an additional untap step.\n\u{2022} After this phase, there is an additional upkeep step.\n\u{2022} After this phase, there is an additional draw step.\nEntwine {3} (Choose all of them if you pay the entwine cost.)";
const INTO_THE_NIGHT_ORACLE: &str =
    "It becomes night. Discard any number of cards, then draw that many cards plus one.";
const CELESTUS_ORACLE: &str = "If it's neither day nor night, it becomes day as The Celestus enters.\n{T}: Add one mana of any color.\n{3}, {T}: If it's night, it becomes day. Otherwise, it becomes night. Activate only as a sorcery.\nWhenever day becomes night or night becomes day, you gain 1 life. You may draw a card. If you do, discard a card.";
const DOUBLING_SEASON_ORACLE: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

/// A board at `phase`, with stocked libraries so a real draw step can't end the
/// game (CR 704.5b).
fn board<T>(
    phase: Phase,
    stage: impl FnOnce(&mut GameScenario, &'static CardDatabase) -> T,
) -> (GameRunner, T) {
    let db = load_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.with_library_top(P0, &["Top One", "Top Two", "Top Three"]);
    scenario.with_library_top(P1, &["Top One", "Top Two", "Top Three"]);
    let staged = stage(&mut scenario, db);
    (scenario.build(), staged)
}

/// A board with the real card on its front face, able to attack this turn.
fn grub_board<T>(
    phase: Phase,
    stage: impl FnOnce(&mut GameScenario, &'static CardDatabase) -> T,
) -> (GameRunner, ObjectId, T) {
    let (mut runner, (grub, staged)) = board(phase, |scenario, db| {
        let grub = scenario.add_real_card(P0, MATRIARCH, Zone::Battlefield, db);
        (grub, stage(scenario, db))
    });
    runner
        .state_mut()
        .objects
        .get_mut(&grub)
        .expect("Grub exists")
        .summoning_sick = false;
    (runner, grub, staged)
}

/// Setup only: puts a staged Grub on her back face without paying for it.
fn flip(runner: &mut GameRunner, grub: ObjectId) {
    engine::game::transform::transform_permanent(runner.state_mut(), grub, &mut Vec::new())
        .expect("Grub must transform");
    assert_eq!(runner.state().objects[&grub].name, AUNTIE);
}

fn attack_with(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("the attack must be legal");
}

/// Both players pass, resolving the top of the stack.
fn both_pass(runner: &mut GameRunner) {
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
    }
}

/// Passes through empty priority windows into `phase`. `advance_to_phase` can't
/// do this mid-step: its leading `auto_advance` re-enters the current step.
fn pass_into(runner: &mut GameRunner, phase: Phase) {
    for _ in 0..16 {
        if runner.state().phase == phase {
            return;
        }
        assert!(runner.state().stack.is_empty(), "the stack must be empty");
        both_pass(runner);
    }
    panic!("never reached {phase:?}; at {:?}", runner.state().phase);
}

fn assert_optional_prompt(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the option must be offered; got {:?}",
        runner.state().waiting_for
    );
}

fn decide(runner: &mut GameRunner, accept: bool) {
    assert_optional_prompt(runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("answering the optional effect must succeed");
}

/// Accepts the option, asserts the exact pool of the CR 701.68a choice, and
/// chooses `chosen`.
fn blight(runner: &mut GameRunner, pool: &[ObjectId], chosen: ObjectId) {
    decide(runner, true);
    let WaitingFor::EffectZoneChoice { player, cards, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 701.68a: blighting asks for a creature; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    let mut offered = cards;
    offered.sort();
    let mut expected = pool.to_vec();
    expected.sort();
    assert_eq!(offered, expected, "the blight pool");
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("blighting the chosen creature must succeed");
}

fn select_goblin(runner: &mut GameRunner, goblin: ObjectId) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the return trigger must ask for its target; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(goblin)],
        })
        .expect("targeting the Goblin must succeed");
}

fn assert_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(state.stack.is_empty(), "stack: {:?}", state.stack);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        state.waiting_for
    );
}

fn tokens(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token)
        .collect()
}

fn minus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Minus1Minus1)
        .copied()
        .unwrap_or(0)
}

fn is_attacking(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == id))
}

/// Live parser to runtime: this board is built from the Oracle text, so it is
/// the row a parser revert fails. `add_real_card` would read the baked fixture.
#[test]
fn auntie_from_oracle_text_creates_the_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let grub = scenario
        .add_creature_from_oracle(P0, AUNTIE, 2, 1, AUNTIE_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);

    assert_settled(&runner);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    assert_eq!(runner.state().objects[&created[0]].name, "Bear");
    assert!(is_attacking(&runner, created[0]));
}

/// CR 701.68c + CR 707.2 + CR 508.4: with two other creatures offered, the
/// token follows the choice and copies neither the counter nor the wrong body.
#[test]
fn auntie_copies_the_blighted_creature_as_a_tapped_attacking_token() {
    let (mut runner, grub, (bear, elf)) = grub_board(Phase::BeginCombat, |scenario, _| {
        (
            scenario.add_creature(P0, "Bear", 2, 2).id(),
            scenario.add_creature(P0, "Elf", 3, 3).id(),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear, elf], elf);

    assert_settled(&runner);
    assert_eq!(
        minus_counters(&runner, elf),
        1,
        "CR 701.68a: blight 1 puts one -1/-1 counter on the chosen creature"
    );
    assert_eq!(minus_counters(&runner, bear), 0);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, "Elf", "the token copies the blighted creature");
    assert!(token.tapped, "the token enters tapped");
    assert!(
        token.counters.is_empty(),
        "CR 707.2: counters aren't copied"
    );
    assert_eq!(
        (token.power, token.toughness),
        (Some(3), Some(3)),
        "CR 707.2: the copy has the printed P/T"
    );
    assert!(
        is_attacking(&runner, created[0]),
        "CR 508.4: the token enters attacking"
    );
    assert!(
        token.back_face.is_none() && !token.transformed,
        "CR 707.8a: a copy of a single-faced creature is a single-faced token"
    );
}

/// The reported half of #769: the token is gone at the end step, the original
/// isn't.
#[test]
fn auntie_token_is_sacrificed_at_the_end_step() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);
    assert_eq!(tokens(&runner).len(), 1, "reach-guard: the token exists");

    pass_into(&mut runner, Phase::End);
    runner.advance_until_stack_empty();

    assert_settled(&runner);
    assert!(
        tokens(&runner).is_empty(),
        "CR 707.9a: the copy's end-step trigger must sacrifice it"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "only the token is sacrificed"
    );
}

/// CR 701.68c + CR 704.3: state-based actions wait until the ability finishes,
/// so a creature the counter kills still provides the copy.
#[test]
fn auntie_copies_a_creature_the_blight_kills() {
    let (mut runner, grub, elf) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Elf", 1, 1).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, elf], elf);

    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&elf].zone,
        Zone::Graveyard,
        "the -1/-1 counter kills the 1/1"
    );
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the token still comes in");
    assert_eq!(runner.state().objects[&created[0]].name, "Elf");
}

#[test]
fn declining_the_blight_makes_no_token() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    decide(&mut runner, false);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty(), "no blight, no token");
    assert_eq!(
        minus_counters(&runner, bear),
        0,
        "declining the blight places no counter"
    );
}

/// CR 707.8 + CR 704.5f: Grub can blight herself. The copy has the face she
/// shows, and the token outlives her.
#[test]
fn auntie_blighting_herself_leaves_a_token_auntie() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, _| {
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], grub);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Graveyard);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the token still comes in");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, AUNTIE, "CR 707.8: the copy is the face up");
    assert_eq!((token.power, token.toughness), (Some(2), Some(1)));
    assert!(
        token.counters.is_empty(),
        "CR 707.2: counters aren't copied"
    );
    assert!(
        token.transformed,
        "CR 707.8a: the copy of a transformed Grub enters back face up"
    );
    assert_eq!(
        stored(&runner, created[0]),
        face(MATRIARCH, 2, 1, false),
        "CR 707.8a: the token's other face is the Matriarch"
    );
}

/// CR 614.1a: Doubling Season doubles the counters and the token, Vorinclex only
/// the counters. Two doublers commute, so the engine applies them with no
/// ordering pause, and the rider still has its referent.
#[test]
fn auntie_with_doublers_makes_two_tokens_of_the_creature_with_four_counters() {
    let (mut runner, grub, big) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_real_card(P0, "Doubling Season", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Vorinclex, Monstrous Raider", Zone::Battlefield, db);
        scenario.add_creature(P0, "Big", 6, 6).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    decide(&mut runner, true);
    runner
        .act(GameAction::SelectCards { cards: vec![big] })
        .expect("blighting the chosen creature must succeed");

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, big), 4, "each doubler doubles");
    let created = tokens(&runner);
    assert_eq!(created.len(), 2, "Doubling Season doubles the token");
    for token in created {
        assert_eq!(runner.state().objects[&token].name, "Big");
    }
}

/// Blights the Bear with Vizier of Remedies and Doubling Season out, whose counter
/// clauses don't commute, and applies `first` at the CR 616.1 pause. The blight
/// choice is answered before that pause, so the rider resumes on its binding.
fn blight_bear_applying_first(first: &str) -> (GameRunner, ObjectId) {
    let (mut runner, grub, (vizier, bear)) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_real_card(P0, "Doubling Season", Zone::Battlefield, db);
        (
            scenario.add_real_card(P0, "Vizier of Remedies", Zone::Battlefield, db),
            scenario.add_creature(P0, "Bear", 2, 2).id(),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, vizier, bear], bear);

    let WaitingFor::ReplacementChoice {
        player,
        candidates,
        kind,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 616.1: the order of these replacements matters; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "CR 616.1: the creature's controller chooses");
    assert_eq!(kind, ReplacementChoiceKind::Order);
    let mut offered: Vec<_> = candidates.iter().map(|c| c.source_name.as_str()).collect();
    offered.sort();
    assert_eq!(offered, ["Doubling Season", "Vizier of Remedies"]);
    let index = candidates
        .iter()
        .position(|c| c.source_name == first)
        .expect("the chosen replacement is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choosing a replacement must succeed");
    (runner, bear)
}

/// CR 701.68c: the rider copies the chosen creature whatever the counters came to.
/// Doubling Season's token clause doubles it in either order.
fn assert_two_attacking_bear_tokens(runner: &GameRunner) {
    assert_settled(runner);
    let created = tokens(runner);
    assert_eq!(created.len(), 2, "Doubling Season doubles the token");
    for id in created {
        let token = &runner.state().objects[&id];
        assert_eq!(token.name, "Bear", "the token copies the blighted creature");
        assert!(token.tapped, "the token enters tapped");
        assert!(
            is_attacking(runner, id),
            "CR 508.4: the token enters attacking"
        );
    }
}

/// CR 616.1 + CR 118.12: Vizier first leaves 1 minus 1, so no counter is put on the
/// Bear and there is nothing to double. "If you do" checks the choice to blight,
/// not what happened to the counter, so the tokens still come in.
#[test]
fn auntie_ordering_vizier_first_places_no_counter_and_still_makes_tokens() {
    let (runner, bear) = blight_bear_applying_first("Vizier of Remedies");

    assert_two_attacking_bear_tokens(&runner);
    assert_eq!(minus_counters(&runner, bear), 0, "1 minus 1, doubled");
}

/// CR 616.1: Doubling Season first makes 1 into 2, and Vizier takes one away.
#[test]
fn auntie_ordering_doubling_season_first_places_one_counter_and_makes_tokens() {
    let (runner, bear) = blight_bear_applying_first("Doubling Season");

    assert_two_attacking_bear_tokens(&runner);
    assert_eq!(minus_counters(&runner, bear), 1, "1 doubled, minus 1");
}

/// CR 701.68b + CR 608.2d: Grub dies with her trigger on the stack and she was
/// the only creature, so there is no blight to offer. The rider's referent is
/// never absent on an accepted blight.
#[test]
fn auntie_with_no_creature_left_cannot_blight() {
    let (mut runner, grub, bolt) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db)
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Grub's trigger is on the stack"
    );
    runner.cast(bolt).target_objects(&[grub]).commit();
    both_pass(&mut runner);
    assert_eq!(
        runner.state().objects[&grub].zone,
        Zone::Graveyard,
        "reach-guard: Lightning Bolt kills Grub"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty());
}

/// Slip Out the Back reads "Put a +1/+1 counter on target
/// creature. It phases out." Cast in response to Grub's trigger, it leaves the
/// Bear on the battlefield with its counter (CR 702.26d) but treated as though it
/// doesn't exist (CR 702.26b), so CR 701.68a's "a creature you control" can't be
/// the Bear, and the copy comes from a legal choice.
#[test]
fn blight_does_not_offer_a_phased_out_creature() {
    let (mut runner, grub, (bear, elf, slip)) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Blue);
        (
            scenario.add_creature(P0, "Bear", 2, 2).id(),
            scenario.add_creature(P0, "Elf", 3, 3).id(),
            scenario.add_real_card(P0, SLIP, Zone::Hand, db),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is on the stack"
    );
    runner.cast(slip).target_object(bear).commit();
    both_pass(&mut runner);
    let phased = &runner.state().objects[&bear];
    assert!(phased.is_phased_out(), "reach-guard: the Bear phased out");
    assert_eq!(phased.zone, Zone::Battlefield, "CR 702.26d: it didn't move");
    assert_eq!(
        phased.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "CR 702.26d: its counter stays on it"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);
    decide(&mut runner, true);
    let WaitingFor::EffectZoneChoice { mut cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "CR 701.68a: blighting asks for a creature; got {:?}",
            runner.state().waiting_for
        );
    };
    cards.sort();
    let mut expected = vec![grub, elf];
    expected.sort();
    assert_eq!(cards, expected, "the phased-out Bear is not offered");
    let prompt = runner.state().waiting_for.clone();
    assert!(
        runner
            .act(GameAction::SelectCards { cards: vec![bear] })
            .is_err(),
        "the phased-out Bear can't be chosen"
    );
    assert_eq!(
        runner.state().waiting_for,
        prompt,
        "a refused choice leaves the prompt open"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![elf] })
        .expect("blighting the Elf must succeed");

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, elf), 1);
    assert_eq!(minus_counters(&runner, bear), 0);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    assert_eq!(runner.state().objects[&created[0]].name, "Elf");
    assert!(
        is_attacking(&runner, created[0]),
        "CR 508.4: the token enters attacking"
    );
}

/// CR 701.68b + CR 608.2d + CR 702.26b: Slip Out the Back phases out Grub, her
/// controller's only creature, while her attack trigger waits. The trigger still
/// resolves (CR 113.7a), but no creature exists to blight, so the "you may" is
/// not offered and its rider stays closed.
#[test]
fn auntie_whose_only_creature_phased_out_cannot_blight() {
    let (mut runner, grub, slip) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Blue);
        scenario.add_real_card(P0, SLIP, Zone::Hand, db)
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    runner.cast(slip).target_object(grub).commit();
    both_pass(&mut runner);
    assert!(
        runner.state().objects[&grub].is_phased_out(),
        "reach-guard: Grub phased out"
    );
    assert!(
        !is_attacking(&runner, grub),
        "CR 506.4: a permanent that phases out is removed from combat"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: CR 113.7a, Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty(), "no blight, no token");
    assert_eq!(minus_counters(&runner, grub), 0);
}

/// CR 702.26b: Clever Concealment reads "Any number of target nonland
/// permanents you control phase out." With Solemnity ("Counters can't be put on
/// artifacts, creatures, enchantments, or lands.") phased out, Solemnity is
/// treated as though it doesn't exist and forbids nothing, so Grub can blight.
/// The paired negative is `auntie_cannot_blight_under_solemnity`.
#[test]
fn a_phased_out_solemnity_does_not_stop_the_blight() {
    let (mut runner, grub, (solemnity, bear, concealment)) =
        grub_board(Phase::BeginCombat, |scenario, db| {
            for color in [
                ManaColor::White,
                ManaColor::White,
                ManaColor::Blue,
                ManaColor::Blue,
            ] {
                scenario.add_basic_land(P0, color);
            }
            (
                scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db),
                scenario.add_creature(P0, "Bear", 2, 2).id(),
                scenario.add_real_card(P0, CONCEALMENT, Zone::Hand, db),
            )
        });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is on the stack"
    );
    runner
        .cast(concealment)
        .target_objects(&[solemnity])
        .commit();
    both_pass(&mut runner);
    assert!(
        runner.state().objects[&solemnity].is_phased_out(),
        "reach-guard: Solemnity phased out"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is still on the stack"
    );

    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, bear), 1);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "CR 701.68c: blighting makes one token");
    assert_eq!(runner.state().objects[&created[0]].name, "Bear");
}

/// CR 701.68b + CR 614.17: Solemnity reads "Counters can't be put on artifacts,
/// creatures, enchantments, or lands." so Grub can't blight at all.
#[test]
fn auntie_cannot_blight_under_solemnity() {
    let (mut runner, grub, bear) = grub_board(Phase::BeginCombat, |scenario, db| {
        scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
        scenario.add_creature(P0, "Bear", 2, 2).id()
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Grub's trigger is on the stack"
    );
    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty(), "no blight, no token");
    assert_eq!(minus_counters(&runner, bear), 0);
    assert_eq!(minus_counters(&runner, grub), 0);
}

/// CR 701.68b + CR 608.2d: Melira's Keepers "can't have counters put on it", so
/// it is not a creature the player can put the counter on.
#[test]
fn blight_does_not_offer_a_creature_that_cannot_take_the_counter() {
    let (mut runner, grub, (bear, keepers)) = grub_board(Phase::BeginCombat, |scenario, _| {
        (
            scenario.add_creature(P0, "Bear", 2, 2).id(),
            scenario
                .add_creature_from_oracle(P0, "Melira's Keepers", 4, 4, KEEPERS_ORACLE)
                .id(),
        )
    });
    flip(&mut runner, grub);

    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, bear], bear);

    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, keepers), 0);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1);
    assert_eq!(runner.state().objects[&created[0]].name, "Bear");
}

/// CR 118.12 + CR 701.68b: Gutsplitter Gang reads "you may blight 2. If you
/// don't, you lose 3 life." A player who can't blight doesn't, so the life is
/// lost with no prompt.
#[test]
fn gutsplitter_gang_that_cannot_blight_loses_three_life() {
    let (mut runner, ()) = board(Phase::Upkeep, |scenario, db| {
        scenario.add_creature_from_oracle(P0, "Gutsplitter Gang", 6, 6, GANG_ORACLE);
        scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
    });

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: the first-main trigger is on the stack"
    );
    both_pass(&mut runner);

    assert_settled(&runner);
    assert_eq!(runner.state().players[0].life, 17);
}

/// CR 118.12 + CR 701.68b: Scuzzback Scrounger reads "you may blight 1. If you do,
/// create a Treasure token." A player who can't blight never does, so the rider
/// stays closed. The paired board is the same line with the blight available.
#[test]
fn scuzzback_scrounger_that_cannot_blight_makes_no_treasure() {
    let stage = |solemnity: bool| {
        move |scenario: &mut GameScenario, db: &'static CardDatabase| {
            if solemnity {
                scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
            }
            scenario
                .add_creature_from_oracle(P0, "Scuzzback Scrounger", 3, 2, SCROUNGER_ORACLE)
                .id()
        }
    };

    let (mut able, scrounger) = board(Phase::Upkeep, stage(false));
    able.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut able);
    blight(&mut able, &[scrounger], scrounger);
    assert_settled(&able);
    assert_eq!(tokens(&able).len(), 1, "blighting makes the Treasure");

    let (mut unable, _) = board(Phase::Upkeep, stage(true));
    unable.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        unable.state().stack.len(),
        1,
        "reach-guard: the first-main trigger is on the stack"
    );
    both_pass(&mut unable);

    assert_settled(&unable);
    assert!(tokens(&unable).is_empty(), "no blight, no Treasure");
}

/// CR 701.68b + CR 118.12: a blight nested under another accepted "you may"
/// still can't be chosen when no creature can take the counter, so its own
/// "If you do" stays closed. No printed card nests an optional blight this way
/// (every printed optional blight starts its ability, as Scuzzback Scrounger's
/// does), so this line is synthetic. It guards the inherited "performed" flag
/// from the accepted draw. The paired board has a creature, so the nested
/// blight is offered and its rider makes the Treasure.
#[test]
fn nested_blight_that_cannot_happen_keeps_its_rider_closed() {
    const NESTED_ORACLE: &str = "At the beginning of your first main phase, you may draw a card. \
If you do, you may blight 1. If you do, create a Treasure token.";
    let stage = |with_creature: bool| {
        move |scenario: &mut GameScenario, _: &'static CardDatabase| {
            scenario.add_artifact_from_oracle(P0, "Nested Blight Test Artifact", NESTED_ORACLE);
            with_creature.then(|| scenario.add_creature(P0, "Bear", 2, 2).id())
        }
    };

    let (mut able, bear) = board(Phase::Upkeep, stage(true));
    let bear = bear.expect("the paired board has a creature");
    able.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        able.state().stack.len(),
        1,
        "reach-guard: the first-main trigger is on the stack"
    );
    both_pass(&mut able);
    decide(&mut able, true);
    blight(&mut able, &[bear], bear);
    assert_settled(&able);
    assert_eq!(
        tokens(&able).len(),
        1,
        "the nested blight happened, so its rider makes the Treasure"
    );

    let (mut unable, _) = board(Phase::Upkeep, stage(false));
    unable.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        unable.state().stack.len(),
        1,
        "reach-guard: the first-main trigger is on the stack"
    );
    both_pass(&mut unable);
    // The draw is the only choice: no creature is on the battlefield, so the
    // nested blight is never offered.
    decide(&mut unable, true);

    assert_settled(&unable);
    assert!(
        tokens(&unable).is_empty(),
        "the blight never happened, so its rider makes no Treasure"
    );
}

/// CR 603.12: Warren Torchmaster reads "you may blight 1. When you do, target
/// creature gains haste until end of turn." The paired board is the same line
/// with the blight available.
#[test]
fn warren_torchmaster_that_cannot_blight_grants_no_haste() {
    let stage = |solemnity: bool| {
        move |scenario: &mut GameScenario, db: &'static CardDatabase| {
            if solemnity {
                scenario.add_real_card(P0, SOLEMNITY, Zone::Battlefield, db);
            }
            (
                scenario
                    .add_creature_from_oracle(P0, "Warren Torchmaster", 2, 2, TORCHMASTER_ORACLE)
                    .id(),
                scenario.add_creature(P0, "Bear", 2, 2).id(),
            )
        }
    };

    let (mut able, (torchmaster, bear)) = board(Phase::PreCombatMain, stage(false));
    able.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut able);
    blight(&mut able, &[torchmaster, bear], torchmaster);
    assert!(
        matches!(
            able.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "CR 603.12: the blight fires the reflexive trigger; got {:?}",
        able.state().waiting_for
    );

    let (mut unable, (torchmaster, bear)) = board(Phase::PreCombatMain, stage(true));
    unable.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        unable.state().stack.len(),
        1,
        "reach-guard: the beginning-of-combat trigger is on the stack"
    );
    both_pass(&mut unable);

    assert_settled(&unable);
    for id in [torchmaster, bear] {
        assert!(!unable.state().objects[&id].has_keyword(&Keyword::Haste));
    }
}

/// CR 400.7j + CR 707.9: Nexus of Becoming's copy source is the card its own
/// exile just moved, so a referent bound by an effect reads the same ladder as
/// one bound by a cost.
#[test]
fn nexus_of_becoming_copies_the_card_exiled_this_way() {
    let (mut runner, (goblin, bears)) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        scenario.add_land_to_library_top(P0, "Forest");
        (
            scenario.add_real_card(P0, GOBLIN, Zone::Hand, db),
            scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db),
        )
    });

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, true);
    let WaitingFor::EffectZoneChoice { mut cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the exile must ask for a card; got {:?}",
            runner.state().waiting_for
        );
    };
    cards.sort();
    let mut expected = vec![goblin, bears];
    expected.sort();
    assert_eq!(cards, expected, "the land drawn is not offered");
    runner
        .act(GameAction::SelectCards {
            cards: vec![goblin],
        })
        .expect("exiling the Goblin must succeed");

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Exile);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the exiled card provides one token");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, GOBLIN);
    assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Golem"));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Goblin"));
}

/// With one eligible card the exile takes it without a selection prompt, and the
/// referent still reaches the copy.
#[test]
fn nexus_of_becoming_copies_the_only_eligible_card() {
    let (mut runner, goblin) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        scenario.add_land_to_library_top(P0, "Forest");
        scenario.add_real_card(P0, GOBLIN, Zone::Hand, db)
    });

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Exile);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the exiled card provides one token");
    assert_eq!(runner.state().objects[&created[0]].name, GOBLIN);
}

/// "If you do" stays closed when the Nexus is declined with a legal card in hand.
#[test]
fn nexus_of_becoming_declined_makes_no_token() {
    let (mut runner, goblin) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        scenario.add_land_to_library_top(P0, "Forest");
        scenario.add_real_card(P0, GOBLIN, Zone::Hand, db)
    });

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, false);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty());
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Hand);
}

/// Asserts the real card's Oracle text is `oracle`, so the fixture parse the
/// test runs is the card quoted in its doc.
fn assert_oracle(db: &CardDatabase, name: &str, oracle: &str) {
    assert_eq!(
        db.get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the fixture"))
            .oracle_text
            .as_deref(),
        Some(oracle)
    );
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Nexus of Becoming: "At the beginning of combat on your turn, draw a card.
/// Then you may exile an artifact or creature card from your hand. If you do,
/// create a token that's a copy of the exiled card, except it's a 3/3 Golem
/// artifact creature in addition to its other types."
///
/// CR 118.12 + CR 118.3 + CR 608.2d: with no artifact or creature card in hand
/// after the draw, the exile is impossible, so it is not offered and "If you do"
/// stays closed.
#[test]
fn nexus_with_no_eligible_card_offers_no_exile() {
    let (mut runner, (plains, forest)) = board(Phase::PreCombatMain, |scenario, _| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        let forest = scenario.add_land_to_library_top(P0, "Forest").id();
        let plains = scenario.add_land_to_hand(P0, "Plains").id();
        (plains, forest)
    });

    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().stack.len(), 1, "reach: the Nexus trigger");
    both_pass(&mut runner);

    assert_settled(&runner);
    assert!(tokens(&runner).is_empty());
    assert_eq!(runner.state().objects[&plains].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Hand,
        "the draw ran, so the trigger resolved"
    );
}

/// Nexus of Becoming (Oracle text above): CR 608.2c, "draw a card" happens
/// before "Then you may exile", so the drawn creature card is in the pool.
#[test]
fn nexus_draw_feeds_the_exile_pool() {
    let (mut runner, (plains, bears)) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
        let plains = scenario.add_land_to_hand(P0, "Plains").id();
        (plains, bears)
    });
    // Setup only: Grizzly Bears is the card the draw takes.
    {
        let library = &mut runner.state_mut().players[0].library;
        library.retain(|id| *id != bears);
        library.insert(0, bears);
    }

    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&plains].zone, Zone::Hand);
    let created = tokens(&runner);
    assert_eq!(created.len(), 1, "the exiled card provides one token");
    let token = &runner.state().objects[&created[0]];
    assert_eq!(token.name, "Grizzly Bears");
    assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Golem"));
}

/// Hakbal of the Surging Soul: "At the beginning of combat on your turn, each
/// Merfolk creature you control explores. (…) Whenever Hakbal attacks, you may
/// put a land card from your hand onto the battlefield. If you don't, draw a
/// card."
///
/// CR 118.12 + CR 608.2d: with no land card in hand the put can't be chosen, so
/// "If you don't" draws without a prompt. With a land card the prompt opens.
#[test]
fn hakbal_with_no_land_in_hand_draws_without_a_prompt() {
    for land_in_hand in [false, true] {
        let (mut runner, (hakbal, card)) = board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, "Hakbal of the Surging Soul", HAKBAL_ORACLE);
            let hakbal =
                scenario.add_real_card(P0, "Hakbal of the Surging Soul", Zone::Battlefield, db);
            let card = if land_in_hand {
                scenario.add_land_to_hand(P0, "Forest").id()
            } else {
                scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db)
            };
            (hakbal, card)
        });

        runner.advance_to_phase(Phase::BeginCombat);
        assert_eq!(runner.state().stack.len(), 1, "reach: the explore trigger");
        both_pass(&mut runner);
        let WaitingFor::DigChoice { cards, .. } = runner.state().waiting_for.clone() else {
            panic!(
                "the explore asks about the revealed card; got {:?}",
                runner.state().waiting_for
            );
        };
        runner
            .act(GameAction::SelectCards { cards })
            .expect("keeping the revealed card on top must succeed");
        pass_into(&mut runner, Phase::DeclareAttackers);
        let hand_before = runner.state().players[0].hand.len();
        runner
            .declare_attackers(&[(hakbal, AttackTarget::Player(P1))])
            .expect("the attack must be legal");
        assert_eq!(runner.state().stack.len(), 1, "reach: the attack trigger");
        both_pass(&mut runner);

        if land_in_hand {
            decide(&mut runner, false);
        }
        assert_settled(&runner);
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + 1,
            "land in hand: {land_in_hand}; \"If you don't\" draws a card"
        );
        assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    }
}

/// God-Pharaoh's Gift: "At the beginning of combat on your turn, you may exile a
/// creature card from your graveyard. If you do, create a token that's a copy of
/// that card, except it's a 4/4 black Zombie. It gains haste until end of turn."
///
/// CR 118.12 + CR 608.2d: an empty graveyard can't supply the exile, so it is
/// not offered and no token is made. A creature card there opens the prompt.
#[test]
fn gods_gift_with_an_empty_graveyard_offers_no_exile() {
    let (mut empty, ()) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, "God-Pharaoh's Gift", GODS_GIFT_ORACLE);
        scenario.add_real_card(P0, "God-Pharaoh's Gift", Zone::Battlefield, db);
    });
    empty.advance_to_phase(Phase::BeginCombat);
    assert_eq!(empty.state().stack.len(), 1, "reach: the Gift trigger");
    both_pass(&mut empty);
    assert_settled(&empty);
    assert!(tokens(&empty).is_empty());

    let (mut stocked, ()) = board(Phase::PreCombatMain, |scenario, db| {
        scenario.add_real_card(P0, "God-Pharaoh's Gift", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    });
    stocked.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut stocked);
    assert_optional_prompt(&stocked);
}

/// Aegis Sculptor: "Flying / Ward {2} (…) / At the beginning of your upkeep, you
/// may exile two cards from your graveyard. If you do, put a +1/+1 counter on
/// this creature."
///
/// CR 118.3 + CR 608.2d + CR 118.12: "two cards" needs two whatever the
/// graveyard holds, so an empty graveyard can't supply the exile: it is not
/// offered and no counter is put. Two cards there open the prompt.
#[test]
fn aegis_sculptor_with_an_empty_graveyard_offers_no_exile() {
    let (mut empty, aegis) = board(Phase::Untap, |scenario, db| {
        assert_oracle(db, "Aegis Sculptor", AEGIS_ORACLE);
        scenario.add_real_card(P0, "Aegis Sculptor", Zone::Battlefield, db)
    });
    empty.advance_to_phase(Phase::Upkeep);
    assert_eq!(empty.state().stack.len(), 1, "reach: the upkeep trigger");
    assert_eq!(empty.state().stack[0].source_id, aegis);
    both_pass(&mut empty);
    assert_settled(&empty);
    assert_eq!(
        plus_counters(&empty, aegis),
        0,
        "\"If you do\" stays closed"
    );

    let (mut stocked, (aegis, cards)) = board(Phase::Untap, |scenario, db| {
        let aegis = scenario.add_real_card(P0, "Aegis Sculptor", Zone::Battlefield, db);
        let cards: Vec<ObjectId> = (0..2)
            .map(|_| scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db))
            .collect();
        (aegis, cards)
    });
    stocked.advance_to_phase(Phase::Upkeep);
    both_pass(&mut stocked);
    decide(&mut stocked, false);
    assert_settled(&stocked);
    assert_eq!(plus_counters(&stocked, aegis), 0);
    for id in cards {
        assert_eq!(stocked.state().objects[&id].zone, Zone::Graveyard);
    }
}

/// Show and Tell: "Each player may put an artifact, creature, enchantment, or
/// land card from their hand onto the battlefield."
///
/// CR 101.4 + CR 608.2d: each player makes their own choice, from their own
/// hand, and can't choose an impossible option. With Grizzly Bears in one hand
/// and the other hand empty, only the holder is asked, and the empty seat is
/// not. Each board is the other with the seats swapped.
#[test]
fn show_and_tell_asks_only_the_seat_with_an_eligible_card() {
    for holder in [P1, P0] {
        let (mut runner, (show_and_tell, bears)) = board(Phase::PreCombatMain, |scenario, db| {
            let show_and_tell = scenario
                .add_spell_to_hand_from_oracle(P0, "Show and Tell", false, SHOW_AND_TELL_ORACLE)
                .with_mana_cost(ManaCost::generic(0))
                .id();
            let bears = scenario.add_real_card(holder, "Grizzly Bears", Zone::Hand, db);
            (show_and_tell, bears)
        });

        runner.cast(show_and_tell).commit();
        for seat in [P0, P1] {
            let expected = usize::from(seat == holder);
            assert_eq!(
                runner.state().players[seat.0 as usize].hand.len(),
                expected,
                "holder {holder:?}: after the cast, {seat:?}'s hand"
            );
        }
        assert_eq!(runner.state().stack.len(), 1, "reach: Show and Tell");
        both_pass(&mut runner);

        let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for.clone()
        else {
            panic!(
                "holder {holder:?}: the holder must be asked; got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, holder, "the empty seat is not asked");
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accepting the put must succeed");
        if let WaitingFor::EffectZoneChoice { player, cards, .. } =
            runner.state().waiting_for.clone()
        {
            assert_eq!((player, cards), (holder, vec![bears]));
            runner
                .act(GameAction::SelectCards { cards: vec![bears] })
                .expect("putting Grizzly Bears must succeed");
        }

        assert_settled(&runner);
        let entered = &runner.state().objects[&bears];
        assert_eq!(entered.zone, Zone::Battlefield, "holder {holder:?}");
        assert_eq!(entered.controller, holder);
    }
}

/// The reported "doesn't trigger on cast": casting Grub reaches the enters half
/// of "enters or transforms into".
#[test]
fn matriarch_returns_a_goblin_when_cast() {
    let (mut runner, (grub, goblin)) = board(Phase::PreCombatMain, |scenario, db| {
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        (
            scenario.add_real_card(P0, MATRIARCH, Zone::Hand, db),
            scenario.add_real_card(P0, GOBLIN, Zone::Graveyard, db),
        )
    });

    runner.cast(grub).commit();
    both_pass(&mut runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Battlefield);
    select_goblin(&mut runner, goblin);
    both_pass(&mut runner);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Hand);
}

/// The reported "doesn't trigger on transform" (CR 701.27e): paying {B} on
/// Auntie transforms her into the Matriarch through her own first-main trigger.
#[test]
fn matriarch_returns_a_goblin_when_auntie_transforms_into_her() {
    let (mut runner, grub, goblin) = grub_board(Phase::Upkeep, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Black);
        scenario.add_real_card(P0, GOBLIN, Zone::Graveyard, db)
    });
    flip(&mut runner, grub);

    runner.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_eq!(
        runner.state().objects[&grub].name,
        MATRIARCH,
        "reach-guard: paying {{B}} must transform Grub"
    );
    select_goblin(&mut runner, goblin);
    both_pass(&mut runner);

    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Hand);
}

/// "Transforms into Grub, Storied Matriarch" is not "transforms": flipping to
/// Auntie leaves the graveyard alone.
#[test]
fn transforming_into_auntie_does_not_return_a_goblin() {
    let (mut runner, grub, goblin) = grub_board(Phase::Upkeep, |scenario, db| {
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_real_card(P0, GOBLIN, Zone::Graveyard, db)
    });

    runner.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut runner);
    decide(&mut runner, true);

    assert_eq!(
        runner.state().objects[&grub].name,
        AUNTIE,
        "reach-guard: paying {{R}} must transform Grub"
    );
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Graveyard);
}

// ===== CR 707.2 + CR 613.1: a face swap stores the face's copiable values =====

/// Runs a full layer pass, so every reading below is the settled one.
fn settle(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// Setup only: moves `id` to `zone` outside any spell or ability.
fn put(runner: &mut GameRunner, id: ObjectId, zone: Zone) {
    move_to_zone(runner.state_mut(), id, zone, &mut Vec::new());
    settle(runner);
}

/// Setup only: turns a staged Grub to her other face without paying for it.
fn turn(runner: &mut GameRunner, grub: ObjectId, into: &str) {
    engine::game::transform::transform_permanent(runner.state_mut(), grub, &mut Vec::new())
        .expect("Grub must transform");
    settle(runner);
    assert_eq!(runner.state().objects[&grub].name, into);
}

/// Setup only: turns a daybound or nightbound permanent through its own ability (CR 702.145b/e).
fn turn_by_designation(runner: &mut GameRunner, id: ObjectId, into: &str) {
    engine::game::transform::transform_permanent_with_cause(
        runner.state_mut(),
        id,
        engine::game::transform::TransformCause::DayNightDesignation,
        &mut Vec::new(),
    )
    .expect("designation must transform");
    settle(runner);
    assert_eq!(runner.state().objects[&id].name, into);
}

/// Adds `n` mana of one type to `player`'s pool.
fn fund_player(runner: &mut GameRunner, player: PlayerId, mana: ManaType, n: usize) {
    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    for _ in 0..n {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
}

/// Adds `n` mana of one type to P0's pool.
fn fund(runner: &mut GameRunner, mana: ManaType, n: usize) {
    fund_player(runner, P0, mana, n);
}

/// The displayed face's name, live P/T and live haste.
fn shown(runner: &GameRunner, id: ObjectId) -> (String, Option<i32>, Option<i32>, bool) {
    let obj = &runner.state().objects[&id];
    (
        obj.name.clone(),
        obj.power,
        obj.toughness,
        obj.keywords.contains(&Keyword::Haste),
    )
}

/// The stored face's name, P/T and haste.
fn stored(runner: &GameRunner, id: ObjectId) -> (String, Option<i32>, Option<i32>, bool) {
    let face: &BackFaceData = runner.state().objects[&id]
        .back_face
        .as_ref()
        .expect("the other face is stored");
    (
        face.name.clone(),
        face.power,
        face.toughness,
        face.keywords.contains(&Keyword::Haste),
    )
}

fn face(
    name: &str,
    power: i32,
    toughness: i32,
    haste: bool,
) -> (String, Option<i32>, Option<i32>, bool) {
    (name.to_string(), Some(power), Some(toughness), haste)
}

/// Casts Gravedigger and returns `card` from the graveyard with its trigger.
fn return_with_gravedigger(runner: &mut GameRunner, gravedigger: ObjectId, card: ObjectId) {
    fund(runner, ManaType::Black, 4);
    runner.cast(gravedigger).commit();
    both_pass(runner);
    assert_eq!(runner.state().objects[&gravedigger].zone, Zone::Battlefield);
    // The card is the only creature card in the graveyard, so the trigger
    // takes it as its target without a prompt.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: Gravedigger's trigger is on the stack"
    );
    both_pass(runner);
    decide(runner, true);
    assert_settled(runner);
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Hand,
        "Gravedigger returns the card"
    );
    settle(runner);
}

/// Casts the modal card in hand as its back face and resolves it.
fn cast_as_lisette(runner: &mut GameRunner, card: ObjectId) {
    fund(runner, ManaType::Green, 4);
    runner.cast(card).modal_back_face(true).commit();
    both_pass(runner);
    assert_settled(runner);
    settle(runner);
    assert_eq!(runner.state().objects[&card].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&card].name, LISETTE);
}

/// Goblin Chieftain: "Other Goblin creatures you control get +1/+1 and have
/// haste." Grub pays {R} under it, transforms back while it is out, and is read
/// again after it leaves. CR 613.1: each face starts from its printed 2/1 with
/// menace; the Chieftain's layer 7c P/T and layer 6 haste apply only while it
/// is on the battlefield (CR 611.3a), so neither may be stored with a face.
#[test]
fn a_transform_stores_the_face_without_the_chieftains_bonus() {
    let (mut runner, grub, chieftain) = grub_board(Phase::Upkeep, |scenario, db| {
        assert_oracle(db, CHIEFTAIN, CHIEFTAIN_ORACLE);
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_real_card(P0, CHIEFTAIN, Zone::Battlefield, db)
    });

    // (a) The production transform: Grub's own first-main trigger, paying {R}.
    runner.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut runner);
    decide(&mut runner, true);
    assert_settled(&runner);
    settle(&mut runner);
    assert_eq!(shown(&runner, grub), face(AUNTIE, 3, 2, true));
    assert_eq!(
        stored(&runner, grub),
        face(MATRIARCH, 2, 1, false),
        "CR 707.2: the stored Matriarch holds no Chieftain bonus"
    );

    // (b) Back to the Matriarch while the Chieftain is still out.
    turn(&mut runner, grub, MATRIARCH);
    assert_eq!(
        shown(&runner, grub),
        face(MATRIARCH, 3, 2, true),
        "CR 613.1: the bonus applies once, over the printed 2/1"
    );

    // (c) The Chieftain leaves: nothing of it remains.
    put(&mut runner, chieftain, Zone::Exile);
    assert_eq!(shown(&runner, grub), face(MATRIARCH, 2, 1, false));
    assert!(runner.state().objects[&grub]
        .keywords
        .contains(&Keyword::Menace));

    // (d) The Auntie stored at (b), under the Chieftain, returns printed.
    turn(&mut runner, grub, AUNTIE);
    assert_eq!(shown(&runner, grub), face(AUNTIE, 2, 1, false));
    assert!(runner.state().objects[&grub]
        .keywords
        .contains(&Keyword::Menace));
}

/// CR 712.8a + CR 400.7 + CR 707.2: Grub, Notorious Auntie dies under Goblin
/// Chieftain. The card in the graveyard shows its front face and stores the
/// Auntie as printed, so after Gravedigger ("When this creature enters, you may
/// return target creature card from your graveyard to your hand.") returns it
/// and it is recast and transformed with the Chieftain gone, the Auntie is the
/// printed 2/1. The transform happens with the Chieftain in exile, so only the
/// zone-exit revert can store the bonus.
#[test]
fn a_transformed_card_leaving_the_battlefield_stores_its_printed_back_face() {
    let (mut runner, grub, (chieftain, gravedigger)) =
        grub_board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, CHIEFTAIN, CHIEFTAIN_ORACLE);
            assert_oracle(db, GRAVEDIGGER, GRAVEDIGGER_ORACLE);
            (
                scenario.add_real_card(P0, CHIEFTAIN, Zone::Exile, db),
                scenario.add_real_card(P0, GRAVEDIGGER, Zone::Hand, db),
            )
        });
    flip(&mut runner, grub);
    put(&mut runner, chieftain, Zone::Battlefield);
    assert_eq!(
        shown(&runner, grub),
        face(AUNTIE, 3, 2, true),
        "reach-guard: the Chieftain modifies the Auntie on the battlefield"
    );

    put(&mut runner, grub, Zone::Graveyard);
    assert_eq!(
        shown(&runner, grub),
        face(MATRIARCH, 2, 1, false),
        "CR 712.8a: the graveyard card shows its front face"
    );
    assert_eq!(
        stored(&runner, grub),
        face(AUNTIE, 2, 1, false),
        "CR 707.2: the stored Auntie holds no Chieftain bonus"
    );

    put(&mut runner, chieftain, Zone::Exile);
    return_with_gravedigger(&mut runner, gravedigger, grub);
    fund(&mut runner, ManaType::Black, 3);
    runner.cast(grub).commit();
    runner.advance_until_stack_empty();
    assert_settled(&runner);
    settle(&mut runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Battlefield);
    assert_eq!(shown(&runner, grub), face(MATRIARCH, 2, 1, false));

    turn(&mut runner, grub, AUNTIE);
    assert_eq!(
        shown(&runner, grub),
        face(AUNTIE, 2, 1, false),
        "the recast card's Auntie is the printed 2/1"
    );
}

/// CR 712.8a + CR 707.2, the modal member: Lisette, Dean of the Root (4/4) is
/// cast back face up under Glorious Anthem ("Creatures you control get
/// +1/+1."), dies, and the graveyard card stores Lisette as printed. Returned by
/// Gravedigger and cast as Lisette again with the Anthem gone, she is 4/4.
#[test]
fn a_modal_card_leaving_the_battlefield_stores_its_printed_back_face() {
    let (mut runner, (card, anthem, gravedigger)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
        assert_oracle(db, LISETTE, LISETTE_ORACLE);
        assert_oracle(db, ANTHEM, ANTHEM_ORACLE);
        assert_oracle(db, GRAVEDIGGER, GRAVEDIGGER_ORACLE);
        (
            scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
            scenario.add_real_card(P0, ANTHEM, Zone::Battlefield, db),
            scenario.add_real_card(P0, GRAVEDIGGER, Zone::Hand, db),
        )
    });

    cast_as_lisette(&mut runner, card);
    assert_eq!(
        shown(&runner, card),
        face(LISETTE, 5, 5, false),
        "reach-guard: the Anthem modifies Lisette on the battlefield"
    );

    put(&mut runner, card, Zone::Graveyard);
    assert_eq!(
        shown(&runner, card),
        face(VALENTIN, 1, 1, false),
        "CR 712.8a: the graveyard card shows its front face"
    );
    assert_eq!(
        stored(&runner, card),
        face(LISETTE, 4, 4, false),
        "CR 707.2: the stored Lisette holds no Anthem bonus"
    );

    put(&mut runner, anthem, Zone::Exile);
    return_with_gravedigger(&mut runner, gravedigger, card);
    cast_as_lisette(&mut runner, card);
    assert_eq!(
        shown(&runner, card),
        face(LISETTE, 4, 4, false),
        "the recast Lisette is the printed 4/4"
    );
}

/// CR 611.3a + CR 707.2 + CR 712.11b: a face election off the battlefield
/// stores the face's copiable values too. Painter's Servant ("All cards that
/// aren't on the battlefield, spells, and permanents are the chosen color in
/// addition to their other colors.") paints Valentin, Dean of the Vein red in
/// hand; casting it as Lisette stores Valentin with its printed black only.
/// After Lisette dies and Painter's Servant leaves, the graveyard card is
/// black. A battlefield-only stash rule would keep the red.
#[test]
fn a_face_election_in_hand_stores_the_face_without_the_painted_color() {
    let (mut runner, (card, painter)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
        assert_oracle(db, LISETTE, LISETTE_ORACLE);
        assert_oracle(db, PAINTER, PAINTER_ORACLE);
        (
            scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
            scenario.add_real_card(P0, PAINTER, Zone::Battlefield, db),
        )
    });
    // CR 105.4: red was chosen as Painter's Servant entered.
    runner
        .state_mut()
        .objects
        .get_mut(&painter)
        .expect("Painter's Servant exists")
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&card].color,
        vec![ManaColor::Black, ManaColor::Red],
        "reach-guard: Painter's Servant paints the card in hand"
    );

    cast_as_lisette(&mut runner, card);
    let stored_colors = &runner.state().objects[&card]
        .back_face
        .as_ref()
        .expect("Valentin is stored")
        .color;
    assert_eq!(
        stored_colors,
        &vec![ManaColor::Black],
        "CR 707.2: the stored Valentin holds no painted color"
    );

    put(&mut runner, card, Zone::Graveyard);
    put(&mut runner, painter, Zone::Exile);
    let obj = &runner.state().objects[&card];
    assert_eq!(obj.name, VALENTIN);
    assert_eq!(
        obj.color,
        vec![ManaColor::Black],
        "the restored Valentin is printed black once Painter's Servant is gone"
    );
}

/// CR 733 + CR 701.27a: the journal replay of a resolved transform stores the
/// same face as the original execution, and that face is the Matriarch as
/// printed, though Goblin Chieftain modifies her when Grub pays {R}.
#[test]
fn a_replayed_transform_stores_the_same_printed_face() {
    let (mut runner, grub, ()) = grub_board(Phase::Upkeep, |scenario, db| {
        assert_oracle(db, CHIEFTAIN, CHIEFTAIN_ORACLE);
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_real_card(P0, CHIEFTAIN, Zone::Battlefield, db);
    });
    runner.advance_to_phase(Phase::PreCombatMain);
    both_pass(&mut runner);
    settle(&mut runner);
    assert_eq!(shown(&runner, grub), face(MATRIARCH, 3, 2, true));
    let mut replay = runner.state().clone();
    let journal_start = replay.resolved_rules_journal.entries().len();

    decide(&mut runner, true);
    assert_eq!(runner.state().objects[&grub].name, AUNTIE);
    let command = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .find_map(|command| match command {
            ResolvedRulesCommand::ObjectTransform(command) if command.object.object_id == grub => {
                Some(command)
            }
            _ => None,
        })
        .expect("reach-guard: the transform is journaled");
    engine::game::transform::apply_resolved_transform(&mut replay, &command)
        .expect("the replay applies");

    let original = runner.state().objects[&grub].back_face.as_ref();
    assert_eq!(replay.objects[&grub].back_face.as_ref(), original);
    assert_eq!(
        stored(&runner, grub),
        face(MATRIARCH, 2, 1, false),
        "CR 707.2: both store the printed Matriarch"
    );
}

// ===== CR 707.8a: a token copy of a double-faced object is double-faced =====

/// The stored face of `id`.
fn stored_face(runner: &GameRunner, id: ObjectId) -> &BackFaceData {
    runner.state().objects[&id]
        .back_face
        .as_ref()
        .expect("the other face is stored")
}

/// CR 708.2: Cyber Conversion turned `id` face down, with the characteristics
/// the spell lists: a 2/2 Cyberman artifact creature.
fn assert_cyber_converted(runner: &GameRunner, id: ObjectId) {
    let obj = &runner.state().objects[&id];
    assert!(
        obj.face_down,
        "reach-guard: Cyber Conversion turns a single-faced creature face down"
    );
    assert_eq!((obj.power, obj.toughness), (Some(2), Some(2)));
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    assert!(obj.card_types.subtypes.iter().any(|s| s == "Cyberman"));
}

/// CR 712.16: "If a spell or ability tries to turn a double-faced permanent face
/// down, nothing happens." `id` still shows `name`, is no Cyberman, and keeps
/// the stored face it had before the spell.
fn assert_not_turned_face_down(
    runner: &GameRunner,
    id: ObjectId,
    name: &str,
    stored_before: &BackFaceData,
) {
    let obj = &runner.state().objects[&id];
    assert!(
        !obj.face_down,
        "CR 712.16: a double-faced permanent stays face up"
    );
    assert_eq!(obj.name, name);
    assert!(
        !obj.card_types.subtypes.iter().any(|s| s == "Cyberman"),
        "CR 712.16: Cyber Conversion's characteristics never apply"
    );
    assert_eq!(
        stored_face(runner, id),
        stored_before,
        "CR 712.16: the other face is kept"
    );
    assert!(
        is_double_faced_permanent(obj),
        "CR 712.16: it is a double-faced permanent"
    );
}

/// CR 707.9a: whether `trigger` is the copy's "At the beginning of the end
/// step, sacrifice this token."
fn is_end_step_self_sacrifice(trigger: &TriggerDefinition) -> bool {
    trigger.mode == TriggerMode::Phase
        && trigger.phase == Some(Phase::End)
        && trigger.execute.as_ref().is_some_and(|execute| {
            matches!(
                &*execute.effect,
                Effect::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
        })
}

/// Whether the face up on `id` has the copy's end-step sacrifice trigger.
fn shown_sacrifices_itself(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .trigger_definitions
        .iter_all()
        .any(|entry| is_end_step_self_sacrifice(entry.definition()))
}

/// Whether the face stored on `id` has the copy's end-step sacrifice trigger.
fn stored_sacrifices_itself(runner: &GameRunner, id: ObjectId) -> bool {
    stored_face(runner, id)
        .trigger_definitions
        .iter_all()
        .any(is_end_step_self_sacrifice)
}

/// Activates `source`'s first ability, paying automatically.
fn activate(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the activation must be legal");
}

/// Passes priority until something is on the stack.
fn pass_until_stack(runner: &mut GameRunner) {
    for _ in 0..80 {
        if !runner.state().stack.is_empty() {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
    }
    panic!("nothing reached the stack");
}

/// Passes into this turn's end step, declaring no attackers. `pass_into`
/// cannot answer the declare-attackers prompt.
fn drive_to_end(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().phase == Phase::End {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declaring no attackers must succeed");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            other => panic!("unexpected prompt before the end step: {other:?}"),
        }
    }
    panic!("never reached the end step; at {:?}", runner.state().phase);
}

/// The id of the top stack entry, asserting the ability's source.
fn top_of_stack_from(runner: &GameRunner, source: ObjectId) -> ObjectId {
    let entry = runner.state().stack.back().expect("the stack has an entry");
    assert_eq!(
        entry.source_id, source,
        "the top of the stack is {source:?}'s"
    );
    entry.id
}

/// Grub, Notorious Auntie attacks with a Bear beside her, and her trigger asks
/// whether to blight. P0 holds two Stifles, two Cyber Conversions, two Islands
/// and a Swamp.
fn auntie_about_to_blight() -> (GameRunner, ObjectId, ObjectId, [ObjectId; 2], [ObjectId; 2]) {
    let (mut runner, grub, (bear, stifles, conversions)) =
        grub_board(Phase::BeginCombat, |scenario, db| {
            assert_oracle(db, STIFLE, STIFLE_ORACLE);
            assert_oracle(db, CYBER_CONVERSION, CYBER_CONVERSION_ORACLE);
            let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
            let stifles = [
                scenario.add_real_card(P0, STIFLE, Zone::Hand, db),
                scenario.add_real_card(P0, STIFLE, Zone::Hand, db),
            ];
            let conversions = [
                scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
                scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
            ];
            scenario.add_basic_land(P0, ManaColor::Blue);
            scenario.add_basic_land(P0, ManaColor::Blue);
            scenario.add_basic_land(P0, ManaColor::Black);
            (bear, stifles, conversions)
        });
    flip(&mut runner, grub);
    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    (runner, grub, bear, stifles, conversions)
}

/// The maintainer's row. CR 707.8a: Grub's copy of herself is back face up with
/// the Matriarch stored. Stifle ("Counter target activated or triggered
/// ability.") counters its sacrifice trigger at both end steps ("the end step",
/// CR 603.2). At P0's next first main phase the token's own trigger pays {B} and
/// transforms it into Grub, Storied Matriarch, whose "transforms into" trigger
/// returns Grub's card. CR 707.9a: the sacrifice exception is on both faces, so
/// the Matriarch is sacrificed at that turn's end step.
#[test]
fn grubs_copy_of_herself_transforms_into_the_matriarch() {
    let (mut runner, grub, bear, stifles, conversions) = auntie_about_to_blight();
    blight(&mut runner, &[grub, bear], grub);
    assert_settled(&runner);
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&grub].zone,
        Zone::Graveyard,
        "CR 704.5f: the counter kills Grub"
    );
    let token = tokens(&runner)[0];
    assert_eq!(shown(&runner, token), face(AUNTIE, 2, 1, false));
    assert!(
        runner.state().objects[&token].transformed,
        "CR 707.8a: the copy enters back face up"
    );
    assert_eq!(stored(&runner, token), face(MATRIARCH, 2, 1, false));
    assert!(stored_face(&runner, token)
        .keywords
        .contains(&Keyword::Menace));

    // P0's end step: the token's sacrifice trigger, Stifled.
    pass_until_stack(&mut runner);
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(runner.state().active_player, P0);
    let entry = top_of_stack_from(&runner, token);
    runner.cast(stifles[0]).target_object(entry).resolve();
    assert_eq!(runner.state().objects[&token].zone, Zone::Battlefield);

    // P1's end step: the trigger fires again; P1 passes and P0 Stifles it.
    pass_until_stack(&mut runner);
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(runner.state().active_player, P1);
    let entry = top_of_stack_from(&runner, token);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("passing priority must succeed");
    runner.cast(stifles[1]).target_object(entry).resolve();
    assert_eq!(runner.state().objects[&token].zone, Zone::Battlefield);

    // P0's next first main phase: the token's own trigger pays {B}.
    pass_until_stack(&mut runner);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    top_of_stack_from(&runner, token);
    both_pass(&mut runner);
    decide(&mut runner, true);
    settle(&mut runner);
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, MATRIARCH, "CR 701.27a: the token transforms");
    assert!(!obj.transformed, "CR 712.8d: the front face is up");
    assert_eq!(
        obj.effective_mana_value(),
        3,
        "CR 712.8d: the front face's mana value"
    );
    assert!(
        shown_sacrifices_itself(&runner, token),
        "CR 707.9a: the Matriarch face has the sacrifice exception"
    );
    assert_eq!(stored(&runner, token), face(AUNTIE, 2, 1, false));
    assert!(
        stored_sacrifices_itself(&runner, token),
        "CR 707.9a: the stored Auntie keeps the sacrifice exception"
    );
    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 701.27e: the Matriarch's trigger asks for its target; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots[0].legal_targets, vec![TargetRef::Object(grub)]);
    select_goblin(&mut runner, grub);
    both_pass(&mut runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Hand);

    // Cyber Conversion ("Turn target creature face down. It's a 2/2 Cyberman
    // artifact creature."): the Bear turns face down, the token doesn't.
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(conversions[0]).target_object(bear).resolve();
    settle(&mut runner);
    assert_cyber_converted(&runner, bear);
    let stored_before = stored_face(&runner, token).clone();
    runner.cast(conversions[1]).target_object(token).resolve();
    settle(&mut runner);
    assert_not_turned_face_down(&runner, token, MATRIARCH, &stored_before);
    assert_eq!(shown(&runner, token), face(MATRIARCH, 2, 1, false));
    assert!(
        !runner.state().objects[&token]
            .card_types
            .core_types
            .contains(&CoreType::Artifact),
        "CR 712.16: the token is not a Cyberman artifact"
    );
    assert_eq!(stored(&runner, token), face(AUNTIE, 2, 1, false));
    assert!(
        stored_sacrifices_itself(&runner, token),
        "CR 712.16: the stored Auntie still has the sacrifice exception"
    );

    // That turn's end step: the Matriarch is sacrificed.
    drive_to_end(&mut runner);
    top_of_stack_from(&runner, token);
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().battlefield.contains(&token),
        "CR 707.9a: the Matriarch face has the sacrifice trigger too"
    );
}

/// CR 733: the journal replay of Grub's self-copy installs the same faces as the
/// birth did, and those are the faces CR 707.8a derives.
#[test]
fn a_replayed_double_faced_copy_has_the_same_faces() {
    let (mut runner, grub, bear, _, _) = auntie_about_to_blight();
    decide(&mut runner, true);
    let mut replay = runner.state().clone();
    let journal_start = replay.resolved_rules_journal.entries().len();
    runner
        .act(GameAction::SelectCards { cards: vec![grub] })
        .expect("blighting Grub must succeed");
    assert_settled(&runner);
    assert_eq!(minus_counters(&runner, bear), 0);
    let token = tokens(&runner)[0];
    let command = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .find_map(|command| match command {
            ResolvedRulesCommand::TokenCreation(command) if command.object.object_id == token => {
                Some(command)
            }
            _ => None,
        })
        .expect("reach-guard: the token birth is journaled");

    engine::game::effects::token::apply_resolved_token_creation(&mut replay, &command)
        .expect("the replay applies");

    let original = &runner.state().objects[&token];
    let replayed = &replay.objects[&token];
    assert_eq!(replayed.back_face, original.back_face);
    assert_eq!(replayed.transformed, original.transformed);
    assert!(original.transformed);
    assert_eq!(stored(&runner, token), face(MATRIARCH, 2, 1, false));
}

/// A saved and reloaded game keeps the copy's faces and the face up.
#[test]
fn a_reloaded_double_faced_copy_keeps_its_faces() {
    let (mut runner, grub, bear, _, _) = auntie_about_to_blight();
    blight(&mut runner, &[grub, bear], grub);
    let token = tokens(&runner)[0];
    let json = serde_json::to_string(runner.state()).expect("the game serializes");
    assert!(
        json.contains("double_faced"),
        "reach-guard: the journaled copy spec carries the faces"
    );
    let mut loaded: GameState = serde_json::from_str(&json).expect("the game deserializes");
    engine::game::printed_cards::rehydrate_game_from_card_db(
        &mut loaded,
        load_db().expect("card db"),
    );

    let obj = &loaded.objects[&token];
    assert!(obj.transformed, "the token is still back face up");
    let stored = obj.back_face.as_ref().expect("the Matriarch is stored");
    assert_eq!(
        (stored.name.as_str(), stored.power, stored.toughness),
        (MATRIARCH, Some(2), Some(1))
    );
}

/// CR 707.8a + CR 707.9a: Kessig Prowler ("{4}{G}: Transform this creature.")
/// is blighted and dies; its copy enters front face up with Sinuous Predator
/// stored, transforms by its own ability, and keeps the sacrifice exception.
#[test]
fn a_front_up_copy_transforms_by_its_own_ability() {
    let (mut runner, grub, prowler) = grub_board(Phase::BeginCombat, |scenario, db| {
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        assert_oracle(db, PREDATOR, PREDATOR_ORACLE);
        for _ in 0..5 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db)
    });
    flip(&mut runner, grub);
    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, prowler], prowler);
    assert_settled(&runner);
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].zone, Zone::Graveyard);
    let token = tokens(&runner)[0];
    assert_eq!(shown(&runner, token), face(PROWLER, 2, 1, false));
    assert!(!runner.state().objects[&token].transformed);
    assert_eq!(stored(&runner, token), face(PREDATOR, 4, 4, false));

    activate(&mut runner, token);
    top_of_stack_from(&runner, token);
    both_pass(&mut runner);
    settle(&mut runner);
    assert_eq!(shown(&runner, token), face(PREDATOR, 4, 4, false));
    assert!(runner.state().objects[&token].transformed);
    assert!(
        shown_sacrifices_itself(&runner, token),
        "CR 707.9a: the Sinuous Predator face has the exception"
    );
    assert!(
        stored_sacrifices_itself(&runner, token),
        "CR 707.9a: the stored Kessig Prowler keeps the exception"
    );
    assert!(
        is_attacking(&runner, token),
        "CR 712.18: transforming keeps it attacking"
    );

    pass_into(&mut runner, Phase::End);
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().battlefield.contains(&token),
        "CR 707.9a: the Sinuous Predator face is sacrificed"
    );
}

/// CR 707.2 + CR 613.1: the copy's other face is read from the donor's stored
/// face, so Goblin Chieftain's bonus on the flipped Grub is not in it. Grub
/// survives the blight as a 2/1, and the legend rule (CR 704.5j) keeps the
/// token.
#[test]
fn a_double_faced_copy_stores_the_donors_copiable_face() {
    let (mut runner, grub, chieftain) = grub_board(Phase::BeginCombat, |scenario, db| {
        assert_oracle(db, CHIEFTAIN, CHIEFTAIN_ORACLE);
        scenario.add_real_card(P0, CHIEFTAIN, Zone::Battlefield, db)
    });
    settle(&mut runner);
    flip(&mut runner, grub);
    settle(&mut runner);
    assert_eq!(shown(&runner, grub), face(AUNTIE, 3, 2, true));
    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, chieftain], grub);
    let token = tokens(&runner)
        .into_iter()
        .find(|id| *id != grub)
        .expect("the token exists");
    let WaitingFor::ChooseLegend { candidates, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "CR 704.5j: two Grubs ask for the legend rule; got {:?}",
            runner.state().waiting_for
        );
    };
    let mut candidates = candidates;
    candidates.sort();
    let mut expected = vec![grub, token];
    expected.sort();
    assert_eq!(candidates, expected);
    runner
        .act(GameAction::ChooseLegend { keep: token })
        .expect("keeping the token must succeed");
    settle(&mut runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Graveyard);
    assert_eq!(stored(&runner, token), face(MATRIARCH, 2, 1, false));

    turn(&mut runner, token, MATRIARCH);
    let obj = &runner.state().objects[&token];
    assert_eq!((obj.base_power, obj.base_toughness), (Some(2), Some(1)));
    assert_eq!(
        shown(&runner, token),
        face(MATRIARCH, 3, 2, true),
        "CR 613.1: the Chieftain applies once, over the printed 2/1"
    );
    assert_eq!(stored(&runner, token), face(AUNTIE, 2, 1, false));
}

/// CR 707.8a + CR 707.9b: Feldon of the Third Path copies Grub's card in the
/// graveyard, which left the battlefield transformed under Goblin Chieftain. The
/// token is double-faced with Feldon's artifact exception on both faces; Feldon's
/// "It gains haste" is an effect on the token, not a face value.
#[test]
fn feldons_copy_of_a_double_faced_card_has_the_exception_on_both_faces() {
    let (mut runner, (grub, chieftain, feldon)) = board(Phase::Upkeep, |scenario, db| {
        assert_oracle(db, FELDON, FELDON_ORACLE);
        assert_oracle(db, CHIEFTAIN, CHIEFTAIN_ORACLE);
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::Red);
        }
        (
            scenario.add_real_card(P0, MATRIARCH, Zone::Battlefield, db),
            scenario.add_real_card(P0, CHIEFTAIN, Zone::Exile, db),
            scenario.add_real_card(P0, FELDON, Zone::Battlefield, db),
        )
    });
    runner
        .state_mut()
        .objects
        .get_mut(&feldon)
        .expect("Feldon exists")
        .summoning_sick = false;
    flip(&mut runner, grub);
    put(&mut runner, chieftain, Zone::Battlefield);
    put(&mut runner, grub, Zone::Graveyard);
    assert_eq!(shown(&runner, grub), face(MATRIARCH, 2, 1, false));
    assert_eq!(stored(&runner, grub), face(AUNTIE, 2, 1, false));

    // Grub's card is the only creature card in the graveyard, so it is the
    // target without a prompt.
    activate(&mut runner, feldon);
    top_of_stack_from(&runner, feldon);
    both_pass(&mut runner);
    let token = tokens(&runner)[0];
    select_goblin(&mut runner, grub);
    both_pass(&mut runner);
    put(&mut runner, chieftain, Zone::Exile);
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, MATRIARCH);
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
    let hidden = stored_face(&runner, token);
    assert_eq!(stored(&runner, token), face(AUNTIE, 2, 1, false));
    assert_eq!(
        hidden.card_types.core_types,
        vec![CoreType::Creature, CoreType::Artifact]
    );
    assert!(hidden.keywords.contains(&Keyword::Menace));

    runner.advance_to_phase(Phase::PreCombatMain);
    top_of_stack_from(&runner, token);
    both_pass(&mut runner);
    decide(&mut runner, true);
    settle(&mut runner);
    let obj = &runner.state().objects[&token];
    assert!(obj.transformed, "the token pays {{R}} and transforms");
    assert_eq!(shown(&runner, token), face(AUNTIE, 2, 1, true));
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));

    drive_to_end(&mut runner);
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().battlefield.contains(&token),
        "Feldon's delayed trigger sacrifices it"
    );
}

/// Nexus of Becoming exiles Kessig Prowler from hand. CR 707.8a + CR 707.9b: the
/// token is a 3/3 Golem artifact on both faces.
#[test]
fn nexus_copy_of_a_double_faced_card_transforms_as_a_golem() {
    let (mut runner, prowler) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        scenario.add_artifact_from_oracle(P0, "Nexus of Becoming", NEXUS_ORACLE);
        scenario.add_land_to_library_top(P0, "Forest");
        for _ in 0..5 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        scenario.add_real_card(P0, PROWLER, Zone::Hand, db)
    });
    runner.advance_to_phase(Phase::BeginCombat);
    both_pass(&mut runner);
    decide(&mut runner, true);
    assert_settled(&runner);
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].zone, Zone::Exile);
    let token = tokens(&runner)[0];
    assert_eq!(shown(&runner, token), face(PROWLER, 3, 3, false));
    let hidden = stored_face(&runner, token);
    assert_eq!(stored(&runner, token), face(PREDATOR, 3, 3, false));
    assert!(hidden.card_types.core_types.contains(&CoreType::Artifact));
    assert!(hidden.card_types.subtypes.iter().any(|s| s == "Golem"));

    activate(&mut runner, token);
    both_pass(&mut runner);
    settle(&mut runner);
    let obj = &runner.state().objects[&token];
    assert_eq!(shown(&runner, token), face(PREDATOR, 3, 3, false));
    assert_eq!(
        obj.card_types.core_types,
        vec![CoreType::Creature, CoreType::Artifact]
    );
    for subtype in ["Golem", "Eldrazi", "Werewolf"] {
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == subtype),
            "{subtype}"
        );
    }
}

/// Soul Separator's ruling: "The listed exceptions will apply to both the front
/// and back faces of the Spirit token."
#[test]
fn soul_separators_exceptions_apply_to_both_faces() {
    let (mut runner, (separator, prowler)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, SOUL_SEPARATOR, SOUL_SEPARATOR_ORACLE);
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        (
            scenario.add_real_card(P0, SOUL_SEPARATOR, Zone::Battlefield, db),
            scenario.add_real_card(P0, PROWLER, Zone::Graveyard, db),
        )
    });
    fund(&mut runner, ManaType::Colorless, 5);
    // The Prowler is the only creature card in the graveyard, so it is the
    // target without a prompt.
    activate(&mut runner, separator);
    top_of_stack_from(&runner, separator);
    both_pass(&mut runner);
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].zone, Zone::Exile);
    let copies: Vec<_> = tokens(&runner)
        .into_iter()
        .filter(|id| runner.state().objects[id].name == PROWLER)
        .collect();
    assert_eq!(copies.len(), 1, "one copy of the exiled card");
    let token = copies[0];
    let spirit_flier = |types: &engine::types::card_type::CardType, keywords: &[Keyword]| {
        types.subtypes.iter().any(|s| s == "Spirit") && keywords.contains(&Keyword::Flying)
    };
    let obj = &runner.state().objects[&token];
    assert_eq!(shown(&runner, token), face(PROWLER, 1, 1, false));
    assert!(spirit_flier(&obj.card_types, &obj.keywords));
    let hidden = stored_face(&runner, token);
    assert_eq!(stored(&runner, token), face(PREDATOR, 1, 1, false));
    assert!(spirit_flier(&hidden.card_types, &hidden.keywords));

    turn(&mut runner, token, PREDATOR);
    let obj = &runner.state().objects[&token];
    assert_eq!(shown(&runner, token), face(PREDATOR, 1, 1, false));
    assert!(spirit_flier(&obj.card_types, &obj.keywords));
}

/// CR 614.1a: Doubling Season's added batch carries the faces too.
#[test]
fn doubled_double_faced_copies_each_have_both_faces() {
    let (mut runner, grub, prowler) = grub_board(Phase::BeginCombat, |scenario, db| {
        assert_oracle(db, DOUBLING_SEASON, DOUBLING_SEASON_ORACLE);
        scenario.add_real_card(P0, DOUBLING_SEASON, Zone::Battlefield, db);
        scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db)
    });
    flip(&mut runner, grub);
    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, prowler], prowler);
    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&prowler].zone,
        Zone::Graveyard,
        "CR 704.5f: the doubled counters kill the Prowler"
    );
    let created = tokens(&runner);
    assert_eq!(
        created.len(),
        2,
        "CR 614.1a: Doubling Season doubles the token"
    );
    for token in created {
        assert_eq!(stored(&runner, token), face(PREDATOR, 4, 4, false));
    }
}

/// CR 707.9b: Thing in the Ice enters with four ice counters, so its copy is
/// born on the non-liminal seam, which folds the exception onto the stored face
/// itself.
#[test]
fn a_non_liminal_double_faced_copy_has_the_exception_on_both_faces() {
    let (mut runner, grub, thing) = grub_board(Phase::BeginCombat, |scenario, db| {
        assert_oracle(db, THING, THING_ORACLE);
        assert_oracle(db, HORROR, HORROR_ORACLE);
        scenario.add_real_card(P0, THING, Zone::Battlefield, db)
    });
    flip(&mut runner, grub);
    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, thing], thing);
    assert_settled(&runner);
    let token = tokens(&runner)[0];
    assert_eq!(
        runner.state().objects[&token]
            .counters
            .get(&CounterType::Generic("ice".to_string()))
            .copied(),
        Some(4),
        "reach-guard: the copy enters with its ice counters"
    );
    assert_eq!(stored_face(&runner, token).name, HORROR);
    assert!(
        stored_sacrifices_itself(&runner, token),
        "CR 707.9b: the stored Awoken Horror has the exception"
    );

    turn(&mut runner, token, HORROR);
    pass_into(&mut runner, Phase::End);
    runner.advance_until_stack_empty();
    assert!(!runner.state().battlefield.contains(&token));
    assert_eq!(runner.state().objects[&thing].zone, Zone::Battlefield);
}

/// CR 707.9a: Kiki-Jiki's "except it has haste" reaches the stored face, so the
/// copy keeps haste after it transforms.
#[test]
fn kiki_jikis_haste_reaches_the_stored_face() {
    let (mut runner, (kiki, prowler)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, KIKI, KIKI_ORACLE);
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        for _ in 0..5 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        (
            scenario.add_real_card(P0, KIKI, Zone::Battlefield, db),
            scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
        )
    });
    runner
        .state_mut()
        .objects
        .get_mut(&kiki)
        .expect("Kiki-Jiki exists")
        .summoning_sick = false;
    // The Prowler is the only nonlegendary creature P0 controls, so it is the
    // target without a prompt.
    activate(&mut runner, kiki);
    top_of_stack_from(&runner, kiki);
    both_pass(&mut runner);
    settle(&mut runner);
    let token = tokens(&runner)[0];
    assert_eq!(shown(&runner, token), face(PROWLER, 2, 1, true));
    assert_eq!(stored(&runner, token), face(PREDATOR, 4, 4, true));
    assert_eq!(runner.state().objects[&prowler].zone, Zone::Battlefield);

    activate(&mut runner, token);
    both_pass(&mut runner);
    settle(&mut runner);
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, PREDATOR);
    assert!(obj.keywords.contains(&Keyword::Haste));
    assert!(obj.base_keywords.contains(&Keyword::Haste));
}

/// CR 707.8a: a front-face-up donor whose stored face came from two transforms
/// still gives a double-faced copy.
#[test]
fn a_round_tripped_donor_gives_a_double_faced_copy() {
    let (mut runner, grub, prowler) = grub_board(Phase::BeginCombat, |scenario, db| {
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db)
    });
    flip(&mut runner, grub);
    turn(&mut runner, prowler, PREDATOR);
    turn(&mut runner, prowler, PROWLER);
    assert!(
        stored_face(&runner, prowler).is_swap_snapshot,
        "reach-guard: the stored face came from a transform"
    );
    assert_eq!(
        stored_face(&runner, prowler).layout_kind,
        Some(LayoutKind::Transform),
        "CR 701.27a: the stash keeps the face's layout"
    );
    attack_with(&mut runner, &[grub]);
    both_pass(&mut runner);
    blight(&mut runner, &[grub, prowler], prowler);
    assert_settled(&runner);
    let token = tokens(&runner)[0];
    assert!(!runner.state().objects[&token].transformed);
    assert_eq!(stored(&runner, token), face(PREDATOR, 4, 4, false));
}

/// CR 707.8a + CR 613.2: under Mirrorweave ("Each other creature becomes a copy
/// of target nonlegendary creature until end of turn.") Kessig Prowler is a
/// Grizzly Bears, so Cackling Counterpart's copy has a Grizzly Bears on both
/// faces.
#[test]
fn a_copy_of_a_copied_double_faced_permanent_has_the_copied_values_on_both_faces() {
    let (mut runner, (prowler, bears, weave, cackling)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            assert_eq!(
                db.get_face_by_name(BEARS)
                    .expect("Grizzly Bears is in the fixture")
                    .oracle_text,
                None,
                "Grizzly Bears is vanilla"
            );
            (
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, BEARS, Zone::Battlefield, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
            )
        });
    fund(&mut runner, ManaType::Blue, 7);
    runner.cast(weave).target_object(bears).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].name, BEARS);
    assert_eq!(stored_face(&runner, prowler).name, PREDATOR);

    runner.cast(cackling).target_object(prowler).resolve();
    settle(&mut runner);
    let token = tokens(&runner)[0];
    assert_eq!(shown(&runner, token), face(BEARS, 2, 2, false));
    assert_eq!(stored(&runner, token), face(BEARS, 2, 2, false));
    turn(&mut runner, token, BEARS);
}

/// The equal-values sibling: Mirrorweave targeting a second Kessig Prowler makes
/// the first a copy of it, so its copiable values equal its own printed ones.
/// CR 707.8a + CR 613.2: the copy effect still applies, so Cackling
/// Counterpart's copy stores Kessig Prowler, not Sinuous Predator.
#[test]
fn a_copy_of_a_double_faced_permanent_copying_its_twin_stores_the_copied_face() {
    let (mut runner, (first, second, weave, cackling)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            (
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
            )
        });
    fund(&mut runner, ManaType::Blue, 7);
    runner.cast(weave).target_object(second).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&first].name, PROWLER);
    assert_eq!(stored_face(&runner, first).name, PREDATOR);

    runner.cast(cackling).target_object(first).resolve();
    settle(&mut runner);
    let token = tokens(&runner)[0];
    assert_eq!(stored(&runner, token), face(PROWLER, 2, 1, false));
    turn(&mut runner, token, PROWLER);
}

/// The last sentence of CR 707.8a: tokens that enter as copies through Mystic
/// Reflection's replacement are not double-faced. They copy Sinuous Predator,
/// the face up, whose mana value as a copy of a back face is 0 (CR 202.3b +
/// CR 712.8e).
#[test]
fn tokens_entering_as_copies_of_a_transformed_permanent_are_single_faced() {
    let (mut runner, (prowler, reflection, alarm)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        assert_oracle(db, MYSTIC_REFLECTION, MYSTIC_REFLECTION_ORACLE);
        assert_oracle(db, RAISE_THE_ALARM, RAISE_THE_ALARM_ORACLE);
        (
            scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
            scenario.add_real_card(P0, MYSTIC_REFLECTION, Zone::Hand, db),
            scenario.add_real_card(P0, RAISE_THE_ALARM, Zone::Hand, db),
        )
    });
    turn(&mut runner, prowler, PREDATOR);
    fund(&mut runner, ManaType::Blue, 2);
    fund(&mut runner, ManaType::White, 2);
    runner.cast(reflection).target_object(prowler).resolve();
    runner.cast(alarm).resolve();
    settle(&mut runner);

    let created = tokens(&runner);
    assert_eq!(created.len(), 2, "Raise the Alarm makes two tokens");
    for token in created {
        let obj = &runner.state().objects[&token];
        assert_eq!(shown(&runner, token), face(PREDATOR, 4, 4, false));
        assert!(
            obj.back_face.is_none(),
            "CR 707.8a: not a double-faced token"
        );
        assert!(!obj.transformed);
        assert_eq!(obj.effective_mana_value(), 0);
    }
}

/// CR 707.8a + CR 712.8f: a copy of Valentin, Dean of the Vein cast as Lisette,
/// Dean of the Root is Lisette with Valentin stored, and the legend rule then
/// applies (CR 704.5j). CR 701.27g: each is "a double-faced permanent on the
/// battlefield with its back face up", so Mutagen Connoisseur ("This creature
/// gets +1/+0 for each transformed permanent you control.") counts the card, and
/// then the token copy that is kept.
#[test]
fn a_copy_of_a_modal_card_cast_back_face_up_is_back_face_up() {
    let (mut runner, (card, cackling, connoisseur)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
            assert_oracle(db, LISETTE, LISETTE_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            assert_oracle(db, CONNOISSEUR, CONNOISSEUR_ORACLE);
            (
                scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
                scenario.add_real_card(P0, CONNOISSEUR, Zone::Battlefield, db),
            )
        });
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        0,
        "reach guard: no transformed permanent yet"
    );
    cast_as_lisette(&mut runner, card);
    assert_eq!(shown(&runner, card), face(LISETTE, 4, 4, false));
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        1,
        "CR 701.27g + CR 712.11b: Lisette, back face up, is a transformed permanent"
    );
    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(card).commit();
    both_pass(&mut runner);
    settle(&mut runner);

    let token = tokens(&runner)[0];
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, LISETTE);
    assert!(obj.modal_back_face, "CR 712.8f: the back face is up");
    assert!(!obj.transformed);
    assert_eq!(stored(&runner, token), face(VALENTIN, 1, 1, false));
    let WaitingFor::ChooseLegend { candidates, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "CR 704.5j: two Lisettes ask for the legend rule; got {:?}",
            runner.state().waiting_for
        );
    };
    let mut candidates = candidates;
    candidates.sort();
    let mut expected = vec![card, token];
    expected.sort();
    assert_eq!(candidates, expected);
    runner
        .act(GameAction::ChooseLegend { keep: token })
        .expect("keeping the token must succeed");
    assert!(runner.state().battlefield.contains(&token));
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Graveyard,
        "CR 704.5j: the original goes to the graveyard"
    );
    assert!(
        runner.state().objects[&token].modal_back_face,
        "reach guard: the kept token is back face up"
    );
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        1,
        "CR 701.27g + CR 707.8a: the kept token copy, back face up, is a transformed permanent"
    );
}

/// CR 707.9d + Soul Separator's ruling: "If the copied creature card has an
/// ability that defines its power and toughness, such as that of Sage of
/// Ancient Lore, the Spirit token will still have base power and toughness
/// 1/1." The exceptions apply to both faces, so Werewolf of Ancient Hunger's
/// power/toughness ability is not copied either.
#[test]
fn soul_separators_copy_of_sage_of_ancient_lore_stays_1_1_on_both_faces() {
    let (mut runner, (separator, sage)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, SOUL_SEPARATOR, SOUL_SEPARATOR_ORACLE);
        assert_oracle(db, SAGE, SAGE_ORACLE);
        assert_oracle(
            db,
            WEREWOLF_OF_ANCIENT_HUNGER,
            WEREWOLF_OF_ANCIENT_HUNGER_ORACLE,
        );
        scenario.add_real_card(P1, BEARS, Zone::Hand, db);
        scenario.add_real_card(P1, BEARS, Zone::Hand, db);
        (
            scenario.add_real_card(P0, SOUL_SEPARATOR, Zone::Battlefield, db),
            scenario.add_real_card(P0, SAGE, Zone::Graveyard, db),
        )
    });
    let has_cda = |obj: &engine::game::game_object::GameObject| {
        obj.static_definitions
            .iter_all()
            .any(|definition| definition.characteristic_defining)
    };
    let face_has_cda = |face: &BackFaceData| {
        face.static_definitions
            .iter_all()
            .any(|definition| definition.characteristic_defining)
    };
    assert!(
        has_cda(&runner.state().objects[&sage]) && face_has_cda(stored_face(&runner, sage)),
        "reach-guard: both printed faces define their power and toughness"
    );
    fund(&mut runner, ManaType::Colorless, 5);
    // Sage is the only creature card in the graveyard, so it is the target
    // without a prompt.
    activate(&mut runner, separator);
    top_of_stack_from(&runner, separator);
    both_pass(&mut runner);
    let token = tokens(&runner)
        .into_iter()
        .find(|id| runner.state().objects[id].name == SAGE)
        .expect("the copy of Sage exists");
    // The copy's own "When this creature enters, draw a card."
    top_of_stack_from(&runner, token);
    both_pass(&mut runner);
    assert_settled(&runner);
    settle(&mut runner);
    let in_hands: usize = runner.state().players.iter().map(|p| p.hand.len()).sum();
    assert_eq!(in_hands, 3, "reach-guard: a CDA would read 3 here");
    assert_eq!(shown(&runner, token), face(SAGE, 1, 1, false));
    assert!(!has_cda(&runner.state().objects[&token]));
    assert!(!face_has_cda(stored_face(&runner, token)));

    turn(&mut runner, token, WEREWOLF_OF_ANCIENT_HUNGER);
    assert_eq!(
        shown(&runner, token),
        face(WEREWOLF_OF_ANCIENT_HUNGER, 1, 1, false)
    );
    assert!(!has_cda(&runner.state().objects[&token]));
}

/// CR 712.16: "If a spell or ability tries to turn a double-faced permanent face
/// down, nothing happens." Kessig Prowler transforms by its own ability ("{4}{G}:
/// Transform this creature.") into Sinuous Predator, and Waxing Moon ("Transform
/// up to one target Werewolf you control. …") turns the Werewolf back, so it is
/// front face up with `transformed` false. CR 701.27a: each transform keeps the
/// stored face's layout, so Cyber Conversion still finds a double-faced
/// permanent; Grizzly Bears beside it is the positive reach.
#[test]
fn a_double_faced_card_transformed_twice_cannot_be_turned_face_down() {
    let (mut runner, (prowler, bears, moon, conversions)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, PREDATOR, PREDATOR_ORACLE);
            assert_oracle(db, WAXING_MOON, WAXING_MOON_ORACLE);
            assert_oracle(db, CYBER_CONVERSION, CYBER_CONVERSION_ORACLE);
            (
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, BEARS, Zone::Battlefield, db),
                scenario.add_real_card(P0, WAXING_MOON, Zone::Hand, db),
                [
                    scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
                    scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
                ],
            )
        });

    fund(&mut runner, ManaType::Green, 5);
    activate(&mut runner, prowler);
    top_of_stack_from(&runner, prowler);
    both_pass(&mut runner);
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].name, PREDATOR);
    assert!(stored_face(&runner, prowler).is_swap_snapshot);
    assert_eq!(
        stored_face(&runner, prowler).layout_kind,
        Some(LayoutKind::Transform),
        "CR 701.27a: the stash keeps the face's layout"
    );

    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moon).target_object(prowler).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].name, PROWLER);
    assert!(
        !runner.state().objects[&prowler].transformed,
        "reach-guard: the round trip leaves the front face up"
    );
    assert!(stored_face(&runner, prowler).is_swap_snapshot);
    assert_eq!(
        stored_face(&runner, prowler).layout_kind,
        Some(LayoutKind::Transform),
        "CR 701.27a: the stash keeps the face's layout"
    );

    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(conversions[0]).target_object(bears).resolve();
    settle(&mut runner);
    assert_cyber_converted(&runner, bears);

    let stored_before = stored_face(&runner, prowler).clone();
    runner.cast(conversions[1]).target_object(prowler).resolve();
    settle(&mut runner);
    assert_not_turned_face_down(&runner, prowler, PROWLER, &stored_before);
    assert!(
        !runner.state().objects[&prowler]
            .card_types
            .core_types
            .contains(&CoreType::Artifact),
        "CR 712.16: the Prowler is not a Cyberman artifact"
    );
    assert_eq!(stored_face(&runner, prowler).name, PREDATOR);
    assert_settled(&runner);
}

/// CR 712.11b: Valentin, Dean of the Vein cast as Lisette, Dean of the Root is
/// transformed by Moonmist ("Transform all Humans. …"; Lisette is a Human Druid)
/// and then returns to its owner's hand. CR 701.27a: the transform kept the
/// stored face's modal layout, so the card in hand is still a modal
/// double-faced card and can be cast as Lisette again.
#[test]
fn a_modal_card_moonmisted_and_returned_to_hand_can_be_cast_as_lisette() {
    let (mut runner, (card, moonmist)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
        assert_oracle(db, LISETTE, LISETTE_ORACLE);
        assert_oracle(db, MOONMIST, MOONMIST_ORACLE);
        (
            scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
            scenario.add_real_card(P0, MOONMIST, Zone::Hand, db),
        )
    });
    cast_as_lisette(&mut runner, card);
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moonmist).resolve();
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&card].name,
        VALENTIN,
        "reach-guard: Moonmist transformed Lisette"
    );
    assert_eq!(stored_face(&runner, card).name, LISETTE);
    assert_eq!(
        stored_face(&runner, card).layout_kind,
        Some(LayoutKind::Modal),
        "CR 701.27a: the stash keeps the face's layout"
    );

    put(&mut runner, card, Zone::Hand);
    cast_as_lisette(&mut runner, card);
}

/// CR 715.3b + CR 715.2c + CR 712.1: while Bonecrusher Giant is on the stack as
/// Stomp ("Damage can't be prevented this turn. Stomp deals 2 damage to any
/// target."), the spell has only its Adventure characteristics and the card is
/// one single-faced card, so the stored Bonecrusher Giant is not an other face
/// of a double-faced card. The stack exit re-stamps the Adventure layout.
#[test]
fn stomp_on_the_stack_is_not_double_faced() {
    let (mut runner, giant) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, BONECRUSHER, BONECRUSHER_ORACLE);
        assert_oracle(db, STOMP, STOMP_ORACLE);
        scenario.add_real_card(P0, BONECRUSHER, Zone::Hand, db)
    });
    fund(&mut runner, ManaType::Red, 2);
    runner
        .cast(giant)
        .adventure_face(false)
        .target_player(P1)
        .commit();

    let obj = &runner.state().objects[&giant];
    assert_eq!(obj.name, STOMP);
    assert_eq!(obj.zone, Zone::Stack);
    assert_eq!(stored_face(&runner, giant).name, BONECRUSHER);
    assert_eq!(
        stored_face(&runner, giant).layout_kind,
        Some(LayoutKind::Single),
        "CR 715.3b: the stored normal face is marked single-faced while Stomp is on the stack"
    );
    assert!(
        !is_double_faced_permanent(obj),
        "CR 712.1 + CR 715.2c: an Adventure spell is one single-faced card"
    );

    both_pass(&mut runner);
    assert_eq!(runner.state().players[1].life, 18, "Stomp resolved");
    let obj = &runner.state().objects[&giant];
    assert_eq!(obj.zone, Zone::Exile);
    assert_eq!(obj.name, BONECRUSHER);
    assert_eq!(stored_face(&runner, giant).name, STOMP);
    assert_eq!(
        stored_face(&runner, giant).layout_kind,
        Some(LayoutKind::Adventure),
        "the stack exit re-stamps the Adventure layout"
    );
    assert!(!is_double_faced_permanent(obj));
    assert_settled(&runner);
}

/// CR 712.8d + CR 707.8a: Valentin, Dean of the Vein cast as Lisette, Dean of
/// the Root and then transformed by Moonmist shows its front face with both
/// orientation flags set. Cackling Counterpart's copy is a double-faced token
/// front face up, with Lisette stored, and the legend rule applies (CR 704.5j).
/// CR 701.27g: "A permanent with its front face up is never considered a
/// transformed permanent, even if it had its back face up previously", so Mutagen
/// Connoisseur counts Lisette and then stops counting Valentin. CR 712.8a: off the
/// battlefield the card is Valentin ({B}), so its exit mana value is 1 throughout.
#[test]
fn a_copy_of_lisette_moonmisted_into_valentin_is_double_faced_front_up() {
    let (mut runner, (card, moonmist, cackling, connoisseur)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
            assert_oracle(db, LISETTE, LISETTE_ORACLE);
            assert_oracle(db, MOONMIST, MOONMIST_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            assert_oracle(db, CONNOISSEUR, CONNOISSEUR_ORACLE);
            (
                scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
                scenario.add_real_card(P0, MOONMIST, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
                scenario.add_real_card(P0, CONNOISSEUR, Zone::Battlefield, db),
            )
        });
    cast_as_lisette(&mut runner, card);
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        1,
        "reach guard: Lisette, back face up, is a transformed permanent"
    );
    assert_eq!(
        runner.state().objects[&card].mana_value_on_battlefield_exit(),
        1,
        "CR 712.8a: Lisette leaves as Valentin ({{B}})"
    );
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moonmist).resolve();
    settle(&mut runner);
    let obj = &runner.state().objects[&card];
    assert_eq!(obj.name, VALENTIN);
    assert!(
        obj.transformed && obj.modal_back_face,
        "reach-guard: the front face is up with both flags set"
    );
    assert_eq!(
        obj.base_mana_cost.mana_value(),
        1,
        "reach guard: the front face's own mana value is Valentin's"
    );
    assert_eq!(
        obj.mana_value_on_battlefield_exit(),
        1,
        "CR 712.8a + CR 712.8d: front face up, Valentin leaves as Valentin"
    );
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        0,
        "CR 701.27g: Valentin, front face up, is not a transformed permanent"
    );

    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(card).commit();
    both_pass(&mut runner);
    settle(&mut runner);
    let WaitingFor::ChooseLegend { candidates, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "CR 704.5j: two Valentins ask for the legend rule; got {:?}",
            runner.state().waiting_for
        );
    };
    let token = tokens(&runner)[0];
    let mut candidates = candidates;
    candidates.sort();
    let mut expected = vec![card, token];
    expected.sort();
    assert_eq!(candidates, expected);

    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, VALENTIN);
    assert!(
        !obj.transformed && !obj.modal_back_face,
        "CR 712.8d: the copy is front face up"
    );
    assert_eq!(stored_face(&runner, token).name, LISETTE);
    assert_eq!(
        stored_face(&runner, token).layout_kind,
        Some(LayoutKind::Modal)
    );
    assert!(
        is_double_faced_permanent(obj),
        "CR 707.8a: a copy of a double-faced permanent is a double-faced token"
    );

    runner
        .act(GameAction::ChooseLegend { keep: card })
        .expect("keeping the original must succeed");
    assert!(!runner.state().battlefield.contains(&token));
    assert_eq!(runner.state().objects[&card].zone, Zone::Battlefield);
}

/// CR 707.8a + CR 722.2b: Mirrorweave ("Each other creature becomes a copy of
/// target nonlegendary creature until end of turn.") makes Kessig Prowler a copy
/// of Encouraging Aviator ("Flying / Whenever this creature attacks, it becomes
/// prepared. …"), and Cackling Counterpart copies the Prowler. The token is a
/// double-faced token with Encouraging Aviator on both faces (CR 707.8a "as
/// modified by any other copy effects"), and it keeps Aviator's prepare spell,
/// Jump, beside its other face: "The existence and values of these alternative
/// characteristics are part of the object's copiable values." It transforms by
/// itself, still an Aviator, and becomes prepared when it attacks (CR 722.3a),
/// which exiles a prepared copy of Jump (CR 722.3c).
#[test]
fn a_copy_of_a_double_faced_card_copying_encouraging_aviator_keeps_both_faces_and_jump() {
    let (mut runner, (prowler, aviator, weave, cackling)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, AVIATOR, AVIATOR_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            (
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, AVIATOR, Zone::Battlefield, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
            )
        });
    fund(&mut runner, ManaType::Blue, 7);
    runner.cast(weave).target_object(aviator).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].name, AVIATOR);
    assert_eq!(stored_face(&runner, prowler).name, PREDATOR);

    runner.cast(cackling).target_object(prowler).resolve();
    settle(&mut runner);
    let token = tokens(&runner)[0];
    // (a) both faces are the copied Aviator.
    assert_eq!(runner.state().objects[&token].name, AVIATOR);
    assert_eq!(stored_face(&runner, token).name, AVIATOR);
    assert_eq!(
        stored_face(&runner, token).layout_kind,
        Some(LayoutKind::Transform)
    );
    assert!(
        is_double_faced_permanent(&runner.state().objects[&token]),
        "CR 707.8a: a copy of a double-faced permanent is a double-faced token"
    );

    // (b) setup: the token's own transform still shows an Aviator.
    turn(&mut runner, token, AVIATOR);
    assert_eq!(stored_face(&runner, token).name, AVIATOR);

    // (c) CR 722.3a: its prepare spell survived the copy and the transform.
    runner
        .state_mut()
        .objects
        .get_mut(&token)
        .expect("the token exists")
        .summoning_sick = false;
    attack_with(&mut runner, &[token]);
    both_pass(&mut runner);
    assert!(
        runner.state().objects[&token].prepared.is_some(),
        "CR 722.3a: the attacking token becomes prepared"
    );
    // (d) CR 722.3c: the prepared copy of Jump waits in exile.
    let state = runner.state();
    assert!(
        state.exile.iter().any(|id| {
            state.objects[id].prepared_copy_source == Some(token)
                && state.objects[id].name == "Jump"
        }),
        "CR 722.3c: a prepared copy of the token's prepare spell exists"
    );
}

/// CR 111.10i + CR 712.16: "An Incubator token is a double-faced token", so it is
/// a double-faced permanent with its front face up too, where its stored
/// Phyrexian face has no layout and `transformed` is false. Converter Beast
/// ("When this creature enters, incubate 5. …") makes one; Karn's Touch ("Target
/// noncreature artifact becomes an artifact creature with power and toughness
/// each equal to its mana value until end of turn. …") makes it a 5/5 creature;
/// then Cyber Conversion does nothing to it. Converter Beast itself is the
/// positive reach: Cyber Conversion turns it face down.
#[test]
fn an_animated_incubator_token_cannot_be_turned_face_down() {
    let (mut runner, (beast, touch, conversions)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, CONVERTER_BEAST, CONVERTER_BEAST_ORACLE);
        assert_oracle(db, KARNS_TOUCH, KARNS_TOUCH_ORACLE);
        assert_oracle(db, CYBER_CONVERSION, CYBER_CONVERSION_ORACLE);
        (
            scenario.add_real_card(P0, CONVERTER_BEAST, Zone::Hand, db),
            scenario.add_real_card(P0, KARNS_TOUCH, Zone::Hand, db),
            [
                scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
                scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
            ],
        )
    });
    fund(&mut runner, ManaType::Green, 4);
    runner.cast(beast).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&beast].zone, Zone::Battlefield);
    let incubator = tokens(&runner)[0];
    let name = runner.state().objects[&incubator].name.clone();
    assert!(!runner.state().objects[&incubator].transformed);
    assert_eq!(
        (
            stored_face(&runner, incubator).name.as_str(),
            stored_face(&runner, incubator).layout_kind
        ),
        ("Phyrexian Token", None),
        "reach-guard: the front-face-up Incubator stores its Phyrexian face with no layout"
    );

    fund(&mut runner, ManaType::Blue, 6);
    runner.cast(touch).target_object(incubator).resolve();
    settle(&mut runner);
    assert!(
        runner.state().objects[&incubator]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach-guard: Karn's Touch made the Incubator a creature Cyber Conversion can target"
    );

    runner.cast(conversions[0]).target_object(beast).resolve();
    settle(&mut runner);
    assert_cyber_converted(&runner, beast);

    let stored_before = stored_face(&runner, incubator).clone();
    runner
        .cast(conversions[1])
        .target_object(incubator)
        .resolve();
    settle(&mut runner);
    assert_not_turned_face_down(&runner, incubator, &name, &stored_before);
    assert_settled(&runner);
}

/// Door-stamped (CR 709.5) definitions in `id`'s base sets: left-door triggers,
/// right-door triggers, left-door statics, right-door statics.
fn door_text(runner: &GameRunner, id: ObjectId) -> (usize, usize, usize, usize) {
    let obj = &runner.state().objects[&id];
    let triggers = |door| {
        obj.base_trigger_definitions
            .iter()
            .filter(|definition| definition.room_door == Some(door))
            .count()
    };
    let statics = |door| {
        obj.base_static_definitions
            .iter()
            .filter(|definition| definition.room_door == Some(door))
            .count()
    };
    (
        triggers(RoomDoor::Left),
        triggers(RoomDoor::Right),
        statics(RoomDoor::Left),
        statics(RoomDoor::Right),
    )
}

/// CR 709.5e: P0 pays `door`'s unlock cost as a special action.
fn unlock(runner: &mut GameRunner, room: ObjectId, door: RoomDoor) {
    runner
        .act(GameAction::UnlockRoomDoor {
            object_id: room,
            door,
        })
        .expect("unlocking the door must be legal");
    settle(runner);
}

/// Casts Mirror Room // Fractured Realm as Mirror Room, then Opalescence and Gideon, Ally of
/// Zendikar, and resolves Gideon's −4, so the Room is a creature and P0 has the emblem that keeps
/// a fully locked copy of it alive (see `mirror_room_copied_through`).
fn animate_mirror_room(
    runner: &mut GameRunner,
    room: ObjectId,
    opalescence: ObjectId,
    gideon: ObjectId,
) {
    fund(runner, ManaType::Blue, 3);
    runner.cast(room).modal_back_face(false).resolve();
    settle(runner);
    assert_settled(runner);
    assert_eq!(runner.state().objects[&room].name, MIRROR_ROOM);

    fund(runner, ManaType::White, 4);
    runner.cast(opalescence).resolve();
    fund(runner, ManaType::White, 4);
    runner.cast(gideon).resolve();
    settle(runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: gideon,
            ability_index: 2,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Gideon's −4 must be legal");
    both_pass(runner);
    settle(runner);
    assert_settled(runner);
}

/// The printed Room board for a double-faced copy token whose copied values are
/// a Room's. Mirror Room // Fractured Realm is cast as Mirror Room, so its left
/// half is unlocked (CR 709.5d); its "When you unlock this door" trigger has no
/// creature to target yet. Opalescence ("Each other non-Aura enchantment is a
/// creature in addition to its other types and has base power and base
/// toughness each equal to its mana value.") animates it, and the emblem from
/// Gideon, Ally of Zendikar's "−4: You get an emblem with "Creatures you control
/// get +1/+1."" keeps a fully locked copy of it alive: a locked half has no mana
/// cost (CR 709.5), so that copy is 0/0 before the emblem (CR 704.5f).
/// `enter` puts the double-faced card `creature` onto the battlefield, then
/// Mirrorweave ("Each other creature becomes a copy of target nonlegendary
/// creature until end of turn.") makes it a copy of the Room and Cackling
/// Counterpart ("Create a token that's a copy of target creature you control.")
/// copies it. Returns the Room, the creature and the token.
fn mirror_room_copied_through(
    creature: &'static str,
    enter: fn(&mut GameRunner, ObjectId),
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (mut runner, (room, opalescence, gideon, creature, weave, cackling)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, MIRROR_ROOM, MIRROR_ROOM_ORACLE);
            assert_oracle(db, FRACTURED_REALM, FRACTURED_REALM_ORACLE);
            assert_oracle(db, OPALESCENCE, OPALESCENCE_ORACLE);
            assert_oracle(db, GIDEON, GIDEON_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            (
                scenario.add_real_card(P0, MIRROR_ROOM, Zone::Hand, db),
                scenario.add_real_card(P0, OPALESCENCE, Zone::Hand, db),
                scenario.add_real_card(P0, GIDEON, Zone::Hand, db),
                scenario.add_real_card(P0, creature, Zone::Hand, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
            )
        });
    animate_mirror_room(&mut runner, room, opalescence, gideon);

    enter(&mut runner, creature);
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(room).resolve();
    settle(&mut runner);
    let copied = &runner.state().objects[&creature];
    assert_eq!(
        copied.zone,
        Zone::Battlefield,
        "reach-guard: the emblem keeps the locked copy of the Room alive"
    );
    assert!(
        copied.card_types.subtypes.iter().any(|s| s == "Room"),
        "reach-guard: Mirrorweave made the creature a copy of the Room"
    );

    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(creature).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    let made = tokens(&runner);
    assert_eq!(made.len(), 1, "Cackling Counterpart makes one token");
    let token = made[0];
    assert!(
        is_double_faced_permanent(&runner.state().objects[&token]),
        "CR 707.8a: a copy of a double-faced permanent is a double-faced token"
    );
    assert_eq!(
        runner.state().objects[&token]
            .double_faced_token_alternative
            .as_ref()
            .map(|half| half.name.as_str()),
        Some(FRACTURED_REALM),
        "CR 709.5b: the token keeps the copied Room's other half beside its other face"
    );
    (runner, room, creature, token)
}

/// CR 709.5b + CR 707.8a: Cackling Counterpart's copy of a Kessig Prowler that
/// Mirrorweave made a copy of Mirror Room // Fractured Realm is a double-faced
/// token, front face up, with Mirror Room's "When you unlock this door, create
/// a token that's a copy of target creature you control, except it's a
/// Reflection in addition to its other creature types." once, under the left
/// door, and Fractured Realm's "If a triggered ability of a permanent you
/// control triggers, that ability triggers an additional time." once, under the
/// right door. Unlocking Mirror Room fires its trigger (CR 709.5h); unlocking
/// Fractured Realm afterwards fires nothing.
#[test]
fn a_double_faced_copy_of_mirror_room_has_each_doors_text_once() {
    let (mut runner, room, prowler, token) =
        mirror_room_copied_through(PROWLER, |runner, prowler| {
            fund(runner, ManaType::Green, 1);
            runner.cast(prowler).resolve();
            settle(runner);
            assert_eq!(runner.state().objects[&prowler].zone, Zone::Battlefield);
        });
    assert_eq!(stored_face(&runner, prowler).name, PREDATOR);
    let obj = &runner.state().objects[&token];
    assert!(!obj.transformed && !obj.modal_back_face, "CR 712.8d");
    assert_eq!(
        door_text(&runner, room),
        (1, 0, 0, 1),
        "reach-guard: the Room has Mirror Room's trigger and Fractured Realm's static"
    );
    assert_eq!(
        door_text(&runner, token),
        door_text(&runner, room),
        "CR 709.5 + CR 707.2: the token has each door's copied text exactly once"
    );

    // Positive reach: Mirror Room's door fires its own trigger.
    fund(&mut runner, ManaType::Blue, 3);
    unlock(&mut runner, token, RoomDoor::Left);
    assert_eq!(runner.state().objects[&token].name, MIRROR_ROOM);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "CR 709.5h: unlocking Mirror Room triggers it; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(room)],
        })
        .expect("targeting the Room must succeed");
    both_pass(&mut runner);
    settle(&mut runner);
    assert_settled(&runner);
    assert_eq!(tokens(&runner).len(), 2, "Mirror Room made its Reflection");

    // CR 709.5h: Fractured Realm's door has no trigger to fire.
    fund(&mut runner, ManaType::Blue, 7);
    unlock(&mut runner, token, RoomDoor::Right);
    assert_eq!(
        runner.state().objects[&token].name,
        format!("{MIRROR_ROOM} // {FRACTURED_REALM}")
    );
    assert_settled(&runner);
    assert_eq!(tokens(&runner).len(), 2, "no second Reflection");
}

/// CR 707.8a: "If the token is a copy of a double-faced permanent with its back
/// face up, the token enters the battlefield with its back face up." Valentin,
/// Dean of the Vein cast as Lisette, Dean of the Root is made a copy of Mirror
/// Room // Fractured Realm by Mirrorweave, and Cackling Counterpart copies it.
/// The token is back face up, and its halves are still Mirror Room {2}{U} and
/// then Fractured Realm {5}{U}{U}, in printed order (CR 709.5b), with each
/// door's text once. Unlocking Fractured Realm costs exactly {5}{U}{U}
/// (CR 709.5e).
#[test]
fn a_back_face_up_copy_of_mirror_room_keeps_its_halves_in_printed_order() {
    let (mut runner, room, lisette, token) = mirror_room_copied_through(VALENTIN, cast_as_lisette);
    assert!(runner.state().objects[&lisette].modal_back_face);
    assert_eq!(stored_face(&runner, lisette).name, VALENTIN);
    assert!(
        runner.state().objects[&token].modal_back_face,
        "CR 707.8a + CR 712.8f: the token enters back face up"
    );

    let printed = RoomCopiableHalves {
        left: RoomHalfIdentity {
            name: MIRROR_ROOM.to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
        },
        right: Some(RoomHalfIdentity {
            name: FRACTURED_REALM.to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 5,
            },
        }),
    };
    let views = derive_views(runner.state(), Some(P0));
    assert_eq!(
        views.room_half_identities.get(&room),
        Some(&printed),
        "reach-guard: the Room's halves"
    );
    assert_eq!(
        views.room_half_identities.get(&token),
        Some(&printed),
        "CR 709.5b + CR 709.5e: the token's halves and door costs are in printed order"
    );
    assert_eq!(
        door_text(&runner, token),
        (1, 0, 0, 1),
        "CR 709.5: the token has each door's copied text exactly once"
    );

    fund(&mut runner, ManaType::Blue, 7);
    unlock(&mut runner, token, RoomDoor::Right);
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "CR 709.5e: Fractured Realm's door costs {{5}}{{U}}{{U}}"
    );
    assert_eq!(runner.state().objects[&token].name, FRACTURED_REALM);
    assert_settled(&runner);

    // CR 701.27a + CR 712.9: setup transform. Both faces are the copied Room, so the
    // halves, door costs and door text read the same.
    turn(&mut runner, token, FRACTURED_REALM);
    assert_eq!(
        derive_views(runner.state(), Some(P0))
            .room_half_identities
            .get(&token),
        Some(&printed),
        "CR 709.5b: a transformed token keeps its halves in printed order"
    );
    assert_eq!(door_text(&runner, token), (1, 0, 0, 1));

    // CR 712.8a: setup zone exit. In the graveyard the token has its front face up,
    // and still each door's copied text once, until it ceases to exist (CR 704.5d).
    put(&mut runner, token, Zone::Graveyard);
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.zone, Zone::Graveyard);
    assert!(!obj.transformed && !obj.modal_back_face, "CR 712.8a");
    assert_eq!(door_text(&runner, token), (1, 0, 0, 1));
}

/// Passes priority until the declare-attackers declaration, declining any
/// optional prompt on the way. `advance_to_combat` can't be used after a cast in
/// the main phase: it re-enters the main step and re-offers Grub's first-main
/// trigger.
fn pass_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("passing priority must succeed"),
            WaitingFor::OptionalEffectChoice { .. } => runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .expect("declining the optional effect must succeed"),
            other => panic!("unexpected prompt before declaring attackers: {other:?}"),
        };
    }
    panic!("never reached the declare-attackers step");
}

/// Casts Grub, Storied Matriarch from hand ({2}{B}) and resolves her and her
/// enters trigger, which has no Goblin card to return. Then, as setup, flips her
/// to Grub, Notorious Auntie, able to attack.
fn cast_grub_as_auntie(runner: &mut GameRunner, grub: ObjectId) {
    fund(runner, ManaType::Black, 3);
    runner.cast(grub).commit();
    both_pass(runner);
    assert_eq!(runner.state().objects[&grub].zone, Zone::Battlefield);
    runner.advance_until_stack_empty();
    settle(runner);
    assert_settled(runner);
    flip(runner, grub);
    runner
        .state_mut()
        .objects
        .get_mut(&grub)
        .expect("Grub exists")
        .summoning_sick = false;
}

/// Grub attacks alone; her trigger asks whether to blight.
fn auntie_attacks(runner: &mut GameRunner, grub: ObjectId) {
    pass_to_declare_attackers(runner);
    runner
        .declare_attackers(&[(grub, AttackTarget::Player(P1))])
        .expect("the attack must be legal");
    both_pass(runner);
}

/// At the end step's first priority `token` is still on the battlefield and its
/// copy's sacrifice trigger is on the stack, so a token gone after it resolves
/// was sacrificed by that trigger, not by state-based actions.
fn assert_sacrifice_trigger_waiting(runner: &GameRunner, token: ObjectId) {
    assert_eq!(runner.state().phase, Phase::End);
    assert!(
        runner.state().battlefield.contains(&token),
        "reach-guard: the token survives to the end step"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == token),
        "CR 707.9a + CR 709.5: the token's end-step trigger must be on the stack; stack={:?}",
        runner.state().stack
    );
}

/// Mirror Room // Fractured Realm cast as Mirror Room (left half unlocked,
/// CR 709.5d); Glorious Anthem ("Creatures you control get +1/+1.") and
/// Opalescence ("Each other non-Aura enchantment is a creature in addition to
/// its other types and has base power and base toughness each equal to its mana
/// value.") make the Room and the Anthem creatures. Grub is cast, flipped, and
/// blights the Room. Returns Grub, the Room, the Anthem, the token and a
/// Cackling Counterpart in hand.
fn auntie_copies_an_animated_mirror_room(
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let (mut runner, (room, opalescence, anthem, grub, cackling)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, MIRROR_ROOM, MIRROR_ROOM_ORACLE);
            assert_oracle(db, FRACTURED_REALM, FRACTURED_REALM_ORACLE);
            assert_oracle(db, OPALESCENCE, OPALESCENCE_ORACLE);
            assert_oracle(db, ANTHEM, ANTHEM_ORACLE);
            assert_oracle(db, AUNTIE, AUNTIE_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            (
                scenario.add_real_card(P0, MIRROR_ROOM, Zone::Hand, db),
                scenario.add_real_card(P0, OPALESCENCE, Zone::Hand, db),
                scenario.add_real_card(P0, ANTHEM, Zone::Battlefield, db),
                scenario.add_real_card(P0, MATRIARCH, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
            )
        });
    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(room).modal_back_face(false).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&room].name, MIRROR_ROOM);
    fund(&mut runner, ManaType::White, 4);
    runner.cast(opalescence).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    cast_grub_as_auntie(&mut runner, grub);

    auntie_attacks(&mut runner, grub);
    // CR 701.68a: under Opalescence the Anthem is a creature P0 controls too.
    blight(&mut runner, &[grub, room, anthem], room);
    assert_settled(&runner);
    let made = tokens(&runner);
    assert_eq!(made.len(), 1, "reach-guard: Grub's copy of the Room exists");
    let token = made[0];
    assert_eq!(
        runner.state().objects[&token].room_unlocks,
        Some(RoomUnlockState::default()),
        "CR 709.5d: the token enters with neither half unlocked"
    );
    assert_eq!(
        door_text(&runner, room),
        (1, 0, 0, 1),
        "reach-guard: the Room has Mirror Room's trigger and Fractured Realm's static"
    );
    assert_eq!(
        door_text(&runner, token),
        door_text(&runner, room),
        "CR 709.5 + CR 707.2: the copied half stamps are kept"
    );
    (runner, grub, room, anthem, token, cackling)
}

/// CR 707.9a + CR 709.5 + CR 709.5d: Grub's copy of Mirror Room, animated by
/// Opalescence, enters with both doors locked. "At the beginning of the end step,
/// sacrifice this token." is an ability the copy gained, not text of either half,
/// so no door gates it: the token is sacrificed at the end step.
#[test]
fn auntie_copy_of_an_animated_mirror_room_is_sacrificed_with_both_doors_locked() {
    let (mut runner, _grub, room, _anthem, token, _cackling) =
        auntie_copies_an_animated_mirror_room();

    pass_into(&mut runner, Phase::End);
    assert_sacrifice_trigger_waiting(&runner, token);
    runner.advance_until_stack_empty();

    assert_settled(&runner);
    assert!(
        tokens(&runner).is_empty(),
        "CR 707.9a + CR 709.5: the gained trigger functions with both doors locked"
    );
    assert_eq!(
        runner.state().objects[&room].zone,
        Zone::Battlefield,
        "only the token is sacrificed"
    );
}

/// CR 707.9a: the gained trigger is part of the token's copiable values, so
/// Cackling Counterpart's copy of Grub's Room token has it outside both doors
/// too, and both tokens are sacrificed at the end step.
#[test]
fn a_copy_of_aunties_room_token_keeps_the_grant_outside_both_doors() {
    let (mut runner, _grub, _room, _anthem, token, cackling) =
        auntie_copies_an_animated_mirror_room();

    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(token).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    let second = tokens(&runner)
        .into_iter()
        .find(|id| *id != token)
        .expect("reach-guard: Cackling Counterpart copies the token");
    assert_eq!(
        runner.state().objects[&second].room_unlocks,
        Some(RoomUnlockState::default()),
        "CR 709.5d: the second token enters with neither half unlocked"
    );
    assert_eq!(
        door_text(&runner, second),
        (1, 0, 0, 1),
        "CR 709.5 + CR 707.2: each door's copied text once"
    );

    pass_into(&mut runner, Phase::End);
    assert_sacrifice_trigger_waiting(&runner, token);
    assert_sacrifice_trigger_waiting(&runner, second);
    for id in [token, second] {
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| entry.source_id == id)
                .count(),
            1,
            "one sacrifice trigger from each token"
        );
    }
    runner.advance_until_stack_empty();

    assert_settled(&runner);
    assert!(
        tokens(&runner).is_empty(),
        "CR 707.9a: both tokens have the gained trigger outside both doors"
    );
}

/// The maintainer's board. CR 707.8a + CR 707.9a + CR 709.5: Mirrorweave ("Each
/// other creature becomes a copy of target nonlegendary creature until end of
/// turn.") makes Kessig Prowler a copy of Mirror Room, animated by Opalescence
/// and kept alive by Gideon, Ally of Zendikar's emblem ("Creatures you control
/// get +1/+1."). Grub is cast after Mirrorweave resolves (CR 611.2c) and blights
/// the Prowler. The double-faced token has the sacrifice trigger on both faces,
/// outside both doors, and is sacrificed at the end step.
#[test]
fn auntie_copy_of_a_prowler_mirrorweaved_into_mirror_room_is_sacrificed() {
    let (mut runner, (room, opalescence, gideon, prowler, weave, grub)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, MIRROR_ROOM, MIRROR_ROOM_ORACLE);
            assert_oracle(db, FRACTURED_REALM, FRACTURED_REALM_ORACLE);
            assert_oracle(db, OPALESCENCE, OPALESCENCE_ORACLE);
            assert_oracle(db, GIDEON, GIDEON_ORACLE);
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, PREDATOR, PREDATOR_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, AUNTIE, AUNTIE_ORACLE);
            (
                scenario.add_real_card(P0, MIRROR_ROOM, Zone::Hand, db),
                scenario.add_real_card(P0, OPALESCENCE, Zone::Hand, db),
                scenario.add_real_card(P0, GIDEON, Zone::Hand, db),
                scenario.add_real_card(P0, PROWLER, Zone::Hand, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, MATRIARCH, Zone::Hand, db),
            )
        });
    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(room).modal_back_face(false).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    fund(&mut runner, ManaType::White, 4);
    runner.cast(opalescence).resolve();
    fund(&mut runner, ManaType::White, 4);
    runner.cast(gideon).resolve();
    settle(&mut runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: gideon,
            ability_index: 2,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Gideon's −4 must be legal");
    both_pass(&mut runner);
    settle(&mut runner);
    assert_settled(&runner);
    fund(&mut runner, ManaType::Green, 1);
    runner.cast(prowler).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].zone, Zone::Battlefield);
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(room).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    assert!(
        runner.state().objects[&prowler]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Room"),
        "reach-guard: Mirrorweave made the Prowler a copy of the Room"
    );
    cast_grub_as_auntie(&mut runner, grub);
    assert_eq!(
        runner.state().objects[&grub].name,
        AUNTIE,
        "CR 611.2c: Mirrorweave's affected set was fixed before Grub entered"
    );

    auntie_attacks(&mut runner, grub);
    blight(&mut runner, &[grub, room, prowler], prowler);
    assert_settled(&runner);
    let made = tokens(&runner);
    assert_eq!(made.len(), 1, "reach-guard: Grub's copy exists");
    let token = made[0];
    assert!(
        is_double_faced_permanent(&runner.state().objects[&token]),
        "CR 707.8a: a copy of a double-faced permanent is a double-faced token"
    );
    assert_eq!(
        runner.state().objects[&token]
            .double_faced_token_alternative
            .as_ref()
            .map(|half| half.name.as_str()),
        Some(FRACTURED_REALM),
        "CR 709.5b: the token keeps the copied Room's other half"
    );
    assert_eq!(
        runner.state().objects[&token].room_unlocks,
        Some(RoomUnlockState::default()),
        "CR 709.5d: both doors are locked"
    );
    assert_eq!(door_text(&runner, token), (1, 0, 0, 1));
    assert!(shown_sacrifices_itself(&runner, token));
    assert!(stored_sacrifices_itself(&runner, token));

    pass_into(&mut runner, Phase::End);
    assert_sacrifice_trigger_waiting(&runner, token);
    runner.advance_until_stack_empty();

    assert_settled(&runner);
    assert!(
        !runner.state().battlefield.contains(&token),
        "CR 707.9a + CR 709.5: the gained trigger functions with both doors locked"
    );
}

/// CR 709.4 + CR 709.5 + CR 709.5h: Sin, Spira's Punishment ("Whenever Sin enters
/// or attacks, exile a permanent card from your graveyard at random, then create
/// a tapped token that's a copy of that card. …") copies the Mirror Room //
/// Fractured Realm card in P0's graveyard. Off the battlefield that card has both
/// halves' characteristics, so the token has Mirror Room's trigger under the left
/// door and Fractured Realm's static under the right door. Unlocking Mirror Room
/// fires its trigger; unlocking Fractured Realm afterwards fires nothing.
#[test]
fn sins_copy_of_mirror_room_from_a_graveyard_has_each_doors_text_once() {
    let (mut runner, (room, sin, bears)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, MIRROR_ROOM, MIRROR_ROOM_ORACLE);
        assert_oracle(db, FRACTURED_REALM, FRACTURED_REALM_ORACLE);
        assert_oracle(db, SIN, SIN_ORACLE);
        (
            scenario.add_real_card(P0, MIRROR_ROOM, Zone::Graveyard, db),
            scenario.add_real_card(P0, SIN, Zone::Hand, db),
            scenario.add_real_card(P0, BEARS, Zone::Battlefield, db),
        )
    });
    assert_eq!(
        door_text(&runner, room),
        (0, 0, 0, 0),
        "reach-guard: the graveyard card's text is assigned to no door"
    );

    fund(&mut runner, ManaType::Black, 1);
    fund(&mut runner, ManaType::Green, 1);
    fund(&mut runner, ManaType::Blue, 5);
    runner.cast(sin).commit();
    both_pass(&mut runner);
    assert_eq!(runner.state().objects[&sin].zone, Zone::Battlefield);
    runner.advance_until_stack_empty();
    settle(&mut runner);
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&room].zone, Zone::Exile);
    let made = tokens(&runner);
    assert_eq!(made.len(), 1, "reach-guard: Sin copies the exiled card");
    let token = made[0];
    assert_eq!(
        runner.state().objects[&token].room_unlocks,
        Some(RoomUnlockState::default()),
        "CR 709.5d: the token enters with neither half unlocked"
    );
    assert_eq!(
        door_text(&runner, token),
        (1, 0, 0, 1),
        "CR 709.4 + CR 709.5: each half's text once, under its own door"
    );

    // Positive reach: Mirror Room's door fires its own trigger.
    fund(&mut runner, ManaType::Blue, 3);
    unlock(&mut runner, token, RoomDoor::Left);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "CR 709.5h: unlocking Mirror Room triggers it; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(bears)],
        })
        .expect("targeting the Bears must succeed");
    both_pass(&mut runner);
    settle(&mut runner);
    assert_settled(&runner);
    assert_eq!(tokens(&runner).len(), 2, "Mirror Room made its Reflection");

    // CR 709.5h: Fractured Realm's door has no trigger to fire.
    fund(&mut runner, ManaType::Blue, 7);
    unlock(&mut runner, token, RoomDoor::Right);
    assert_settled(&runner);
    assert_eq!(tokens(&runner).len(), 2, "no second Reflection");
}

/// CR 111.10i + CR 712.16 + CR 707.8a: Converter Beast's Incubator transforms,
/// Mirrorweave makes it a copy of Encouraging Aviator, and Cackling Counterpart
/// copies it. The token is double-faced, back face up, with Aviator's values on
/// its stored front face, which has no printed layout. After the game is saved
/// and reloaded, the stored face is the same and Cyber Conversion ("Turn target
/// creature face down. It's a 2/2 Cyberman artifact creature.") does nothing to
/// the token, while it turns the nontoken Encouraging Aviator face down.
#[test]
fn a_reloaded_copy_of_a_transformed_incubator_with_aviators_values_cannot_be_turned_face_down() {
    let (mut runner, (beast, aviator, weave, cackling, conversions)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, CONVERTER_BEAST, CONVERTER_BEAST_ORACLE);
            assert_oracle(db, AVIATOR, AVIATOR_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            assert_oracle(db, CYBER_CONVERSION, CYBER_CONVERSION_ORACLE);
            (
                scenario.add_real_card(P0, CONVERTER_BEAST, Zone::Hand, db),
                scenario.add_real_card(P0, AVIATOR, Zone::Battlefield, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
                [
                    scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
                    scenario.add_real_card(P0, CYBER_CONVERSION, Zone::Hand, db),
                ],
            )
        });
    fund(&mut runner, ManaType::Green, 4);
    runner.cast(beast).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&beast].zone, Zone::Battlefield);
    let incubator = tokens(&runner)[0];
    fund(&mut runner, ManaType::Green, 2);
    activate(&mut runner, incubator);
    both_pass(&mut runner);
    settle(&mut runner);
    assert_settled(&runner);
    assert!(
        runner.state().objects[&incubator].transformed,
        "reach-guard: the Incubator transformed"
    );

    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(aviator).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&incubator].name, AVIATOR);
    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(incubator).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    let token = tokens(&runner)
        .into_iter()
        .find(|id| *id != incubator)
        .expect("reach-guard: Cackling Counterpart copies the Incubator");
    let obj = &runner.state().objects[&token];
    assert!(obj.transformed, "CR 707.8a: the copy enters back face up");
    assert_eq!(
        (
            stored_face(&runner, token).name.as_str(),
            stored_face(&runner, token).layout_kind
        ),
        (AVIATOR, None),
        "reach-guard: the stored front face has Aviator's values and no layout"
    );
    assert_eq!(
        obj.double_faced_token_alternative
            .as_ref()
            .map(|face| (face.name.as_str(), face.layout_kind)),
        Some(("Jump", Some(LayoutKind::Prepare))),
        "CR 722.2b: the copied prepare spell is kept beside the other face"
    );
    assert!(is_double_faced_permanent(obj));
    let stored_before = stored_face(&runner, token).clone();

    let json = serde_json::to_string(runner.state()).expect("the game serializes");
    let mut loaded: GameState = serde_json::from_str(&json).expect("the game deserializes");
    engine::game::printed_cards::rehydrate_game_from_card_db(
        &mut loaded,
        load_db().expect("card db"),
    );
    let mut runner = GameRunner::from_state(loaded);
    assert_eq!(
        stored_face(&runner, token),
        &stored_before,
        "CR 111.1 + CR 707.2: the reloaded stored face is the serialized one"
    );

    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(conversions[0]).target_object(aviator).resolve();
    settle(&mut runner);
    assert_cyber_converted(&runner, aviator);
    runner.cast(conversions[1]).target_object(token).resolve();
    settle(&mut runner);
    assert_not_turned_face_down(&runner, token, AVIATOR, &stored_before);
    assert_settled(&runner);
}

// ===== CR 701.27g: a transformed permanent is a double-faced permanent back face up =====

/// Mutagen Connoisseur's settled power: "This creature gets +1/+0 for each
/// transformed permanent you control." on a 0/5.
fn connoisseur_power(runner: &mut GameRunner, connoisseur: ObjectId) -> i32 {
    settle(runner);
    runner.state().objects[&connoisseur]
        .power
        .expect("Mutagen Connoisseur has power")
}

/// CR 701.27g + CR 712.8d: the control for the modal rows. Valentin, Dean of the
/// Vein cast front face up is not a transformed permanent, and neither is the
/// Cackling Counterpart copy kept by the legend rule (CR 704.5j). Kessig Prowler
/// ("{4}{G}: Transform this creature."), turned afterward, is one, so Mutagen
/// Connoisseur's count fires on this board.
#[test]
fn a_modal_card_cast_front_face_up_is_not_a_transformed_permanent() {
    let (mut runner, (card, cackling, prowler, connoisseur)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, CONNOISSEUR, CONNOISSEUR_ORACLE);
            (
                scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, CONNOISSEUR, Zone::Battlefield, db),
            )
        });
    fund(&mut runner, ManaType::Black, 1);
    runner.cast(card).modal_back_face(false).commit();
    both_pass(&mut runner);
    assert_settled(&runner);
    settle(&mut runner);
    assert_eq!(runner.state().objects[&card].name, VALENTIN);
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        0,
        "CR 701.27g: Valentin, front face up, is not a transformed permanent"
    );

    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(card).commit();
    both_pass(&mut runner);
    settle(&mut runner);
    let token = tokens(&runner)[0];
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseLegend { .. }),
        "CR 704.5j: two Valentins ask for the legend rule; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseLegend { keep: token })
        .expect("keeping the token must succeed");
    assert!(runner.state().battlefield.contains(&token));
    let obj = &runner.state().objects[&token];
    assert!(
        !obj.transformed && !obj.modal_back_face && is_double_faced_permanent(obj),
        "reach guard: the kept token is a double-faced token front face up"
    );
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        0,
        "CR 701.27g: the front-face-up token copy is not a transformed permanent"
    );

    turn(&mut runner, prowler, PREDATOR);
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        1,
        "positive control: the transformed Kessig Prowler is counted"
    );
}

/// CR 701.27g + CR 712.8e + CR 707.8a: Kessig Prowler transformed into Sinuous
/// Predator is a transformed permanent, and Cackling Counterpart's copy of it is a
/// double-faced token back face up, so Mutagen Connoisseur counts both.
#[test]
fn a_transformed_card_and_its_token_copy_are_transformed_permanents() {
    let (mut runner, (prowler, cackling, connoisseur)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, CACKLING, CACKLING_ORACLE);
            assert_oracle(db, CONNOISSEUR, CONNOISSEUR_ORACLE);
            (
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
                scenario.add_real_card(P0, CONNOISSEUR, Zone::Battlefield, db),
            )
        });
    assert_eq!(connoisseur_power(&mut runner, connoisseur), 0);
    turn(&mut runner, prowler, PREDATOR);
    assert_eq!(connoisseur_power(&mut runner, connoisseur), 1);

    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(prowler).resolve();
    settle(&mut runner);
    let token = tokens(&runner)[0];
    assert!(
        runner.state().objects[&token].transformed,
        "reach guard: the token copy is back face up"
    );
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        2,
        "CR 701.27g + CR 707.8a: the card and its token copy are each counted"
    );
}

/// CR 709.2 + CR 709.3 + CR 701.27g: Mirror Room // Fractured Realm cast as
/// Fractured Realm records its right half on the battlefield, but "each split card
/// is only one card", never a double-faced permanent, so it is not a transformed
/// permanent. The transformed Kessig Prowler keeps Mutagen Connoisseur at 1, so
/// the Room's 0 is not vacuous.
#[test]
fn a_room_cast_as_its_right_half_is_not_a_transformed_permanent() {
    let (mut runner, (room, prowler, connoisseur)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, MIRROR_ROOM, MIRROR_ROOM_ORACLE);
        assert_oracle(db, FRACTURED_REALM, FRACTURED_REALM_ORACLE);
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        assert_oracle(db, CONNOISSEUR, CONNOISSEUR_ORACLE);
        (
            scenario.add_real_card(P0, MIRROR_ROOM, Zone::Hand, db),
            scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
            scenario.add_real_card(P0, CONNOISSEUR, Zone::Battlefield, db),
        )
    });
    turn(&mut runner, prowler, PREDATOR);
    assert_eq!(connoisseur_power(&mut runner, connoisseur), 1);

    fund(&mut runner, ManaType::Blue, 7);
    runner.cast(room).modal_back_face(true).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    let obj = &runner.state().objects[&room];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.modal_back_face, "reach guard: the right half was cast");
    assert_eq!(
        obj.room_unlocks,
        Some(RoomUnlockState {
            left_unlocked: false,
            right_unlocked: true,
        }),
        "CR 709.5d: the right half is unlocked"
    );
    assert!(
        !is_double_faced_permanent(obj),
        "CR 709.2: a Room is not a double-faced permanent"
    );
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        1,
        "CR 701.27g: the Room is not a transformed permanent"
    );
}

/// CR 701.27g + CR 730.2i: Gemrazer cast for its mutate cost under a transformed
/// Kessig Prowler makes a merged permanent. "A merged permanent is not a
/// double-faced permanent even if it contains one or more double-faced
/// components", so it is never a transformed permanent, though its top component
/// is still back face up.
#[test]
fn a_mutated_pile_is_never_a_transformed_permanent() {
    let (mut runner, (prowler, gemrazer, connoisseur)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, PROWLER, PROWLER_ORACLE);
            assert_oracle(db, GEMRAZER, GEMRAZER_ORACLE);
            assert_oracle(db, CONNOISSEUR, CONNOISSEUR_ORACLE);
            (
                scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db),
                scenario.add_real_card(P0, GEMRAZER, Zone::Hand, db),
                scenario.add_real_card(P0, CONNOISSEUR, Zone::Battlefield, db),
            )
        });
    turn(&mut runner, prowler, PREDATOR);
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        1,
        "reach guard: the transformed Prowler is counted before it mutates"
    );

    fund(&mut runner, ManaType::Green, 3);
    runner
        .cast(gemrazer)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_object(prowler)
        .commit();
    both_pass(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::MutateMergeChoice { .. }
        ),
        "CR 702.140c: the mutating spell merges with its target, over or under; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseMutateMergeSide {
            side: MergeSide::Bottom,
        })
        .expect("putting Gemrazer under must succeed");
    assert_settled(&runner);
    settle(&mut runner);

    let pile = &runner.state().objects[&prowler];
    assert_eq!(pile.merge_kind, Some(MergeKind::Mutate));
    assert_eq!(pile.merged_components.len(), 2, "reach guard: two cards");
    assert!(
        pile.transformed,
        "reach guard: the top component is still back face up"
    );
    assert_eq!(
        connoisseur_power(&mut runner, connoisseur),
        0,
        "CR 701.27g + CR 730.2i: a merged permanent is never a transformed permanent"
    );
}

/// Builds a real mutated Incubator through casting and the merge choice.
fn mutated_incubator<T>(
    side: MergeSide,
    stage: impl FnOnce(&mut GameScenario, &'static CardDatabase) -> T,
) -> (GameRunner, ObjectId, ObjectId, T) {
    let (mut runner, (beast, touch, gemrazer, staged)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, CONVERTER_BEAST, CONVERTER_BEAST_ORACLE);
            assert_oracle(db, KARNS_TOUCH, KARNS_TOUCH_ORACLE);
            assert_oracle(db, GEMRAZER, GEMRAZER_ORACLE);
            (
                scenario.add_real_card(P0, CONVERTER_BEAST, Zone::Hand, db),
                scenario.add_real_card(P0, KARNS_TOUCH, Zone::Hand, db),
                scenario.add_real_card(P0, GEMRAZER, Zone::Hand, db),
                stage(scenario, db),
            )
        });
    fund(&mut runner, ManaType::Green, 4);
    runner.cast(beast).resolve();
    settle(&mut runner);
    let incubator = tokens(&runner)[0];
    assert!(!runner.state().objects[&incubator].transformed);
    assert_eq!(
        (
            stored_face(&runner, incubator).name.as_str(),
            stored_face(&runner, incubator).layout_kind
        ),
        ("Phyrexian Token", None),
        "reach-guard: the front-face-up Incubator stores its Phyrexian face with no layout"
    );

    fund(&mut runner, ManaType::Blue, 2);
    runner.cast(touch).target_object(incubator).resolve();
    settle(&mut runner);
    assert!(
        runner.state().objects[&incubator]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach-guard: Karn's Touch made the Incubator a creature Gemrazer can mutate onto"
    );

    fund(&mut runner, ManaType::Green, 3);
    runner
        .cast(gemrazer)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_object(incubator)
        .commit();
    both_pass(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::MutateMergeChoice { .. }
        ),
        "CR 702.140c: the mutating spell merges with its target, over or under; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseMutateMergeSide { side })
        .expect("merging Gemrazer must succeed");
    settle(&mut runner);
    assert_settled(&runner);
    let pile = &runner.state().objects[&incubator];
    assert_eq!(pile.merge_kind, Some(MergeKind::Mutate));
    assert_eq!(
        pile.merged_components,
        match side {
            MergeSide::Top => vec![gemrazer, incubator],
            MergeSide::Bottom => vec![incubator, gemrazer],
        },
        "reach-guard: the chosen component is on top"
    );
    assert!(
        !pile.transformed,
        "reach-guard: the Incubator is front face up"
    );

    assert_eq!(
        pile.abilities
            .iter()
            .filter(|a| matches!(*a.effect, Effect::Transform { .. }))
            .count(),
        1,
        "reach guard: the Incubator supplies exactly one transform ability"
    );
    (runner, incubator, gemrazer, staged)
}

/// CR 730.2i + CR 111.10i: "If a merged permanent contains one or more double-faced
/// components that can transform …, transforming … that permanent causes each of those
/// double-faced components to turn so that its other face is up", and "An Incubator token is
/// a double-faced token". Converter Beast ("When this creature enters, incubate 5. …") makes
/// an Incubator, Karn's Touch ("Target noncreature artifact becomes an artifact creature with
/// power and toughness each equal to its mana value until end of turn. …") animates it, and
/// Gemrazer is cast for its mutate cost under it. The pile, with the front-face-up Incubator
/// on top, transforms through the Incubator's own "{2}: Transform this token."
#[test]
fn a_mutated_pile_with_an_incubator_on_top_transforms() {
    let (mut runner, incubator, _, ()) = mutated_incubator(MergeSide::Bottom, |_, _| ());
    assert_eq!(runner.state().objects[&incubator].name, "Incubator");
    let pile_ts = runner.state().objects[&incubator].timestamp;
    let effect_id = runner.state().objects[&incubator]
        .merge_layer_effect_id
        .expect("merge effect");
    let effect_ts = runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|effect| effect.id == effect_id)
        .expect("live merge effect")
        .timestamp;
    fund(&mut runner, ManaType::Colorless, 2);
    activate(&mut runner, incubator);
    assert!(
        matches!(
            runner.state().stack.last().map(|entry| &entry.kind),
            Some(StackEntryKind::ActivatedAbility { source_id, .. }) if *source_id == incubator
        ),
        "reach-guard: the Incubator's transform ability is on the stack; stack: {:?}",
        runner.state().stack
    );
    let mut replay = runner.state().clone();
    let journal_start = replay.resolved_rules_journal.entries().len();
    let mut events = Vec::new();
    for _ in 0..2 {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must succeed")
                .events,
        );
    }
    settle(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        transformed_events(&events, incubator),
        1,
        "CR 730.2i: the pile transforms; events: {events:?}"
    );
    let pile = &runner.state().objects[&incubator];
    assert!(
        pile.transformed,
        "CR 730.2i: the Incubator component is back face up"
    );
    assert_eq!(pile.merge_kind, Some(MergeKind::Mutate));
    // CR 613.7g + CR 730.2i + CR 730.2a: neither the pile nor its merge effect gets a new timestamp.
    assert_eq!(pile.timestamp, pile_ts);
    assert_eq!(pile.merge_layer_effect_id, Some(effect_id));
    assert_eq!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .expect("merge effect survives")
            .timestamp,
        effect_ts
    );
    // CR 730.2a + CR 702.140e + CR 111.10i: the top component has turned,
    // while Gemrazer still supplies its abilities.
    assert_eq!(pile.name, "Phyrexian Token");
    assert!(pile.card_types.core_types.contains(&CoreType::Artifact));
    assert!(pile.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!(pile.card_types.subtypes, vec!["Phyrexian"]);
    assert!(!pile
        .abilities
        .iter()
        .any(|a| matches!(*a.effect, Effect::Transform { .. })));
    assert!(pile.keywords.contains(&Keyword::Reach));
    assert!(pile.keywords.contains(&Keyword::Trample));
    assert!(pile
        .trigger_definitions
        .iter_all()
        .any(|e| e.definition.mode == TriggerMode::Mutates));

    let command = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .find_map(|command| match command {
            ResolvedRulesCommand::ObjectTransform(command)
                if command.object.object_id == incubator =>
            {
                Some(command)
            }
            _ => None,
        })
        .expect("reach guard: the transform is journaled");
    assert_eq!(command.resulting_timestamp, pile_ts);
    engine::game::transform::apply_resolved_transform(&mut replay, &command)
        .expect("the replay applies");
    evaluate_layers(&mut replay);
    assert_eq!(replay.objects[&incubator].name, "Phyrexian Token");
    assert_eq!(replay.objects[&incubator].timestamp, pile_ts);
    assert_eq!(
        replay.objects[&incubator].merge_layer_effect_id,
        Some(effect_id)
    );
    assert_eq!(
        replay
            .transient_continuous_effects
            .iter()
            .find(|effect| effect.id == effect_id)
            .expect("replayed merge effect")
            .timestamp,
        effect_ts
    );
}

/// CR 730.2i + CR 702.140e: a buried Incubator turns, and its front-face
/// transform ability is lost while Gemrazer remains the topmost component.
#[test]
fn a_buried_incubator_transforms_with_the_mutated_pile() {
    let (mut runner, incubator, gemrazer, ()) = mutated_incubator(MergeSide::Top, |_, _| ());
    assert_eq!(
        runner.state().objects[&incubator].merged_components,
        vec![gemrazer, incubator]
    );
    assert_eq!(
        (
            stored_face(&runner, incubator).name.as_str(),
            stored_face(&runner, incubator).layout_kind
        ),
        ("Phyrexian Token", None)
    );
    fund(&mut runner, ManaType::Colorless, 2);
    activate(&mut runner, incubator);
    top_of_stack_from(&runner, incubator);
    let mut events = Vec::new();
    for _ in 0..2 {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must succeed")
                .events,
        );
    }
    settle(&mut runner);
    assert_settled(&runner);
    assert_eq!(transformed_events(&events, incubator), 1);
    let pile = &runner.state().objects[&incubator];
    assert!(pile.transformed);
    assert_eq!(pile.name, "Gemrazer");
    assert!(!pile
        .abilities
        .iter()
        .any(|a| matches!(*a.effect, Effect::Transform { .. })));
    assert!(pile.keywords.contains(&Keyword::Reach));
    assert!(pile.keywords.contains(&Keyword::Trample));
}

/// Advances through public actions, observing every action before the next one.
fn pass_into_next_upkeep(
    runner: &mut GameRunner,
    mut observe: impl FnMut(&GameRunner, &[GameEvent]),
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..60 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::Upkeep {
            return events;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref other => panic!("unexpected prompt before the next upkeep: {other:?}"),
        };
        let result = runner
            .act(action)
            .expect("advancing to the next upkeep must succeed");
        observe(runner, &result.events);
        events.extend(result.events);
    }
    panic!("never reached P1's upkeep; at {:?}", runner.state().phase);
}

/// Resolves the stack through public actions, declining The Celestus's optional draw if its
/// day/night trigger resolves (filed follow-up: it does not fire today).
fn resolve_stack(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..12 {
        let action = match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: false }
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                GameAction::PassPriority
            }
            _ => return events,
        };
        events.extend(
            runner
                .act(action)
                .expect("resolving the stack must succeed")
                .events,
        );
    }
    panic!("the stack did not settle within twelve actions");
}

/// CR 613.7 + CR 730.2a: a later copy effect wins over the merge, but its
/// characteristics do not survive its own duration in the merge snapshot.
#[test]
fn a_later_copy_effect_applies_over_and_ends_apart_from_the_mutated_pile() {
    let (mut runner, incubator, _, (vanguard, weave, moonmist)) =
        mutated_incubator(MergeSide::Bottom, |scenario, db| {
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, MOONMIST, MOONMIST_ORACLE);
            let face = db
                .get_face_by_name(ELITE_VANGUARD)
                .expect("Elite Vanguard is in the fixture");
            assert!(face.oracle_text.is_none());
            assert_eq!(face.card_type.core_types, vec![CoreType::Creature]);
            assert_eq!(face.card_type.subtypes, vec!["Human", "Soldier"]);
            (
                scenario.add_real_card(P1, ELITE_VANGUARD, Zone::Battlefield, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, MOONMIST, Zone::Hand, db),
            )
        });
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(vanguard).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&incubator].name, ELITE_VANGUARD);
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moonmist).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    let pile = &runner.state().objects[&incubator];
    assert!(
        pile.transformed,
        "reach guard: Moonmist turned the Human copy"
    );
    assert_eq!(pile.name, ELITE_VANGUARD);
    assert!(!pile.keywords.contains(&Keyword::Reach));
    assert!(!pile.keywords.contains(&Keyword::Trample));
    assert!(!pile
        .trigger_definitions
        .iter_all()
        .any(|e| e.definition.mode == TriggerMode::Mutates));

    pass_into_next_upkeep(&mut runner, |_, _| {});
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    let pile = &runner.state().objects[&incubator];
    assert_eq!(pile.zone, Zone::Battlefield);
    // CR 111.10i + CR 702.140e: the turned Incubator and Gemrazer's abilities remain.
    assert_eq!(pile.name, "Phyrexian Token");
    assert!(pile.card_types.core_types.contains(&CoreType::Artifact));
    assert!(pile.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!(pile.card_types.subtypes, vec!["Phyrexian"]);
    assert!(pile.keywords.contains(&Keyword::Reach));
    assert!(pile.keywords.contains(&Keyword::Trample));
    assert!(pile
        .trigger_definitions
        .iter_all()
        .any(|e| e.definition.mode == TriggerMode::Mutates));
    assert!(!pile
        .abilities
        .iter()
        .any(|a| matches!(*a.effect, Effect::Transform { .. })));
}

/// Pays Moonrage Brute's ward through the public payment prompt and finishes the spell.
fn cast_paying_ward(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    runner.cast(spell).target_object(target).commit();
    for _ in 0..12 {
        match runner.state().waiting_for {
            WaitingFor::UnlessPayment { .. } => {
                runner
                    .act(GameAction::PayUnlessCost { pay: true })
                    .expect("pay ward");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::Priority { .. } => return,
            ref other => panic!("unexpected ward or spell prompt: {other:?}"),
        }
    }
    panic!("ward and spell did not finish within twelve actions");
}

/// CR 502.1 + CR 702.145f: phased-in nightbound permanents immediately agree with day.
/// CR 502.3 + CR 613.4c: Meekstone ("Creatures with power 3 or greater don't untap during
/// their controllers' untap steps.") reads Brutal Cathar's settled power, including counters.
/// Moonrage Brute has "Ward—Pay 3 life." CR 502.4 + CR 503.1a: the two Cathar transform
/// triggers wait until upkeep, before its active player receives priority.
#[test]
fn a_phased_out_nightbound_permanent_does_not_transform_until_it_phases_in() {
    let (
        mut runner,
        (celestus, marauder, cathar_a, cathar_b, slip, slip2, ripple, _meekstone, _bears),
    ) = board(Phase::PreCombatMain, |scenario, db| {
        for (name, oracle) in [
            (CELESTUS, CELESTUS_ORACLE),
            (SCOUNDREL, SCOUNDREL_ORACLE),
            (MARAUDER, MARAUDER_ORACLE),
            (SLIP, SLIP_ORACLE),
            (BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE),
            (MOONRAGE_BRUTE, MOONRAGE_BRUTE_ORACLE),
            (MEEKSTONE, MEEKSTONE_ORACLE),
            (REALITY_RIPPLE, REALITY_RIPPLE_ORACLE),
        ] {
            assert_oracle(db, name, oracle);
        }
        (
            scenario.add_real_card(P0, CELESTUS, Zone::Battlefield, db),
            scenario.add_real_card(P1, SCOUNDREL, Zone::Battlefield, db),
            scenario.add_real_card(P1, BRUTAL_CATHAR, Zone::Battlefield, db),
            scenario.add_real_card(P1, BRUTAL_CATHAR, Zone::Battlefield, db),
            scenario.add_real_card(P0, SLIP, Zone::Hand, db),
            scenario.add_real_card(P0, SLIP, Zone::Hand, db),
            scenario.add_real_card(P0, REALITY_RIPPLE, Zone::Hand, db),
            scenario.add_real_card(P0, MEEKSTONE, Zone::Battlefield, db),
            scenario.add_real_card(P0, BEARS, Zone::Battlefield, db),
        )
    });
    runner.state_mut().day_night = Some(DayNight::Night);
    for (id, name) in [
        (marauder, MARAUDER),
        (cathar_a, MOONRAGE_BRUTE),
        (cathar_b, MOONRAGE_BRUTE),
    ] {
        turn_by_designation(&mut runner, id, name);
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    fund(&mut runner, ManaType::Blue, 1);
    runner.cast(slip).target_object(marauder).resolve();
    settle(&mut runner);
    let life_before = runner.state().players[0].life;
    fund(&mut runner, ManaType::Blue, 2);
    cast_paying_ward(&mut runner, ripple, cathar_a);
    fund(&mut runner, ManaType::Blue, 1);
    cast_paying_ward(&mut runner, slip2, cathar_b);
    settle(&mut runner);
    assert_eq!(
        runner.state().players[0].life,
        life_before - 6,
        "both wards were paid"
    );
    assert_eq!(plus_counters(&runner, marauder), 1);
    assert_eq!(plus_counters(&runner, cathar_a), 0);
    assert_eq!(plus_counters(&runner, cathar_b), 1);
    for id in [marauder, cathar_a, cathar_b] {
        assert!(runner.state().objects[&id].is_phased_out());
    }
    activate_celestus(&mut runner, celestus);
    for (id, name) in [
        (marauder, MARAUDER),
        (cathar_a, MOONRAGE_BRUTE),
        (cathar_b, MOONRAGE_BRUTE),
    ] {
        let obj = &runner.state().objects[&id];
        assert!(obj.is_phased_out());
        assert!(obj.transformed);
        assert_eq!(obj.name, name);
    }
    let ids = [marauder, cathar_a, cathar_b];
    let mut phased_in = [false; 3];
    let events = pass_into_next_upkeep(&mut runner, |runner, events| {
        for (index, id) in ids.iter().enumerate() {
            phased_in[index] |= events.iter().any(
                |e| matches!(e, GameEvent::PermanentPhasedIn { object_id } if object_id == id),
            );
            if !phased_in[index] {
                assert!(
                    runner.state().objects[id].transformed,
                    "CR 702.26b: no checkpoint may turn the phased-out permanent"
                );
            }
        }
    });
    assert_eq!(runner.state().day_night, Some(DayNight::Day));
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    let upkeep_at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                GameEvent::PhaseChanged {
                    phase: Phase::Upkeep,
                    ..
                }
            )
        })
        .expect("entered upkeep");
    for id in ids {
        let obj = &runner.state().objects[&id];
        assert!(obj.is_phased_in());
        assert!(!obj.transformed);
        let phased_in_at = events
            .iter()
            .position(
                |e| matches!(e, GameEvent::PermanentPhasedIn { object_id } if *object_id == id),
            )
            .expect("phased in");
        let transformed_at = events
            .iter()
            .position(|e| matches!(e, GameEvent::Transformed { object_id } if *object_id == id))
            .expect("transformed");
        assert!(phased_in_at < transformed_at && transformed_at < upkeep_at);
        if id == cathar_a {
            assert_eq!(obj.name, BRUTAL_CATHAR);
            assert_eq!(obj.power, Some(2));
            assert!(!obj.tapped);
            let untapped_at = events
                .iter()
                .position(
                    |e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == id),
                )
                .expect("counter-free Cathar untaps");
            assert!(transformed_at < untapped_at && untapped_at < upkeep_at);
        } else {
            assert!(obj.tapped);
            assert!(!events.iter().any(
                |e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == id)
            ));
        }
    }
    assert_eq!(runner.state().objects[&marauder].name, SCOUNDREL);
    assert_eq!(runner.state().objects[&marauder].power, Some(5));
    assert_eq!(runner.state().objects[&cathar_b].name, BRUTAL_CATHAR);
    assert_eq!(runner.state().objects[&cathar_b].power, Some(3));
    let mut sources = order_prompt_sources(&runner, P1);
    sources.sort();
    let mut expected = vec![cathar_a, cathar_b];
    expected.sort();
    assert_eq!(sources, expected);
}

/// CR 502.1 + CR 502.2 + CR 731.2a: phasing comes first, then the day/night check. Reality
/// Ripple ("Target artifact, creature, or land phases out. …") phases out Moonrage Brute at
/// night; The Celestus ("{3}, {T}: If it's night, it becomes day. …") makes it day; P0, the
/// active player, casts no spells. In P1's untap step the Brute phases in by day and transforms
/// into Brutal Cathar (CR 702.145f), whose "Whenever this creature enters or transforms into
/// Brutal Cathar, exile target creature an opponent controls until this creature leaves the
/// battlefield." triggers (CR 603.10 + CR 701.27e); then it becomes night and the Cathar
/// transforms back (CR 702.145b). CR 502.4 + CR 503.1a: the exile waits for the upkeep.
#[test]
fn a_permanent_that_phases_in_by_day_transforms_before_it_becomes_night() {
    let (mut runner, (celestus, brute, bears, ripple)) =
        board(Phase::PreCombatMain, |scenario, db| {
            for (name, oracle) in [
                (CELESTUS, CELESTUS_ORACLE),
                (BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE),
                (MOONRAGE_BRUTE, MOONRAGE_BRUTE_ORACLE),
                (REALITY_RIPPLE, REALITY_RIPPLE_ORACLE),
            ] {
                assert_oracle(db, name, oracle);
            }
            (
                scenario.add_real_card(P0, CELESTUS, Zone::Battlefield, db),
                scenario.add_real_card(P1, BRUTAL_CATHAR, Zone::Battlefield, db),
                scenario.add_real_card(P0, BEARS, Zone::Battlefield, db),
                scenario.add_real_card(P1, REALITY_RIPPLE, Zone::Hand, db),
            )
        });
    runner.state_mut().day_night = Some(DayNight::Night);
    turn_by_designation(&mut runner, brute, MOONRAGE_BRUTE);

    // P0 activates The Celestus. P1 responds by phasing out its own Brute (no ward: P1
    // controls it), so P0 casts no spell this turn.
    fund(&mut runner, ManaType::Colorless, 3);
    runner
        .act(GameAction::ActivateAbility {
            source_id: celestus,
            ability_index: CELESTUS_DAY_NIGHT_ABILITY,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("The Celestus's day/night ability must be legal");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes with the ability on the stack");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    fund_player(&mut runner, P1, ManaType::Blue, 2);
    runner.cast(ripple).target_object(brute).commit();
    resolve_stack(&mut runner);

    let obj = &runner.state().objects[&brute];
    assert!(obj.is_phased_out() && obj.transformed);
    assert_eq!(obj.name, MOONRAGE_BRUTE);
    assert_eq!(runner.state().day_night, Some(DayNight::Day));
    assert_eq!(runner.state().objects[&ripple].zone, Zone::Graveyard);
    assert!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .is_none_or(|spells| spells.is_empty()),
        "the active player cast no spells this turn"
    );

    let events = pass_into_next_upkeep(&mut runner, |_, _| {});
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    let first =
        |pred: &dyn Fn(&GameEvent) -> bool| events.iter().position(pred).expect("event happened");
    let untap_at = first(&|e| {
        matches!(
            e,
            GameEvent::PhaseChanged {
                phase: Phase::Untap,
                ..
            }
        )
    });
    let phased_in_at =
        first(&|e| matches!(e, GameEvent::PermanentPhasedIn { object_id } if *object_id == brute));
    let transforms: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, GameEvent::Transformed { object_id } if *object_id == brute))
        .map(|(index, _)| index)
        .collect();
    let changes: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, GameEvent::DayNightChanged { .. }))
        .map(|(index, _)| index)
        .collect();
    let upkeep_at = first(&|e| {
        matches!(
            e,
            GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
                ..
            }
        )
    });
    assert_eq!(
        transforms.len(),
        2,
        "into Brutal Cathar by day, back at night"
    );
    assert_eq!(
        changes.len(),
        1,
        "the designation changes once, in the untap step"
    );
    assert!(matches!(
        &events[changes[0]],
        GameEvent::DayNightChanged { new_state } if new_state == "Night"
    ));
    assert!(
        untap_at < phased_in_at
            && phased_in_at < transforms[0]
            && transforms[0] < changes[0]
            && changes[0] < transforms[1]
            && transforms[1] < upkeep_at,
        "CR 502.1, then CR 502.2: {events:?}"
    );
    assert_eq!(runner.state().day_night, Some(DayNight::Night));
    let obj = &runner.state().objects[&brute];
    assert!(obj.is_phased_in() && obj.transformed);
    assert_eq!(obj.name, MOONRAGE_BRUTE);

    // CR 603.10 + CR 701.27e: Brutal Cathar's trigger was checked right after it transformed.
    let exile_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == brute
            )
        })
        .count();
    assert_eq!(exile_triggers, 1);
    resolve_stack(&mut runner);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    assert_eq!(runner.state().objects[&brute].name, MOONRAGE_BRUTE);
}

/// CR 502.2 + CR 731.2: the check belongs to the untap step. With day and P0 casting no spells,
/// it becomes night in P1's untap step, not in P0's cleanup. In an untap step added by Untap,
/// Upkeep, Draw ("After this phase, there is an additional untap step."), the check runs again
/// against the previous turn: after P1's The Celestus made it day, it becomes night again,
/// although P1 has cast a spell this turn.
#[test]
fn an_added_untap_step_checks_the_previous_turn_again() {
    let (mut runner, (celestus, untap_step)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, CELESTUS, CELESTUS_ORACLE);
        let celestus = scenario.add_real_card(P1, CELESTUS, Zone::Battlefield, db);
        let untap_step = scenario
            .add_spell_to_hand_from_oracle(P1, UNTAP_UPKEEP_DRAW, false, UNTAP_UPKEEP_DRAW_ORACLE)
            .from_oracle_text_with_keywords(&["Entwine"], UNTAP_UPKEEP_DRAW_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            })
            .id();
        (celestus, untap_step)
    });
    runner.state_mut().day_night = Some(DayNight::Day);

    let events = pass_into_next_upkeep(&mut runner, |_, _| {});
    let untap_at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                GameEvent::PhaseChanged {
                    phase: Phase::Untap,
                    ..
                }
            )
        })
        .expect("P1's untap step began");
    let changes: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, GameEvent::DayNightChanged { .. }))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(changes.len(), 1);
    assert!(
        untap_at < changes[0],
        "CR 502.2: not in P0's cleanup: {events:?}"
    );
    assert_eq!(runner.state().day_night, Some(DayNight::Night));

    for _ in 0..8 {
        if runner.state().phase == Phase::PreCombatMain {
            break;
        }
        resolve_stack(&mut runner);
        runner
            .act(GameAction::PassPriority)
            .expect("pass to P1's main phase");
    }
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    fund_player(&mut runner, P1, ManaType::Colorless, 3);
    runner
        .act(GameAction::ActivateAbility {
            source_id: celestus,
            ability_index: CELESTUS_DAY_NIGHT_ABILITY,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 activates The Celestus");
    resolve_stack(&mut runner);
    assert_eq!(runner.state().day_night, Some(DayNight::Day), "reach guard");
    fund_player(&mut runner, P1, ManaType::Blue, 2);
    runner.cast(untap_step).modes(&[0]).commit();
    resolve_stack(&mut runner);
    assert!(runner.state().stack.is_empty());

    let mut events = Vec::new();
    for _ in 0..8 {
        if runner.state().phase == Phase::BeginCombat {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).expect("pass").events);
    }
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_eq!(runner.state().active_player, P1);
    let at =
        |pred: &dyn Fn(&GameEvent) -> bool| events.iter().position(pred).expect("event happened");
    let added_untap_at = at(&|e| {
        matches!(
            e,
            GameEvent::PhaseChanged {
                phase: Phase::Untap,
                ..
            }
        )
    });
    let night_at =
        at(&|e| matches!(e, GameEvent::DayNightChanged { new_state } if new_state == "Night"));
    let untapped_at =
        at(&|e| matches!(e, GameEvent::PermanentUntapped { object_id } if *object_id == celestus));
    let combat_at = at(&|e| {
        matches!(
            e,
            GameEvent::PhaseChanged {
                phase: Phase::BeginCombat,
                ..
            }
        )
    });
    assert!(
        added_untap_at < night_at && night_at < combat_at,
        "{events:?}"
    );
    assert!(
        added_untap_at < untapped_at && untapped_at < combat_at,
        "reach guard: the added step's CR 502.3 untap ran"
    );
    assert_eq!(runner.state().day_night, Some(DayNight::Night));
}

/// CR 702.145b: Moonmist transforms a non-daybound Human DFC but cannot transform Brutal Cathar.
#[test]
fn a_daylight_moonmist_does_not_transform_a_daybound_permanent() {
    let (mut runner, (cathar, shepherd, bears, moonmist)) =
        board(Phase::PreCombatMain, |scenario, db| {
            for (name, oracle) in [
                (BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE),
                (MOONMIST, MOONMIST_ORACLE),
                (GATSTAF_SHEPHERD, GATSTAF_SHEPHERD_ORACLE),
                (GATSTAF_HOWLER, GATSTAF_HOWLER_ORACLE),
            ] {
                assert_oracle(db, name, oracle);
            }
            (
                scenario.add_real_card(P0, BRUTAL_CATHAR, Zone::Battlefield, db),
                scenario.add_real_card(P0, GATSTAF_SHEPHERD, Zone::Battlefield, db),
                scenario.add_real_card(P1, BEARS, Zone::Battlefield, db),
                scenario.add_real_card(P0, MOONMIST, Zone::Hand, db),
            )
        });
    runner.state_mut().day_night = Some(DayNight::Day);
    settle(&mut runner);
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moonmist).commit();
    let mut events = Vec::new();
    for _ in 0..2 {
        events.extend(runner.act(GameAction::PassPriority).expect("pass").events);
    }
    assert_eq!(transformed_events(&events, cathar), 0);
    assert!(!runner.state().objects[&cathar].transformed);
    assert_eq!(runner.state().objects[&cathar].name, BRUTAL_CATHAR);
    assert_eq!(transformed_events(&events, shepherd), 1);
    assert_eq!(runner.state().objects[&shepherd].name, GATSTAF_HOWLER);
    assert_eq!(runner.state().objects[&moonmist].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 702.145e: Waxing Moon grants trample but cannot transform a nightbound permanent.
#[test]
fn a_nightbound_permanent_ignores_waxing_moon() {
    let (mut runner, (brute, waxing)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE);
        assert_oracle(db, MOONRAGE_BRUTE, MOONRAGE_BRUTE_ORACLE);
        assert_oracle(db, WAXING_MOON, WAXING_MOON_ORACLE);
        (
            scenario.add_real_card(P0, BRUTAL_CATHAR, Zone::Battlefield, db),
            scenario.add_real_card(P0, WAXING_MOON, Zone::Hand, db),
        )
    });
    runner.state_mut().day_night = Some(DayNight::Night);
    turn_by_designation(&mut runner, brute, MOONRAGE_BRUTE);
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(waxing).target_object(brute).commit();
    let mut events = Vec::new();
    for _ in 0..2 {
        events.extend(runner.act(GameAction::PassPriority).expect("pass").events);
    }
    settle(&mut runner);
    assert_eq!(transformed_events(&events, brute), 0);
    let obj = &runner.state().objects[&brute];
    assert_eq!(obj.name, MOONRAGE_BRUTE);
    assert!(obj.transformed);
    assert!(
        obj.has_keyword(&Keyword::Trample),
        "reach guard: Waxing Moon resolved"
    );
    assert_eq!(runner.state().objects[&waxing].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(runner
        .act(GameAction::Transform { object_id: brute })
        .is_err());
}

// ===== CR 702.145b/e: daybound and nightbound read the face that is up =====

/// Casts Into the Night ("It becomes night. Discard any number of cards, then
/// draw that many cards plus one.") with an empty hand and resolves it.
fn cast_into_the_night(runner: &mut GameRunner, spell: ObjectId) {
    fund(runner, ManaType::Red, 4);
    runner.cast(spell).commit();
    both_pass(runner);
    assert_settled(runner);
    settle(runner);
    assert_eq!(runner.state().day_night, Some(DayNight::Night));
}

/// Activates The Celestus's "{3}, {T}: If it's night, it becomes day. Otherwise,
/// it becomes night. Activate only as a sorcery." and resolves it.
fn activate_celestus(runner: &mut GameRunner, celestus: ObjectId) {
    fund(runner, ManaType::Colorless, 3);
    runner
        .act(GameAction::ActivateAbility {
            source_id: celestus,
            ability_index: CELESTUS_DAY_NIGHT_ABILITY,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("The Celestus's day/night ability must be legal");
    both_pass(runner);
    assert!(
        !runner.state().stack.iter().any(|entry| matches!(
            entry.kind,
            StackEntryKind::ActivatedAbility { source_id, .. } if source_id == celestus
        )),
        "the day/night ability must have resolved; stack: {:?}",
        runner.state().stack
    );
    // The Celestus's own "Whenever day becomes night or night becomes day, you gain
    // 1 life. You may draw a card. If you do, discard a card." not resolving after
    // this activation is a filed follow-up. These rows don't depend on it either
    // way: the permanents turn over inside `resolve_set_day_night`, before any
    // trigger. If it fires, it resolves here and its draw is declined.
    for _ in 0..4 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => decide(runner, false),
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => both_pass(runner),
            _ => break,
        }
    }
    assert_settled(runner);
    settle(runner);
    assert_eq!(runner.state().day_night, Some(DayNight::Day));
}

/// The index of The Celestus's "{3}, {T}" ability in the fixture parse.
const CELESTUS_DAY_NIGHT_ABILITY: usize = 2;

/// CR 702.145b: "As it becomes night, if this permanent is front face up,
/// transform it." Valentin, Dean of the Vein cast as Lisette, Dean of the Root is
/// back face up. Mirrorweave ("Each other creature becomes a copy of target
/// nonlegendary creature until end of turn.") gives it Baneblade Scoundrel's
/// daybound, and Into the Night makes it night: the Scoundrel transforms, Lisette
/// does not.
#[test]
fn a_modal_card_back_face_up_with_daybound_does_not_transform_at_night() {
    let (mut runner, (scoundrel, card, weave, night)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, SCOUNDREL, SCOUNDREL_ORACLE);
            assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, INTO_THE_NIGHT, INTO_THE_NIGHT_ORACLE);
            (
                scenario.add_real_card(P0, SCOUNDREL, Zone::Battlefield, db),
                scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, INTO_THE_NIGHT, Zone::Hand, db),
            )
        });
    runner.state_mut().day_night = Some(DayNight::Day);
    cast_as_lisette(&mut runner, card);
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(scoundrel).resolve();
    settle(&mut runner);
    let obj = &runner.state().objects[&card];
    assert!(
        obj.keywords.contains(&Keyword::Daybound) && obj.modal_back_face && !obj.transformed,
        "reach guard: Lisette, back face up, is a daybound copy"
    );

    cast_into_the_night(&mut runner, night);
    let scoundrel_obj = &runner.state().objects[&scoundrel];
    assert!(
        scoundrel_obj.transformed,
        "positive control: the Scoundrel, front face up, transforms"
    );
    assert_eq!(stored_face(&runner, scoundrel).name, SCOUNDREL);
    let obj = &runner.state().objects[&card];
    assert!(
        !obj.transformed,
        "CR 702.145b: back face up, the Lisette copy does not transform"
    );
    assert_eq!(stored_face(&runner, card).name, VALENTIN);
}

/// CR 702.145b: Lisette, Dean of the Root Moonmisted into Valentin, Dean of the
/// Vein is front face up with both orientation flags set. Made a copy of Baneblade
/// Scoundrel by Mirrorweave, it is daybound and front face up, so it transforms
/// when Into the Night makes it night.
#[test]
fn a_front_face_up_daybound_copy_with_both_flags_transforms_at_night() {
    let (mut runner, (card, moonmist, scoundrel, weave, night)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
            assert_oracle(db, MOONMIST, MOONMIST_ORACLE);
            assert_oracle(db, SCOUNDREL, SCOUNDREL_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            assert_oracle(db, INTO_THE_NIGHT, INTO_THE_NIGHT_ORACLE);
            (
                scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
                scenario.add_real_card(P0, MOONMIST, Zone::Hand, db),
                scenario.add_real_card(P0, SCOUNDREL, Zone::Hand, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, INTO_THE_NIGHT, Zone::Hand, db),
            )
        });
    runner.state_mut().day_night = Some(DayNight::Day);
    cast_as_lisette(&mut runner, card);
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moonmist).resolve();
    settle(&mut runner);
    fund(&mut runner, ManaType::Black, 4);
    runner.cast(scoundrel).resolve();
    settle(&mut runner);
    assert!(!runner.state().objects[&scoundrel].transformed);
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(scoundrel).resolve();
    settle(&mut runner);
    let obj = &runner.state().objects[&card];
    assert!(
        obj.keywords.contains(&Keyword::Daybound) && obj.transformed && obj.modal_back_face,
        "reach guard: Valentin, front face up with both flags, is a daybound copy"
    );

    cast_into_the_night(&mut runner, night);
    assert!(
        runner.state().objects[&scoundrel].transformed,
        "positive control: the Scoundrel transforms"
    );
    assert_eq!(stored_face(&runner, scoundrel).name, SCOUNDREL);
    let obj = &runner.state().objects[&card];
    assert!(
        !obj.transformed && obj.modal_back_face,
        "CR 702.145b: front face up, the Valentin copy transforms to its back face"
    );
    assert_eq!(stored_face(&runner, card).name, VALENTIN);
}

/// CR 702.145e: "As it becomes day, if this permanent is back face up, transform
/// it." At night Baneblade Scoundrel enters as Baneclaw Marauder (CR 702.145b: "If
/// it is night … it enters transformed"). Valentin, Dean of the Vein cast as
/// Lisette, Dean of the Root is back face up; Mirrorweave makes it a copy of the
/// Marauder, so it is nightbound, and The Celestus makes it day: it transforms.
#[test]
fn a_modal_card_back_face_up_with_nightbound_transforms_at_day() {
    let (mut runner, (celestus, scoundrel, card, weave)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, CELESTUS, CELESTUS_ORACLE);
            assert_oracle(db, SCOUNDREL, SCOUNDREL_ORACLE);
            assert_oracle(db, MARAUDER, MARAUDER_ORACLE);
            assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            (
                scenario.add_real_card(P0, CELESTUS, Zone::Battlefield, db),
                scenario.add_real_card(P0, SCOUNDREL, Zone::Hand, db),
                scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
            )
        });
    runner.state_mut().day_night = Some(DayNight::Night);
    enter_scoundrel_at_night(&mut runner, scoundrel);
    cast_as_lisette(&mut runner, card);
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(scoundrel).resolve();
    settle(&mut runner);
    let obj = &runner.state().objects[&card];
    assert!(
        obj.keywords.contains(&Keyword::Nightbound) && obj.modal_back_face && !obj.transformed,
        "reach guard: Lisette, back face up, is a nightbound copy"
    );

    activate_celestus(&mut runner, celestus);
    assert_scoundrel_turned_back(&runner, scoundrel);
    let obj = &runner.state().objects[&card];
    assert!(
        obj.transformed && obj.modal_back_face,
        "CR 702.145e: back face up, the Lisette copy transforms to its front face"
    );
    assert_eq!(stored_face(&runner, card).name, LISETTE);
}

/// CR 702.145e: the nightbound sibling of the both-flags row. Lisette, Dean of the
/// Root Moonmisted into Valentin, Dean of the Vein is front face up; Mirrorweave
/// makes it a copy of Baneclaw Marauder, and when The Celestus makes it day it is
/// not back face up, so it does not transform.
#[test]
fn a_front_face_up_nightbound_copy_with_both_flags_does_not_transform_at_day() {
    let (mut runner, (celestus, card, moonmist, scoundrel, weave)) =
        board(Phase::PreCombatMain, |scenario, db| {
            assert_oracle(db, CELESTUS, CELESTUS_ORACLE);
            assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
            assert_oracle(db, MOONMIST, MOONMIST_ORACLE);
            assert_oracle(db, SCOUNDREL, SCOUNDREL_ORACLE);
            assert_oracle(db, MARAUDER, MARAUDER_ORACLE);
            assert_oracle(db, MIRRORWEAVE, MIRRORWEAVE_ORACLE);
            (
                scenario.add_real_card(P0, CELESTUS, Zone::Battlefield, db),
                scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
                scenario.add_real_card(P0, MOONMIST, Zone::Hand, db),
                scenario.add_real_card(P0, SCOUNDREL, Zone::Hand, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
            )
        });
    runner.state_mut().day_night = Some(DayNight::Night);
    cast_as_lisette(&mut runner, card);
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moonmist).resolve();
    settle(&mut runner);
    enter_scoundrel_at_night(&mut runner, scoundrel);
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(scoundrel).resolve();
    settle(&mut runner);
    let obj = &runner.state().objects[&card];
    assert!(
        obj.keywords.contains(&Keyword::Nightbound) && obj.transformed && obj.modal_back_face,
        "reach guard: Valentin, front face up with both flags, is a nightbound copy"
    );

    activate_celestus(&mut runner, celestus);
    assert_scoundrel_turned_back(&runner, scoundrel);
    let obj = &runner.state().objects[&card];
    assert!(
        obj.transformed && obj.modal_back_face,
        "CR 702.145e: front face up, the Valentin copy does not transform"
    );
    assert_eq!(stored_face(&runner, card).name, LISETTE);
}

/// Casts Baneblade Scoundrel at night. CR 702.145b: "If it is night and this
/// permanent is represented by a double-faced card, it enters transformed" — it is
/// Baneclaw Marauder, its name read before Mirrorweave can change it.
fn enter_scoundrel_at_night(runner: &mut GameRunner, scoundrel: ObjectId) {
    fund(runner, ManaType::Black, 4);
    runner.cast(scoundrel).resolve();
    settle(runner);
    let obj = &runner.state().objects[&scoundrel];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, MARAUDER, "CR 702.145b: it enters transformed");
    assert!(obj.transformed);
    assert_eq!(stored_face(runner, scoundrel).name, SCOUNDREL);
}

/// CR 702.145e: the Mirrorweave target, back face up and nightbound, transforms as
/// it becomes day. Its flag and stored face are read, never its displayed name: it
/// is Mirrorweave's own target.
fn assert_scoundrel_turned_back(runner: &GameRunner, scoundrel: ObjectId) {
    assert!(
        !runner.state().objects[&scoundrel].transformed,
        "positive control: the Scoundrel transforms back"
    );
    assert_eq!(stored_face(runner, scoundrel).name, MARAUDER);
}

/// A Feldon of the Third Path board: Feldon, able to activate, with `copied` the only creature
/// card in P0's graveyard and Into the Night in P0's hand during the day; `opponent` puts that
/// card on the battlefield under P1.
fn feldon_copy_board(
    copied: &'static str,
    copied_oracle: &'static str,
    opponent: Option<&'static str>,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let (mut runner, (feldon, night, opposing)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, FELDON, FELDON_ORACLE);
        assert_oracle(db, copied, copied_oracle);
        assert_oracle(db, INTO_THE_NIGHT, INTO_THE_NIGHT_ORACLE);
        scenario.add_real_card(P0, copied, Zone::Graveyard, db);
        (
            scenario.add_real_card(P0, FELDON, Zone::Battlefield, db),
            scenario.add_real_card(P0, INTO_THE_NIGHT, Zone::Hand, db),
            opponent.map(|name| scenario.add_real_card(P1, name, Zone::Battlefield, db)),
        )
    });
    runner
        .state_mut()
        .objects
        .get_mut(&feldon)
        .expect("Feldon exists")
        .summoning_sick = false;
    runner.state_mut().day_night = Some(DayNight::Day);
    (runner, feldon, night, opposing)
}

/// Activates Feldon of the Third Path targeting the only creature card in the graveyard and
/// resolves the ability, returning the events of both passes and the token it made. The token
/// is read from its creation event, not the battlefield, so a token that dies is still named.
fn feldon_makes_a_token(
    runner: &mut GameRunner,
    feldon: ObjectId,
) -> (Vec<engine::types::events::GameEvent>, ObjectId) {
    fund(runner, ManaType::Red, 3);
    activate(runner, feldon);
    top_of_stack_from(runner, feldon);
    let mut events = Vec::new();
    for _ in 0..2 {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must succeed")
                .events,
        );
    }
    let made: Vec<ObjectId> = events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(made.len(), 1, "reach guard: Feldon made exactly one token");
    (events, made[0])
}

fn transformed_events(events: &[engine::types::events::GameEvent], id: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, engine::types::events::GameEvent::Transformed { object_id } if *object_id == id)
        })
        .count()
}

fn entry_record_names(events: &[engine::types::events::GameEvent], id: ObjectId) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                record,
                ..
            } if *object_id == id => Some(record.name.clone()),
            _ => None,
        })
        .collect()
}

/// CR 702.145c: "Any time a player controls a permanent that is front face up with daybound and
/// it's night, that player transforms that permanent." Feldon of the Third Path ("{2}{R}, {T}:
/// Create a token that's a copy of target creature card in your graveyard, except it's an
/// artifact in addition to its other types. It gains haste. Sacrifice it at the beginning of the
/// next end step.") copies Baneblade Scoundrel at night. CR 111.1 + CR 702.145b: the token isn't
/// represented by a card, so it enters front face up (CR 707.8a) and then transforms into
/// Baneclaw Marauder, before the next designation change. CR 707.9b: the artifact exception is on
/// both faces. CR 712.18: the haste grant and the delayed sacrifice still apply to it.
#[test]
fn feldons_copy_of_a_daybound_card_made_at_night_transforms() {
    let (mut runner, feldon, night, _) = feldon_copy_board(SCOUNDREL, SCOUNDREL_ORACLE, None);
    assert_oracle(load_db().expect("card db"), MARAUDER, MARAUDER_ORACLE);
    cast_into_the_night(&mut runner, night);

    let (events, token) = feldon_makes_a_token(&mut runner, feldon);
    assert_settled(&runner);
    settle(&mut runner);
    assert_eq!(
        entry_record_names(&events, token),
        vec![SCOUNDREL.to_string()],
        "reach guard: the token entered front face up"
    );
    let obj = &runner.state().objects[&token];
    assert_eq!(
        obj.name, MARAUDER,
        "CR 702.145c: at night the daybound token transforms"
    );
    assert!(obj.transformed);
    assert_eq!(obj.transformation_count, 1);
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
    assert!(obj.keywords.contains(&Keyword::Nightbound));
    assert!(obj.keywords.contains(&Keyword::Haste));
    assert!(!obj.keywords.contains(&Keyword::Daybound));
    assert_eq!(stored_face(&runner, token).name, SCOUNDREL);
    assert_eq!(transformed_events(&events, token), 1);
    assert_eq!(runner.state().day_night, Some(DayNight::Night));

    drive_to_end(&mut runner);
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().battlefield.contains(&token),
        "Feldon's delayed trigger sacrifices it"
    );
}

/// CR 603.10 + CR 603.6a + CR 702.145c: Feldon of the Third Path copies Brutal Cathar
/// ("Whenever this creature enters or transforms into Brutal Cathar, exile target creature an
/// opponent controls until this creature leaves the battlefield.") at night. The token enters
/// as Brutal Cathar, so its enters trigger triggers; then it transforms into Moonrage Brute.
#[test]
fn feldons_copy_of_brutal_cathar_at_night_triggers_then_transforms() {
    let (mut runner, feldon, night, bears) =
        feldon_copy_board(BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE, Some(BEARS));
    assert_oracle(
        load_db().expect("card db"),
        MOONRAGE_BRUTE,
        MOONRAGE_BRUTE_ORACLE,
    );
    let bears = bears.expect("the Bears are staged");
    cast_into_the_night(&mut runner, night);

    let (events, token) = feldon_makes_a_token(&mut runner, feldon);
    assert_eq!(
        entry_record_names(&events, token),
        vec![BRUTAL_CATHAR.to_string()]
    );
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 1, "reach guard: the enters trigger waits");
    assert!(
        matches!(
            stack[0].kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == token
        ),
        "the trigger's source is the token; stack: {stack:?}"
    );
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, MOONRAGE_BRUTE, "CR 702.145c: then it transforms");
    assert!(obj.transformed);
    assert_eq!(obj.transformation_count, 1);

    both_pass(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Exile,
        "CR 603.10: the trigger of the face it entered with resolves"
    );
    assert_eq!(runner.state().objects[&token].name, MOONRAGE_BRUTE);
}

/// CR 702.145c: the daybound token transforms "immediately and isn't a state-based action", so
/// it is turned before the first state-based action check (CR 704.3). P1's Elesh Norn, Grand
/// Cenobite ("Creatures your opponents control get -2/-2.") would make Feldon of the Third
/// Path's copy of Brutal Cathar a 0/0, put into the graveyard under CR 704.5f; as Moonrage
/// Brute it is a 1/1 and survives. Its enters trigger, triggered as Brutal Cathar (CR 603.10),
/// then exiles the Norn, and the token is 3/3.
#[test]
fn feldons_copy_of_brutal_cathar_at_night_survives_an_opposing_minus_two() {
    let (mut runner, feldon, night, norn) =
        feldon_copy_board(BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE, Some(ELESH_NORN));
    let db = load_db().expect("card db");
    assert_oracle(db, MOONRAGE_BRUTE, MOONRAGE_BRUTE_ORACLE);
    assert_oracle(db, ELESH_NORN, ELESH_NORN_ORACLE);
    let norn = norn.expect("the Norn is staged");
    cast_into_the_night(&mut runner, night);

    let (events, token) = feldon_makes_a_token(&mut runner, feldon);
    assert_eq!(
        entry_record_names(&events, token),
        vec![BRUTAL_CATHAR.to_string()],
        "reach guard: the token entered as Brutal Cathar"
    );
    let departures: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to,
                record,
            } if *object_id == token => Some((record.name.clone(), record.toughness, *to)),
            _ => None,
        })
        .collect();
    assert!(
        runner.state().battlefield.contains(&token) && departures.is_empty(),
        "CR 702.145c: the token transforms before the state-based action check, so CR 704.5f \
         doesn't put it into the graveyard; departures: {departures:?}"
    );
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, MOONRAGE_BRUTE);
    assert_eq!(
        obj.toughness,
        Some(1),
        "the Norn's -2/-2 applies to Moonrage Brute"
    );
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 1, "the enters trigger waits");
    assert!(
        matches!(
            stack[0].kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == token
        ),
        "the trigger's source is the token; stack: {stack:?}"
    );

    both_pass(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&norn].zone,
        Zone::Exile,
        "the trigger exiles the Norn"
    );
    let obj = &runner.state().objects[&token];
    assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));
}

/// The departures from the battlefield of `id` in `events`: the record's name, its toughness and
/// the zone it went to.
fn departures(
    events: &[engine::types::events::GameEvent],
    id: ObjectId,
) -> Vec<(String, Option<i32>, Zone)> {
    events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to,
                record,
            } if *object_id == id => Some((record.name.clone(), record.toughness, *to)),
            _ => None,
        })
        .collect()
}

/// The number of Wolf tokens Cult of the Waxing Moon's trigger created in `events`.
fn wolves_created(events: &[engine::types::events::GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, engine::types::events::GameEvent::TokenCreated { name, .. } if name == "Wolf")
        })
        .count()
}

/// The sources of the `OrderTriggers` prompt, which must be open for `player`.
fn order_prompt_sources(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::OrderTriggers {
            player: prompted,
            triggers,
        } if *prompted == player => triggers.iter().map(|trigger| trigger.source_id).collect(),
        other => panic!("CR 603.3b: {player:?} orders their triggers; got {other:?}"),
    }
}

/// Answers the `OrderTriggers` prompt whose sources are `sources`, mapped by source: `first` is
/// placed first (the bottom of the group, so it resolves last), the rest in prompt order.
fn order_first(
    runner: &mut GameRunner,
    sources: &[ObjectId],
    first: ObjectId,
) -> Vec<engine::types::events::GameEvent> {
    let first_index = sources
        .iter()
        .position(|source| *source == first)
        .expect("the trigger to place first is in the prompt");
    let order: Vec<usize> = std::iter::once(first_index)
        .chain((0..sources.len()).filter(|index| *index != first_index))
        .collect();
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("ordering the triggers must succeed")
        .events
}

/// Every player passes priority once, in turn order, returning the events.
fn each_player_passes(
    runner: &mut GameRunner,
    players: usize,
) -> Vec<engine::types::events::GameEvent> {
    let mut events = Vec::new();
    for _ in 0..players {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "got {:?}",
            runner.state().waiting_for
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must succeed")
                .events,
        );
    }
    events
}

/// Passes priority until the stack is empty, returning the events.
fn pass_until_settled(runner: &mut GameRunner) -> Vec<engine::types::events::GameEvent> {
    let mut events = Vec::new();
    for _ in 0..24 {
        if runner.state().stack.is_empty() {
            assert_settled(runner);
            return events;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "got {:?}",
            runner.state().waiting_for
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must succeed")
                .events,
        );
    }
    panic!("the stack never emptied");
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// A Feldon of the Third Path board for Cult of the Waxing Moon during the day: P0 has Feldon,
/// able to activate, Cult of the Waxing Moon, Brutal Cathar the only creature card in the
/// graveyard and Into the Night in hand; P1 has Elesh Norn, Grand Cenobite. With `plague`, P0
/// also has Engineered Plague with Werewolf chosen; its effect is symmetric, and P0 controls it
/// because staging it under P1 leaves priority with P1. Returns Feldon, Into the Night, Cult and
/// the Norn.
fn feldon_cathar_cult_board(plague: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let (mut runner, (feldon, night, cult, norn, plague)) =
        board(Phase::PreCombatMain, |scenario, db| {
            for (name, oracle) in [
                (FELDON, FELDON_ORACLE),
                (CULT, CULT_ORACLE),
                (BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE),
                (MOONRAGE_BRUTE, MOONRAGE_BRUTE_ORACLE),
                (INTO_THE_NIGHT, INTO_THE_NIGHT_ORACLE),
                (ELESH_NORN, ELESH_NORN_ORACLE),
                (ENGINEERED_PLAGUE, ENGINEERED_PLAGUE_ORACLE),
            ] {
                assert_oracle(db, name, oracle);
            }
            scenario.add_real_card(P0, BRUTAL_CATHAR, Zone::Graveyard, db);
            (
                scenario.add_real_card(P0, FELDON, Zone::Battlefield, db),
                scenario.add_real_card(P0, INTO_THE_NIGHT, Zone::Hand, db),
                scenario.add_real_card(P0, CULT, Zone::Battlefield, db),
                scenario.add_real_card(P1, ELESH_NORN, Zone::Battlefield, db),
                plague
                    .then(|| scenario.add_real_card(P0, ENGINEERED_PLAGUE, Zone::Battlefield, db)),
            )
        });
    runner
        .state_mut()
        .objects
        .get_mut(&feldon)
        .expect("Feldon exists")
        .summoning_sick = false;
    if let Some(plague) = plague {
        runner
            .state_mut()
            .objects
            .get_mut(&plague)
            .expect("Engineered Plague exists")
            .chosen_attributes = vec![ChosenAttribute::CreatureType("Werewolf".to_string())];
    }
    runner.state_mut().day_night = Some(DayNight::Day);
    settle(&mut runner);
    let state = runner.state();
    assert_eq!(
        (
            state.objects[&feldon].power,
            state.objects[&feldon].toughness
        ),
        (Some(0), Some(1)),
        "reach guard: the Norn's -2/-2 applies to Feldon"
    );
    assert_eq!(
        (state.objects[&cult].power, state.objects[&cult].toughness),
        (Some(3), Some(2)),
        "reach guard: the Norn's -2/-2 applies to Cult"
    );
    (runner, feldon, night, cult, norn)
}

/// CR 603.10: "objects that exist immediately after an event are checked to see if the event
/// matched any trigger conditions". Cult of the Waxing Moon ("Whenever a permanent you control
/// transforms into a non-Human creature, create a 2/2 green Wolf creature token.") sees Feldon of
/// the Third Path's token copy of Brutal Cathar ("Whenever this creature enters or transforms into
/// Brutal Cathar, exile target creature an opponent controls until this creature leaves the
/// battlefield.") transform into Moonrage Brute at night (CR 702.145c), although the state-based
/// check that follows puts it into the graveyard: P1's Elesh Norn, Grand Cenobite ("Creatures
/// your opponents control get -2/-2.") and Engineered Plague ("All creatures of the chosen type
/// get -1/-1.", Werewolf) make Moonrage Brute 0/0 (CR 704.5f), and the token ceases to exist
/// (CR 111.7). The token's enters trigger, checked against the face it entered with, goes on the
/// stack too; it does nothing, because the token left before it resolved (CR 610.3b).
#[test]
fn cult_of_the_waxing_moon_sees_feldons_copy_of_brutal_cathar_transform_although_it_dies() {
    let (mut runner, feldon, night, cult, norn) = feldon_cathar_cult_board(true);
    cast_into_the_night(&mut runner, night);

    let (mut events, token) = feldon_makes_a_token(&mut runner, feldon);
    assert_eq!(
        entry_record_names(&events, token),
        vec![BRUTAL_CATHAR.to_string()],
        "reach guard: the token entered as Brutal Cathar"
    );
    assert_eq!(
        transformed_events(&events, token),
        1,
        "reach guard: CR 702.145c: the token transformed"
    );
    assert_eq!(
        departures(&events, token),
        vec![(MOONRAGE_BRUTE.to_string(), Some(0), Zone::Graveyard)],
        "reach guard: CR 704.5f: Moonrage Brute is 0/0 and dies"
    );
    assert!(!runner.state().battlefield.contains(&token));

    let sources = order_prompt_sources(&runner, P0);
    assert_eq!(
        sorted(sources.clone()),
        sorted(vec![cult, token]),
        "CR 603.10: Cult's trigger and the token's own, each once"
    );
    events.extend(order_first(&mut runner, &sources, token));
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 2, "stack: {stack:?}");
    assert_eq!(
        stack.iter().filter(|entry| entry.source_id == cult).count(),
        1
    );
    assert_eq!(
        stack
            .iter()
            .filter(|entry| entry.source_id == token)
            .count(),
        1
    );

    events.extend(pass_until_settled(&mut runner));
    assert_eq!(wolves_created(&events), 1, "Cult creates one Wolf");
    assert_eq!(
        runner.state().objects[&norn].zone,
        Zone::Battlefield,
        "CR 610.3b: the token left before its trigger resolved, so the Norn isn't exiled"
    );
}

/// CR 603.10: the surviving control of the row above. Without Engineered Plague, Feldon of the
/// Third Path's copy of Brutal Cathar is a 1/1 Moonrage Brute under Elesh Norn, Grand Cenobite,
/// and stays. Cult of the Waxing Moon sees the same transform and creates a Wolf, and the token's
/// enters trigger exiles the Norn.
#[test]
fn cult_of_the_waxing_moon_sees_feldons_copy_of_brutal_cathar_transform_and_it_survives() {
    let (mut runner, feldon, night, cult, norn) = feldon_cathar_cult_board(false);
    cast_into_the_night(&mut runner, night);

    let (mut events, token) = feldon_makes_a_token(&mut runner, feldon);
    assert_eq!(transformed_events(&events, token), 1);
    assert!(departures(&events, token).is_empty());
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, MOONRAGE_BRUTE);
    assert_eq!(obj.toughness, Some(1));

    let sources = order_prompt_sources(&runner, P0);
    assert_eq!(sorted(sources.clone()), sorted(vec![cult, token]));
    events.extend(order_first(&mut runner, &sources, token));
    assert_eq!(runner.state().stack.len(), 2);

    events.extend(pass_until_settled(&mut runner));
    assert_eq!(wolves_created(&events), 1, "Cult creates one Wolf");
    assert_eq!(
        runner.state().objects[&norn].zone,
        Zone::Exile,
        "the token's trigger resolved while it stayed"
    );
}

const P2: PlayerId = PlayerId(2);

/// A three-player Arcane Artisan board at night, P0 active: P0 has Arcane Artisan, able to
/// activate, and Grizzly Bears; P1 has Cult of the Waxing Moon; P2 has a stocked library. With
/// `p1_loses`, Brutal Cathar is P1's only card in hand and P1's library is empty; otherwise P1's
/// hand is empty and Brutal Cathar is the only card in P1's library. Returns Arcane Artisan, the
/// Bears and Cult.
fn artisan_cult_board(p1_loses: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let db = load_db().expect("card db");
    assert_oracle(db, ARCANE_ARTISAN, ARCANE_ARTISAN_ORACLE);
    assert_oracle(db, CULT, CULT_ORACLE);
    assert_oracle(db, BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE);
    let mut scenario = GameScenario::new_n_player(3, 769);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Top One", "Top Two", "Top Three"]);
    scenario.with_library_top(P2, &["Top One", "Top Two", "Top Three"]);
    let artisan = scenario.add_real_card(P0, ARCANE_ARTISAN, Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, BEARS, Zone::Battlefield, db);
    let cult = scenario.add_real_card(P1, CULT, Zone::Battlefield, db);
    let cathar_zone = if p1_loses { Zone::Hand } else { Zone::Library };
    let cathar = scenario.add_real_card(P1, BRUTAL_CATHAR, cathar_zone, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&artisan)
        .expect("Arcane Artisan exists")
        .summoning_sick = false;
    runner.state_mut().day_night = Some(DayNight::Night);
    settle(&mut runner);
    let p1 = &runner.state().players[1];
    if p1_loses {
        assert_eq!(p1.hand.iter().copied().collect::<Vec<_>>(), vec![cathar]);
        assert!(p1.library.is_empty(), "reach guard: P1's draw will fail");
    } else {
        assert!(p1.hand.is_empty());
        assert_eq!(p1.library.iter().copied().collect::<Vec<_>>(), vec![cathar]);
    }
    (runner, artisan, bears, cult)
}

/// Activates Arcane Artisan targeting P1 and every player passes once, so it resolves: P1 draws,
/// then exiles their only card in hand, Brutal Cathar, and creates a token copy of it, which at
/// night turns into Moonrage Brute (CR 702.145c). Returns the events and the token, read from its
/// creation event, which P1 controls.
fn artisan_makes_a_cathar_for_p1(
    runner: &mut GameRunner,
    artisan: ObjectId,
) -> (Vec<engine::types::events::GameEvent>, ObjectId) {
    fund(runner, ManaType::Blue, 3);
    activate(runner, artisan);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "Arcane Artisan targets a player; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .expect("targeting P1 must succeed");
    top_of_stack_from(runner, artisan);
    let events = each_player_passes(runner, 3);
    let made: Vec<ObjectId> = events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::TokenCreated {
                object_id, name, ..
            } if name == BRUTAL_CATHAR => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        made.len(),
        1,
        "reach guard: one token copy of Brutal Cathar"
    );
    let token = made[0];
    let controllers: Vec<PlayerId> = events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                record,
                ..
            } if *object_id == token => Some(record.controller),
            _ => None,
        })
        .collect();
    assert_eq!(controllers, vec![P1], "reach guard: P1 creates the token");
    assert_eq!(
        transformed_events(&events, token),
        1,
        "reach guard: CR 702.145c: the token transformed"
    );
    (events, token)
}

/// CR 800.4d: "If a triggered ability that would be controlled by a player who has left the game
/// would be put onto the stack, it isn't put on the stack." Arcane Artisan ("{2}{U}, {T}: Target
/// player draws a card, then exiles a card from their hand. If a creature card is exiled this way,
/// that player creates a token that's a copy of that card.") targets P1, whose library is empty
/// and whose only card in hand is Brutal Cathar ("Whenever this creature enters or transforms
/// into Brutal Cathar, exile target creature an opponent controls until this creature leaves the
/// battlefield."). At night the token turns into Moonrage Brute, and P1's Cult of the Waxing Moon
/// ("Whenever a permanent you control transforms into a non-Human creature, create a 2/2 green
/// Wolf creature token.") triggers (CR 603.10). The state-based check that follows has P1 lose
/// for drawing from an empty library (CR 704.5b), and P1 leaves this three-player game, which
/// goes on (CR 800.4a). Neither Cult's trigger nor the token's is put on the stack.
#[test]
fn cult_of_the_waxing_moon_trigger_is_not_put_on_the_stack_for_a_player_who_lost() {
    let (mut runner, artisan, bears, cult) = artisan_cult_board(true);
    let (events, token) = artisan_makes_a_cathar_for_p1(&mut runner, artisan);
    let lost: Vec<PlayerId> = events
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::PlayerLost { player_id } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(lost, vec![P1], "reach guard: CR 704.5b: P1 loses");
    let state = runner.state();
    assert!(state.players[1].is_eliminated);
    assert!(state.game_end.is_none(), "CR 800.4: the game goes on");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "got {:?}",
        state.waiting_for
    );

    assert!(
        !state
            .stack
            .iter()
            .any(|entry| entry.source_id == cult || entry.source_id == token),
        "CR 800.4d: P1's triggers aren't put on the stack; stack: {:?}",
        state.stack
    );
    assert!(state.stack.is_empty(), "stack: {:?}", state.stack);
    assert_eq!(wolves_created(&events), 0);
    assert_eq!(
        state.objects[&bears].zone,
        Zone::Battlefield,
        "CR 800.4d: the token's enters trigger isn't put on the stack either"
    );
}

/// The positive pair of the row above: P1 draws Brutal Cathar from a one-card library and
/// survives, so on the same route Cult of the Waxing Moon's trigger and the token's enters
/// trigger both go on the stack for P1 (CR 603.10, CR 702.145c, CR 603.3b). Cult creates a Wolf,
/// and the token's trigger exiles the Grizzly Bears it targets.
#[test]
fn cult_of_the_waxing_moon_trigger_on_arcane_artisans_copy_when_its_controller_survives() {
    let (mut runner, artisan, bears, cult) = artisan_cult_board(false);
    let (mut events, token) = artisan_makes_a_cathar_for_p1(&mut runner, artisan);

    let sources = order_prompt_sources(&runner, P1);
    assert_eq!(sorted(sources.clone()), sorted(vec![cult, token]));
    events.extend(order_first(&mut runner, &sources, token));
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            source_id,
            ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(*source_id, Some(token));
            assert_eq!(target_slots.len(), 1);
            let legal = &target_slots[0].legal_targets;
            assert_eq!(legal.len(), 2, "legal targets: {legal:?}");
            assert!(legal.contains(&TargetRef::Object(artisan)));
            assert!(legal.contains(&TargetRef::Object(bears)));
        }
        other => panic!("the token's trigger asks for its target; got {other:?}"),
    }
    events.extend(
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bears)],
            })
            .expect("targeting the Bears must succeed")
            .events,
    );

    events.extend(pass_until_settled(&mut runner));
    assert_eq!(wolves_created(&events), 1, "Cult creates one Wolf");
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&artisan].zone, Zone::Battlefield);
}

/// A Feldon of the Third Path board for Cult of the Waxing Moon at night: P0 has Feldon, able to
/// activate, Cult of the Waxing Moon, a Tovolar, Dire Overlord // Tovolar, the Midnight Scourge
/// card on the battlefield as Tovolar, the Midnight Scourge, and a second Tovolar card, the only
/// creature card in the graveyard. Returns Feldon, Cult and the Tovolar on the battlefield.
fn feldon_tovolar_cult_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (mut runner, (feldon, cult, tovolar)) = board(Phase::PreCombatMain, |scenario, db| {
        for (name, oracle) in [
            (FELDON, FELDON_ORACLE),
            (CULT, CULT_ORACLE),
            (TOVOLAR, TOVOLAR_ORACLE),
            (MIDNIGHT_SCOURGE, MIDNIGHT_SCOURGE_ORACLE),
        ] {
            assert_oracle(db, name, oracle);
        }
        scenario.add_real_card(P0, TOVOLAR, Zone::Graveyard, db);
        (
            scenario.add_real_card(P0, FELDON, Zone::Battlefield, db),
            scenario.add_real_card(P0, CULT, Zone::Battlefield, db),
            scenario.add_real_card(P0, TOVOLAR, Zone::Battlefield, db),
        )
    });
    runner
        .state_mut()
        .objects
        .get_mut(&feldon)
        .expect("Feldon exists")
        .summoning_sick = false;
    engine::game::transform::transform_permanent_with_cause(
        runner.state_mut(),
        tovolar,
        engine::game::transform::TransformCause::DayNightDesignation,
        &mut Vec::new(),
    )
    .expect("Tovolar must transform");
    runner.state_mut().day_night = Some(DayNight::Night);
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&tovolar].name,
        MIDNIGHT_SCOURGE,
        "reach guard: the staged Tovolar is back face up"
    );
    assert_settled(&runner);
    (runner, feldon, cult, tovolar)
}

/// CR 704.5j: "If two or more legendary permanents with the same name are controlled by the same
/// player, that player chooses one of them, and the rest are put into their owners' graveyards."
/// At night, Feldon of the Third Path ("{2}{R}, {T}: Create a token that's a copy of target
/// creature card in your graveyard, except it's an artifact in addition to its other types. It
/// gains haste. Sacrifice it at the beginning of the next end step.") copies a Tovolar, Dire
/// Overlord // Tovolar, the Midnight Scourge card while P0 controls Tovolar, the Midnight Scourge.
/// The token enters as Tovolar, Dire Overlord (CR 707.8a) and turns into Tovolar, the Midnight
/// Scourge (CR 702.145c), so Cult of the Waxing Moon ("Whenever a permanent you control
/// transforms into a non-Human creature, create a 2/2 green Wolf creature token.") triggers
/// (CR 603.10). The state-based check that follows asks P0 which Tovolar to keep, and Cult's
/// trigger waits until that choice is made (CR 704.3). P0 keeps the original; Cult creates one
/// Wolf.
#[test]
fn cult_of_the_waxing_moon_trigger_waits_behind_the_legend_rule_choice() {
    let (mut runner, feldon, cult, tovolar) = feldon_tovolar_cult_board();
    let (mut events, token) = feldon_makes_a_token(&mut runner, feldon);
    assert_eq!(
        entry_record_names(&events, token),
        vec![TOVOLAR.to_string()],
        "reach guard: the token entered as Tovolar, Dire Overlord"
    );
    assert_eq!(
        transformed_events(&events, token),
        1,
        "reach guard: CR 702.145c: the token transformed"
    );

    match &runner.state().waiting_for {
        WaitingFor::ChooseLegend {
            player,
            legend_name,
            candidates,
        } => {
            assert_eq!(*player, P0);
            assert_eq!(legend_name, MIDNIGHT_SCOURGE);
            assert_eq!(sorted(candidates.clone()), sorted(vec![tovolar, token]));
        }
        other => panic!("CR 704.5j: P0 chooses a Tovolar to keep; got {other:?}"),
    }
    assert!(
        runner.state().stack.is_empty(),
        "CR 704.3: no trigger is put on the stack before the choice; stack: {:?}",
        runner.state().stack
    );
    assert_eq!(wolves_created(&events), 0);

    events.extend(
        runner
            .act(GameAction::ChooseLegend { keep: tovolar })
            .expect("keeping the original Tovolar must succeed")
            .events,
    );
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 1, "stack: {stack:?}");
    assert_eq!(
        stack[0].source_id, cult,
        "CR 603.10: Cult's trigger goes on the stack"
    );
    assert!(!runner.state().battlefield.contains(&token));

    events.extend(pass_until_settled(&mut runner));
    assert_eq!(wolves_created(&events), 1, "Cult creates one Wolf");
    let kept = &runner.state().objects[&tovolar];
    assert_eq!(
        (kept.zone, kept.name.as_str()),
        (Zone::Battlefield, MIDNIGHT_SCOURGE)
    );
}

/// Cackling Counterpart ("Create a token that's a copy of target creature you control.") copies
/// Mirror Room // Fractured Realm, cast as Mirror Room and animated by Opalescence, during the
/// day; then Baneblade Scoundrel enters and Mirrorweave ("Each other creature becomes a copy of
/// target nonlegendary creature until end of turn.") makes the Room and the token copies of it.
/// Returns the Room, the token, the Scoundrel and Into the Night, still in hand.
fn single_faced_mirror_room_token() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let (mut runner, (room, opalescence, gideon, cackling, scoundrel, weave, night)) =
        board(Phase::PreCombatMain, |scenario, db| {
            for (name, oracle) in [
                (MIRROR_ROOM, MIRROR_ROOM_ORACLE),
                (FRACTURED_REALM, FRACTURED_REALM_ORACLE),
                (OPALESCENCE, OPALESCENCE_ORACLE),
                (GIDEON, GIDEON_ORACLE),
                (CACKLING, CACKLING_ORACLE),
                (SCOUNDREL, SCOUNDREL_ORACLE),
                (MIRRORWEAVE, MIRRORWEAVE_ORACLE),
                (INTO_THE_NIGHT, INTO_THE_NIGHT_ORACLE),
            ] {
                assert_oracle(db, name, oracle);
            }
            (
                scenario.add_real_card(P0, MIRROR_ROOM, Zone::Hand, db),
                scenario.add_real_card(P0, OPALESCENCE, Zone::Hand, db),
                scenario.add_real_card(P0, GIDEON, Zone::Hand, db),
                scenario.add_real_card(P0, CACKLING, Zone::Hand, db),
                scenario.add_real_card(P0, SCOUNDREL, Zone::Hand, db),
                scenario.add_real_card(P0, MIRRORWEAVE, Zone::Hand, db),
                scenario.add_real_card(P0, INTO_THE_NIGHT, Zone::Hand, db),
            )
        });
    runner.state_mut().day_night = Some(DayNight::Day);
    animate_mirror_room(&mut runner, room, opalescence, gideon);

    fund(&mut runner, ManaType::Blue, 3);
    runner.cast(cackling).target_object(room).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    let made = tokens(&runner);
    assert_eq!(made.len(), 1, "Cackling Counterpart makes one token");
    let token = made[0];
    let face = stored_face(&runner, token);
    assert_eq!(
        (face.name.as_str(), face.layout_kind),
        (FRACTURED_REALM, None),
        "reach guard: CR 709.5b: the token stores the Room's other half with no layout"
    );
    let obj = &runner.state().objects[&token];
    assert!(obj.double_faced_token_alternative.is_none());
    assert!(
        !is_double_faced_permanent(obj),
        "reach guard: the token isn't a double-faced token"
    );

    fund(&mut runner, ManaType::Black, 4);
    runner.cast(scoundrel).resolve();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&scoundrel].name, SCOUNDREL);
    fund(&mut runner, ManaType::Blue, 4);
    runner.cast(weave).target_object(scoundrel).resolve();
    settle(&mut runner);
    assert_settled(&runner);
    let obj = &runner.state().objects[&token];
    assert!(
        obj.has_keyword(&Keyword::Daybound),
        "reach guard: Mirrorweave gave the token daybound"
    );
    assert!(!obj.transformed && !obj.modal_back_face, "front face up");
    (runner, room, token, scoundrel, night)
}

/// CR 701.27a: "Only permanents represented by double-faced tokens and double-faced cards can
/// transform." CR 701.27c: "If a spell or ability instructs a player to transform a permanent that
/// isn't represented by a double-faced token or a double-faced card, nothing happens." Cackling
/// Counterpart's token copy of Mirror Room // Fractured Realm keeps the other half as a copiable
/// value (CR 709.5b) and is a single-faced token. Mirrorweave makes it a copy of Baneblade
/// Scoundrel ("Daybound"), so it is daybound and front face up when Into the Night makes it night,
/// but it doesn't transform. The real Baneblade Scoundrel transforms in the same designation
/// change (CR 702.145c), and the Mirror Room card, a split card that also became a copy of it, is
/// left alone.
#[test]
fn a_single_faced_room_token_with_daybound_does_not_transform_at_night() {
    let (mut runner, room, token, scoundrel, night) = single_faced_mirror_room_token();
    cast_into_the_night(&mut runner, night);
    assert_eq!(runner.state().day_night, Some(DayNight::Night));

    let obj = &runner.state().objects[&scoundrel];
    assert!(
        obj.transformed,
        "positive reach: CR 702.145c: the Scoundrel transforms"
    );
    assert_eq!(obj.transformation_count, 1);

    let obj = &runner.state().objects[&token];
    assert!(
        !obj.transformed,
        "CR 701.27c: the single-faced Room token doesn't transform"
    );
    assert_eq!(obj.transformation_count, 0);
    assert_eq!(stored_face(&runner, token).name, FRACTURED_REALM);

    let obj = &runner.state().objects[&room];
    assert!(!obj.transformed, "the Mirror Room card is left alone");
    assert_eq!(obj.transformation_count, 0);
}

/// CR 702.145c: during the day a daybound token front face up agrees, so Feldon of the Third
/// Path's copy of Baneblade Scoundrel stays Baneblade Scoundrel.
#[test]
fn feldons_copy_of_a_daybound_card_made_during_the_day_stays_front_face_up() {
    let (mut runner, feldon, _, _) = feldon_copy_board(SCOUNDREL, SCOUNDREL_ORACLE, None);

    let (events, token) = feldon_makes_a_token(&mut runner, feldon);
    assert_settled(&runner);
    settle(&mut runner);
    let obj = &runner.state().objects[&token];
    assert!(
        obj.keywords.contains(&Keyword::Daybound),
        "reach guard: the token is daybound"
    );
    assert_eq!(stored_face(&runner, token).name, MARAUDER);
    let obj = &runner.state().objects[&token];
    assert_eq!(obj.name, SCOUNDREL);
    assert!(!obj.transformed);
    assert_eq!(obj.transformation_count, 0);
    assert_eq!(transformed_events(&events, token), 0);
    assert_eq!(runner.state().day_night, Some(DayNight::Day));
}

/// CR 702.145b: "If it is night and this permanent is represented by a double-faced card, it
/// enters transformed." Brutal Cathar cast at night enters as Moonrage Brute, so the
/// agreement pass doesn't transform it again and the front face's enters trigger never
/// triggers: the opponent's Grizzly Bears stay.
#[test]
fn brutal_cathar_cast_at_night_enters_transformed_without_its_trigger() {
    let (mut runner, (cathar, night, bears)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, BRUTAL_CATHAR, BRUTAL_CATHAR_ORACLE);
        assert_oracle(db, MOONRAGE_BRUTE, MOONRAGE_BRUTE_ORACLE);
        assert_oracle(db, INTO_THE_NIGHT, INTO_THE_NIGHT_ORACLE);
        (
            scenario.add_real_card(P0, BRUTAL_CATHAR, Zone::Exile, db),
            scenario.add_real_card(P0, INTO_THE_NIGHT, Zone::Hand, db),
            scenario.add_real_card(P1, BEARS, Zone::Battlefield, db),
        )
    });
    runner.state_mut().day_night = Some(DayNight::Day);
    // Staged outside the hand, so Into the Night's "Discard any number of cards" has
    // nothing to ask about.
    cast_into_the_night(&mut runner, night);
    put(&mut runner, cathar, Zone::Hand);

    fund(&mut runner, ManaType::White, 3);
    runner.cast(cathar).commit();
    both_pass(&mut runner);
    assert_settled(&runner);
    settle(&mut runner);
    let obj = &runner.state().objects[&cathar];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.transformed, "reach guard: it entered transformed");
    assert_eq!(stored_face(&runner, cathar).name, BRUTAL_CATHAR);
    let obj = &runner.state().objects[&cathar];
    assert_eq!(obj.name, MOONRAGE_BRUTE);
    assert_eq!(obj.transformation_count, 0);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
}

/// Moves `card` from the battlefield to the graveyard and returns (raw `transformed`, latched
/// `transformed_permanent`, the latched source read, the departed connive subject's
/// `transformed`) for the departed incarnation.
fn depart_and_read(runner: &mut GameRunner, card: ObjectId) -> (bool, bool, bool, bool) {
    let identity = ObjectIncarnationRef {
        object_id: card,
        incarnation: runner.state().objects[&card].incarnation,
    };
    put(runner, card, Zone::Graveyard);
    let state = runner.state();
    let record = state
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == card && record.from_zone == Some(Zone::Battlefield))
        .expect("the departure is recorded");
    let context = record
        .trigger_source_context()
        .expect("the departure carries its source context");
    (
        context.transformed,
        context.transformed_permanent,
        context.source_read(state).transformed(),
        state
            .capture_departed_connive_subject(identity)
            .expect("the departed incarnation has a connive subject")
            .snapshot
            .transformed,
    )
}

/// CR 701.27g + CR 400.7: Valentin, Dean of the Vein cast as Lisette, Dean of the Root is a
/// transformed permanent, and the departed incarnation's latched answer says so; the raw
/// face-swap bit stays false for reconstruction. Shared-path witness: no printed card reads a
/// departed source's "transformed permanent"; the nearest printed reader is Mutagen Connoisseur
/// ("This creature gets +1/+0 for each transformed permanent you control.").
#[test]
fn a_departed_modal_card_back_face_up_was_a_transformed_permanent() {
    let (mut runner, card) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
        assert_oracle(db, LISETTE, LISETTE_ORACLE);
        scenario.add_real_card(P0, VALENTIN, Zone::Hand, db)
    });
    cast_as_lisette(&mut runner, card);
    let obj = &runner.state().objects[&card];
    assert!(
        obj.modal_back_face && !obj.transformed,
        "reach guard: Lisette, back face up"
    );

    assert_eq!(
        depart_and_read(&mut runner, card),
        (false, true, true, true),
        "CR 701.27g: the departed Lisette was a transformed permanent"
    );
}

/// CR 701.27g: "A permanent with its front face up is never considered a transformed
/// permanent, even if it had its back face up previously." Lisette, Dean of the Root
/// Moonmisted into Valentin, Dean of the Vein has both flags; its departure latches false.
#[test]
fn a_departed_front_face_up_modal_card_with_both_flags_was_not_a_transformed_permanent() {
    let (mut runner, (card, moonmist)) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, VALENTIN, VALENTIN_ORACLE);
        assert_oracle(db, MOONMIST, MOONMIST_ORACLE);
        (
            scenario.add_real_card(P0, VALENTIN, Zone::Hand, db),
            scenario.add_real_card(P0, MOONMIST, Zone::Hand, db),
        )
    });
    cast_as_lisette(&mut runner, card);
    fund(&mut runner, ManaType::Green, 2);
    runner.cast(moonmist).resolve();
    settle(&mut runner);
    let obj = &runner.state().objects[&card];
    assert_eq!(obj.name, VALENTIN, "reach guard: Moonmist turned it over");
    assert!(obj.modal_back_face && obj.transformed);

    assert_eq!(
        depart_and_read(&mut runner, card),
        (true, false, false, false),
        "CR 701.27g: the departed Valentin was not a transformed permanent"
    );
}

/// CR 701.27g: the positive control. A transformed Kessig Prowler ("{4}{G}: Transform this
/// creature.") departs as Sinuous Predator, a transformed permanent on every read.
#[test]
fn a_departed_transformed_card_was_a_transformed_permanent() {
    let (mut runner, prowler) = board(Phase::PreCombatMain, |scenario, db| {
        assert_oracle(db, PROWLER, PROWLER_ORACLE);
        assert_oracle(db, PREDATOR, PREDATOR_ORACLE);
        scenario.add_real_card(P0, PROWLER, Zone::Battlefield, db)
    });
    engine::game::transform::transform_permanent(runner.state_mut(), prowler, &mut Vec::new())
        .expect("Kessig Prowler transforms");
    settle(&mut runner);
    assert_eq!(runner.state().objects[&prowler].name, PREDATOR);

    assert_eq!(
        depart_and_read(&mut runner, prowler),
        (true, true, true, true)
    );
}
